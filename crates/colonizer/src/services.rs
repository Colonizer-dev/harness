//! Services that come back after a resume (issue #700): a suspension takes the microVM and every
//! process inside with it, so the host collects what a resumed colony should relaunch — the
//! repository's `.colonizer/services.toml` plus the records the previous run's writers left — into
//! a `restore` key on session.json, for the guest to relaunch, wait out, and report. Nothing secret
//! travels: a manifest's `env` holds names only (a table of values is refused unread), and every
//! command is scrubbed of the colony's own secret values before it is written.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Component, Path};

/// The session-dir directory the guest's writers record services in, mounted at [`GUEST_DIR`].
pub(crate) const DIR_NAME: &str = "services";
/// The in-VM path [`DIR_NAME`] is mounted at, handed to the runner through [`ENV_VAR`].
pub(crate) const GUEST_DIR: &str = "/colonizer/services";
/// The runner env var naming [`GUEST_DIR`], set on every boot so the writers always have it.
pub(crate) const ENV_VAR: &str = "COLONIZER_SERVICES_DIR";
/// The readiness wait a spec without `timeout_secs` gets — the default docs/colonies.md promises,
/// and the one the guest's relaunch applies, so the boot's health-wait extension must assume it too.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// One service to bring back — also the shape of the JSON files the guest writes into the services
/// directory, so manifest and records share this contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ServiceSpec {
    pub name: String,
    /// The shell command, run via `sh -c` in the worktree (or `cwd`).
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// A decimal TCP port or an http(s) URL the guest polls for readiness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<String>,
    /// Environment variable names passed through from the colony env. Names only, ever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// `false` marks a background task: reported lost on a resume, never relaunched.
    #[serde(default = "default_restart")]
    pub restart: bool,
    pub source: Source,
}

/// Where a spec was declared: the repository's manifest, or the guest's writers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Source {
    Manifest,
    Registered,
    Background,
}

fn default_restart() -> bool {
    true
}

/// One `[[service]]` table of the manifest, before validation. `ready` and `env` stay raw values so
/// a bad one can be refused by name ([`manifest_env`]) instead of failing the whole file's parse.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestService {
    name: String,
    cmd: String,
    cwd: Option<String>,
    ready: Option<toml::Value>,
    env: Option<toml::Value>,
    timeout_secs: Option<u64>,
}

/// The services the repository declares in `.colonizer/services.toml`. A file that will not parse,
/// or one entry that breaks the rules, yields no specs and one warning naming the reason — the
/// manifest is the repository's own file, and a broken one has never been a reason to refuse a boot.
pub(crate) fn manifest(worktree: &Path) -> (Vec<ServiceSpec>, Vec<String>) {
    let path = worktree.join(".colonizer/services.toml");
    let text = match std::fs::read_to_string(&path) {
        // No manifest is the normal shape: no specs, no warnings.
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), Vec::new()),
        Err(e) => return (Vec::new(), vec![format!("{} could not be read ({e})", path.display())]),
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ManifestFile {
        service: Vec<ManifestService>,
    }
    let parsed: ManifestFile = match toml::from_str(&text) {
        Ok(parsed) => parsed,
        Err(e) => {
            return (
                Vec::new(),
                vec![format!("{} could not be parsed ({e}); fix or remove it", path.display())],
            );
        }
    };
    let mut specs = Vec::new();
    let mut warnings = Vec::new();
    for entry in parsed.service {
        let name = entry.name.clone();
        let refused = |problem: String| format!("{}: service `{name}`: {problem}", path.display());
        // The manifest's `ready` travels as an integer port or an http(s) URL string; both
        // normalize to the string form the guest checks.
        let ready = match entry.ready {
            None => None,
            Some(toml::Value::Integer(port)) => Some(port.to_string()),
            Some(toml::Value::String(url)) => Some(url),
            Some(_) => {
                warnings.push(refused("ready must be a TCP port or an http(s) URL".into()));
                continue;
            }
        };
        let env = match manifest_env(entry.env) {
            Ok(env) => env,
            Err(problem) => {
                warnings.push(refused(problem));
                continue;
            }
        };
        // The manifest's `cwd` is worktree-relative by contract. This rule is for the repository's
        // own declaration only and is not re-checked on records ([`records`]): the guest wrote
        // those and resolves their `cwd` inside the VM it ran in, absolute paths included.
        if let Some(cwd) = &entry.cwd {
            let path = Path::new(cwd);
            if cwd.is_empty() || path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
                warnings.push(refused(
                    "cwd must be a directory relative to the worktree root, without `..`".into(),
                ));
                continue;
            }
        }
        let spec = ServiceSpec {
            ready,
            env,
            name: name.clone(),
            cmd: entry.cmd,
            cwd: entry.cwd,
            timeout_secs: entry.timeout_secs,
            restart: true,
            source: Source::Manifest,
        };
        match validate(&spec) {
            Ok(()) => specs.push(spec),
            Err(problem) => warnings.push(refused(problem)),
        }
    }
    (specs, warnings)
}

/// The manifest's `env` is a list of environment variable names. A table — the shape that would
/// carry values — is refused without reading the values: nothing secret is ever taken from the
/// manifest, and the message says where to put them instead.
fn manifest_env(env: Option<toml::Value>) -> Result<Option<Vec<String>>, String> {
    const NAMES: &str = "env must be a list of environment variable names";
    match env {
        None => Ok(None),
        Some(toml::Value::Array(names)) => names
            .iter()
            .map(|name| name.as_str().map(str::to_string).ok_or_else(|| NAMES.to_string()))
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(toml::Value::Table(_)) => Err(format!(
            "{NAMES}; values are never stored here — pass them through the colony env"
        )),
        Some(_) => Err(NAMES.into()),
    }
}

/// The services registered during the run, read from the session's services directory: one JSON
/// [`ServiceSpec`] per file. Invalid and oversized records are skipped with the reason as a
/// warning, so one bad file never costs the rest. Background records (`restart: false`) are
/// consumed — deleted once read — so the guest reports them lost exactly once; restartable records
/// stay, because the next resume relaunches them too.
pub(crate) fn records(dir: &Path) -> (Vec<ServiceSpec>, Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    let mut specs = Vec::new();
    let mut warnings = Vec::new();
    for path in paths {
        let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let failed = |reason: String| format!("service record {name:?}: {reason}");
        // The directory is guest-writable, so its entries are untrusted: read_regular_file refuses
        // a symlink outright (a planted one could name any host path), never blocks on a FIFO, and
        // bounds the read on the opened handle — no check-then-read window the guest could win.
        let text = match crate::github::read_regular_file(&path, MAX_RECORD_BYTES) {
            Ok(text) => text,
            Err(e) => {
                warnings.push(failed(format!("could not be read ({e})")));
                continue;
            }
        };
        let spec: ServiceSpec = match serde_json::from_str(&text) {
            Ok(spec) => spec,
            Err(e) => {
                warnings.push(failed(format!("could not be parsed ({e})")));
                continue;
            }
        };
        if let Err(problem) = validate(&spec) {
            warnings.push(failed(problem));
            continue;
        }
        // A background task is this resume's news only: delete it now, so it is reported lost
        // exactly once. A failed delete only means it is reported again — say so and move on.
        if !spec.restart
            && let Err(e) = std::fs::remove_file(&path)
        {
            warnings.push(failed(format!("could not be consumed ({e})")));
        }
        specs.push(spec);
    }
    (specs, warnings)
}

/// The largest service record the host will read, so a stray file cannot weigh a boot down.
const MAX_RECORD_BYTES: u64 = 16 * 1024;

/// Everything a resumed colony needs told about its services: the manifest's declarations first,
/// then the records the previous run left, each scrubbed of the colony's secret values before it
/// can reach session.json. A record naming a service the manifest already declares is skipped —
/// the manifest is the declaration of record. Warnings go to the session log; none of this fails
/// the boot.
pub(crate) fn resume_specs(worktree: &Path, dir: &Path, secrets: &[String]) -> (Vec<ServiceSpec>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut specs: Vec<ServiceSpec> = {
        let (declared, mut problems) = manifest(worktree);
        warnings.append(&mut problems);
        declared.into_iter().map(|spec| scrubbed(spec, secrets)).collect()
    };
    let mut names: HashSet<String> = specs.iter().map(|s| s.name.clone()).collect();
    let (registered, mut problems) = records(dir);
    warnings.append(&mut problems);
    for spec in registered {
        if names.insert(spec.name.clone()) {
            specs.push(scrubbed(spec, secrets));
        } else {
            warnings.push(format!(
                "service `{}`: already declared in .colonizer/services.toml; ignoring the registered record",
                spec.name
            ));
        }
    }
    (specs, warnings)
}

/// The `restore` key session.json gains on a resume boot: whether the colony was suspended when
/// its boot was claimed, and the services to bring back. Absent on other boots, which is how the
/// guest tells a restore from a fresh start.
pub(crate) fn restore_json(suspended: bool, specs: &[ServiceSpec]) -> Value {
    json!({ "suspended": suspended, "services": specs })
}

/// Replaces every colony secret value in a spec's command with deja's marker ([`crate::deja::scrub`]):
/// the command is the one field a manifest author or the agent could inline a value into.
fn scrubbed(spec: ServiceSpec, secrets: &[String]) -> ServiceSpec {
    if secrets.is_empty() {
        return spec;
    }
    ServiceSpec {
        cmd: crate::deja::scrub(&spec.cmd, secrets),
        ..spec
    }
}

/// The rules every spec answers to, wherever it was declared. Broken rules are warnings, never
/// boot failures — but a spec with an empty command, a name outside the character set, a `ready`
/// that is neither a port nor a URL, or an env entry that is not a name, does not travel.
fn validate(spec: &ServiceSpec) -> Result<(), String> {
    let name_ok = !spec.name.is_empty()
        && spec.name.len() <= 64
        && spec
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !name_ok {
        return Err("name must be 1-64 characters of letters, digits, dots, underscores or dashes".into());
    }
    if spec.cmd.trim().is_empty() {
        return Err("cmd must not be empty".into());
    }
    if let Some(ready) = &spec.ready {
        let port = ready.parse::<u16>().ok().filter(|port| *port > 0);
        if port.is_none() && !ready.starts_with("http://") && !ready.starts_with("https://") {
            return Err("ready must be a TCP port like 5173 or an http(s) URL".into());
        }
    }
    if let Some(names) = &spec.env
        && let Some(bad) = names.iter().find(|name| !is_env_name(name))
    {
        return Err(format!(
            "env must be a list of environment variable names; {bad:?} is not one"
        ));
    }
    Ok(())
}

/// An environment variable name: `[A-Za-z_][A-Za-z0-9_]*`, as the contract's pass-through promises.
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A throwaway directory (with a `.colonizer/` in it), cleaned up by the caller.
    fn dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-services-{tag}-{}", crate::util::short_id()));
        std::fs::create_dir_all(dir.join(".colonizer")).unwrap();
        dir
    }

    fn record(dir: &Path, name: &str, json: &str) {
        std::fs::write(dir.join(format!("{name}.json")), json).unwrap();
    }

    /// Parses a manifest from a throwaway worktree, cleaned up on the way out.
    fn manifest_with(tag: &str, toml: &str) -> (Vec<ServiceSpec>, Vec<String>) {
        let wt = dir(tag);
        std::fs::write(wt.join(".colonizer/services.toml"), toml).unwrap();
        let parsed = manifest(&wt);
        let _ = std::fs::remove_dir_all(wt);
        parsed
    }

    #[test]
    fn the_manifest_contract_holds_and_breaks_loudly() {
        let (specs, warnings) = manifest_with(
            "manifest",
            "[[service]]\nname = \"web\"\ncmd = \"npm run dev -- --port 5173\"\ncwd = \"web\"\nready = 5173\n\
             env = [\"VITE_API_URL\"]\ntimeout_secs = 30\n\n[[service]]\nname = \"docs\"\ncmd = \"mkdocs serve\"\n\
             ready = \"http://localhost:8000/\"\n",
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(specs[0].ready.as_deref(), Some("5173"), "int port normalizes to a string");
        assert_eq!(specs[0].env.as_deref(), Some(["VITE_API_URL".to_string()].as_slice()));
        assert_eq!(specs[0].timeout_secs, Some(30));
        assert!(specs[0].restart && specs[0].source == Source::Manifest, "manifest defaults");
        assert_eq!(specs[1].ready.as_deref(), Some("http://localhost:8000/"));

        // An entry that breaks the contract yields one warning naming the rule — and a refusal
        // carries the rule, never the values it refused.
        let secret = "sk_test_secretvalue42";
        let declared = |field: &str| format!("[[service]]\nname = \"a\"\ncmd = \"x\"\n{field}\n");
        for (field, rule) in [
            (
                format!("env = {{ STRIPE_KEY = \"{secret}\" }}"),
                "pass them through the colony env",
            ),
            ("fork = true".into(), "unknown field"),
            ("ready = true".into(), "ready must be"),
            ("ready = \"localhost:1\"".into(), "ready must be"),
            ("cwd = \"/etc\"".into(), "cwd must be"),
            ("cwd = \"../s\"".into(), "cwd must be"),
            ("env = [\"9BAD\"]".into(), "is not one"),
        ] {
            let (specs, warnings) = manifest_with("refused", &declared(&field));
            assert!(specs.is_empty(), "{field}: {specs:?}");
            assert_eq!(warnings.len(), 1, "{field}: {warnings:?}");
            assert!(warnings[0].contains(rule) && !warnings[0].contains(secret), "{warnings:?}");
        }
        for (toml, rule) in [
            ("[[service]]\nname = \"../e\"\ncmd = \"x\"\n", "name must be"),
            ("[[service]]\nname = \"a\"\ncmd = \" \"\n", "cmd must not"),
        ] {
            let (specs, warnings) = manifest_with("refused", toml);
            assert!(specs.is_empty() && warnings.len() == 1 && warnings[0].contains(rule));
        }
    }

    #[test]
    fn records_are_read_background_ones_consumed_once_and_bad_ones_skipped_with_a_warning() {
        let dir = dir("records");
        record(&dir, "web", r#"{"name":"web","cmd":"npm run dev","source":"registered"}"#);
        record(
            &dir,
            "bg",
            r#"{"name":"bg","cmd":"cargo test","restart":false,"source":"background"}"#,
        );
        record(&dir, "not-json", "this is not json");
        std::fs::write(dir.join("huge.json"), format!("\"{}\"", "x".repeat(17 * 1024))).unwrap();
        // Entries a guest could plant that are not regular files are never followed: the directory
        // is writable from inside the VM, so a symlink there could name any host path.
        std::os::unix::fs::symlink("/etc/passwd", dir.join("linked.json")).unwrap();
        std::fs::create_dir(dir.join("planted.json")).unwrap();
        // A record's `cwd` is the guest's own bookkeeping, so an absolute one is the guest's affair
        // — only the manifest's declaration is held to worktree-relative.
        record(
            &dir,
            "abs-cwd",
            r#"{"name":"abs-cwd","cmd":"make","cwd":"/tmp/build","source":"registered"}"#,
        );
        let (specs, warnings) = records(&dir);
        assert_eq!(
            specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["abs-cwd", "bg", "web"]
        );
        assert_eq!(specs[0].cwd.as_deref(), Some("/tmp/build"));
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("could not be parsed")));
        assert_eq!(
            warnings.iter().filter(|w| w.contains("could not be read")).count(),
            3,
            "{warnings:?}"
        );
        // The background record is gone after this one read; the restartable ones stay for next time.
        assert!(!dir.join("bg.json").exists() && dir.join("web.json").exists());
        assert_eq!(records(&dir).0.len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A guest-writable directory may hold a record that is really a symlink naming a host path, or
    /// a FIFO that would hang a plain open: neither is followed or read, and a background record's
    /// consume reaches only its own directory entry — never the file a link points at.
    #[test]
    fn a_symlinked_record_is_never_read_and_its_target_is_left_alone() {
        let dir = dir("symlink");
        let outside = std::env::temp_dir().join(format!("colonizer-services-outside-{}", crate::util::short_id()));
        std::fs::write(
            &outside,
            r#"{"name":"linked","cmd":"cat /etc/passwd","restart":false,"source":"background"}"#,
        )
        .unwrap();
        // Valid record content, one `..`-free hop outside the directory: exactly what a planted
        // symlink to a host secret would look like.
        std::os::unix::fs::symlink(&outside, dir.join("linked.json")).unwrap();
        // A FIFO with no writer would hang a read that opened it without O_NONBLOCK.
        let fifo = dir.join("fifo.json");
        let fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0, "mkfifo");
        let (specs, warnings) = records(&dir);
        assert!(specs.is_empty(), "{specs:?}");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(std::fs::symlink_metadata(dir.join("linked.json")).is_ok(), "the link stays");
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            r#"{"name":"linked","cmd":"cat /etc/passwd","restart":false,"source":"background"}"#,
            "the target is untouched"
        );
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(outside);
    }

    #[test]
    fn a_resume_takes_the_manifest_first_drops_duplicates_and_never_tells_a_secret() {
        let wt = dir("resume");
        let services = dir("resume-dir");
        let secret = "sk_test_0123456789abcdef";
        std::fs::write(
            wt.join(".colonizer/services.toml"),
            format!("[[service]]\nname = \"web\"\ncmd = \"STRIPE_KEY={secret} npm run dev\"\nenv = [\"STRIPE_KEY\"]\n"),
        )
        .unwrap();
        // `web` is declared twice — the manifest is the declaration of record; `api` only registers.
        let reg = |name: &str, cmd: &str| {
            record(
                &services,
                name,
                &format!(r#"{{"name":"{name}","cmd":"{cmd}","source":"registered"}}"#),
            )
        };
        reg("web", &format!("npx vite -- {secret}"));
        reg("api", &format!("API_KEY={secret} ./serve.sh"));
        let (specs, warnings) = resume_specs(&wt, &services, &[secret.to_string()]);
        assert_eq!(specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["web", "api"]);
        assert!(specs[0].source == Source::Manifest && specs[1].source == Source::Registered);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("already declared"));
        // Names travel; values do not — not into the warnings, not into the restore JSON.
        let restore = restore_json(true, &specs);
        assert!(!restore.to_string().contains(secret) && !warnings[0].contains(secret));
        assert!(restore.to_string().contains("[redacted]"));
        assert_eq!(specs[0].env.as_deref(), Some(["STRIPE_KEY".to_string()].as_slice()));
        assert_eq!(restore["suspended"], json!(true), "the key says the colony was suspended");
        let _ = std::fs::remove_dir_all(wt);
        let _ = std::fs::remove_dir_all(services);
    }
}
