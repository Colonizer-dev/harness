//! Colony secrets: keys the operator lets colonies use, such as a Stripe test key for a test suite.
//!
//! A colony never holds the value. Each secret names an environment variable and the hosts it is
//! for, and boots as a microsandbox `--secret ENV@hosts`: the guest sees a placeholder, and msb
//! swaps in the real value only on TLS connections to those hosts. The colony's prompt lists the
//! names and hosts it may use, never a value.
//!
//! The registry (`<config>/colony-secrets.json`) holds names, hosts and scope only. Values are
//! saved like every other secret, through `util::write_secret` at `<config>/colony-secrets/<ENV>`,
//! so they go to the system keychain when it is available and to a 0600 file otherwise.
//!
//! Hosts must be public DNS names. A colony runs with the `public` network profile, which already
//! reaches the public internet, so no extra network rule is needed; private, loopback and
//! link-local destinations stay behind the fence, and naming one here is refused rather than
//! silently unreachable.

use crate::{ApiResult, Shared, client_error, sandbox::Secret, util};
use anyhow::{Result, anyhow};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};

/// Hosts one colony secret may name.
const MAX_HOSTS: usize = 10;

/// Names a colony secret may not take: the harness's own credentials and variables the guest
/// runtime depends on.
const RESERVED: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "PWD",
    "LANG",
    "TERM",
    "TMPDIR",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "TYPESAFE_API_KEY",
    "JEV_API_KEY",
    "MEM0_API_KEY",
];

/// Prefixes a colony secret may not start with, for the same reason.
const RESERVED_PREFIXES: &[&str] = &["ANTHROPIC_", "CLAUDE_", "COLONIZER_", "OPENAI_", "NODE_", "MSB_", "LD_"];

/// Which colonies may use a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    /// Every colony on this mothership.
    All,
    /// Colonies on repositories of one GitHub org or user.
    Org { org: String },
    /// Colonies on one repository, `owner/name`.
    Repo { repo: String },
}

impl Scope {
    /// Whether a colony on `repo` (`owner/name`) may use the secret. Names compare case-insensitively,
    /// as GitHub treats them.
    pub fn admits(&self, repo: &str) -> bool {
        let owner = repo.split('/').next().unwrap_or_default();
        match self {
            Scope::All => true,
            Scope::Org { org } => owner.eq_ignore_ascii_case(org),
            Scope::Repo { repo: want } => repo.eq_ignore_ascii_case(want),
        }
    }
}

/// One colony secret's metadata. The value is never here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColonySecret {
    pub env: String,
    pub hosts: Vec<String>,
    pub scope: Scope,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl ColonySecret {
    /// The secret's id on the Secrets page and in the API.
    pub fn id(&self) -> String {
        format!("colony:{}", self.env)
    }
}

fn registry_file(config_dir: &FsPath) -> PathBuf {
    config_dir.join("colony-secrets.json")
}

/// Where the value is saved (through the secret store, so usually the keychain).
pub fn value_path(config_dir: &FsPath, env: &str) -> PathBuf {
    config_dir.join("colony-secrets").join(env)
}

/// The registry, sorted by name. A missing file is an empty registry; a damaged one is an error,
/// so a save never overwrites it with an empty list.
pub fn load(config_dir: &FsPath) -> Result<Vec<ColonySecret>> {
    let path = registry_file(config_dir);
    let mut list: Vec<ColonySecret> = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| anyhow!("{} could not be parsed ({e}); fix or remove it", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(anyhow!("{} could not be read ({e})", path.display())),
    };
    list.sort_by(|a, b| a.env.cmp(&b.env));
    Ok(list)
}

fn save(config_dir: &FsPath, list: &[ColonySecret]) -> Result<()> {
    util::write_private(&registry_file(config_dir), &serde_json::to_vec_pretty(list)?)
}

/// Checks an environment variable name: `[A-Z_][A-Z0-9_]*`, at most 64 characters, and not one the
/// harness or the guest runtime owns.
pub fn validate_env(env: &str) -> Result<(), String> {
    let mut chars = env.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_uppercase() || c == '_');
    if !first_ok || !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') || env.len() > 64 {
        return Err(format!(
            "{env:?} is not a valid name: use capital letters, digits and underscores, starting with a letter or underscore"
        ));
    }
    if RESERVED.contains(&env) || RESERVED_PREFIXES.iter().any(|p| env.starts_with(p)) {
        return Err(format!("{env} is reserved for the harness; pick another name"));
    }
    Ok(())
}

/// Checks and normalises the hosts: one or more public DNS names, lower-cased, deduplicated. No
/// wildcards, IP literals, ports, schemes or internal names.
pub fn validate_hosts(hosts: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for raw in hosts {
        let host = raw.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            continue;
        }
        let bad = |why: &str| Err(format!("{raw:?} is not an allowed host: {why}"));
        if host.contains('*') {
            return bad("wildcards are not allowed; name each host");
        }
        if host.contains("://") || host.contains('/') || host.contains(':') {
            return bad("give a bare host name, without a scheme, path or port");
        }
        if host.parse::<std::net::IpAddr>().is_ok() || host.split('.').all(|l| l.chars().all(|c| c.is_ascii_digit())) {
            return bad("IP addresses are not allowed; name the host");
        }
        if host == "localhost"
            || [".localhost", ".local", ".internal", ".lan", ".home", ".arpa"]
                .iter()
                .any(|s| host.ends_with(s))
        {
            return bad("colonies only reach public hosts");
        }
        let labels_ok = host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
        if !host.contains('.') || !labels_ok || host.len() > 253 {
            return bad("it must be a public DNS name such as api.stripe.com");
        }
        if !out.contains(&host) {
            out.push(host);
        }
    }
    if out.is_empty() {
        return Err("name at least one host the secret is for, such as api.stripe.com".into());
    }
    if out.len() > MAX_HOSTS {
        return Err(format!("at most {MAX_HOSTS} hosts per secret"));
    }
    Ok(out)
}

fn validate_scope(scope: Scope) -> Result<Scope, String> {
    let name_ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    match scope {
        Scope::All => Ok(Scope::All),
        Scope::Org { org } => {
            let org = org.trim().to_string();
            if name_ok(&org) {
                Ok(Scope::Org { org })
            } else {
                Err(format!("{org:?} is not an org name"))
            }
        }
        Scope::Repo { repo } => {
            let repo = repo.trim().to_string();
            match repo.split_once('/') {
                Some((owner, name)) if name_ok(owner) && name_ok(name) => Ok(Scope::Repo { repo }),
                _ => Err(format!("{repo:?} is not a repository; use owner/name")),
            }
        }
    }
}

/// The secrets a colony on `repo` may use: those in scope, as `(metadata, value)`. A registered
/// secret with no saved value is left out.
pub fn for_colony(config_dir: &FsPath, repo: &str) -> Vec<(ColonySecret, String)> {
    load(config_dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.scope.admits(repo))
        .filter_map(|s| {
            let value = util::read_secret(&value_path(config_dir, &s.env))?;
            Some((s, value))
        })
        .collect()
}

/// The boot-spec entries: msb substitutes each value on TLS to its hosts only.
pub fn boot_secrets(granted: &[(ColonySecret, String)]) -> Vec<Secret> {
    granted
        .iter()
        .map(|(s, value)| Secret {
            env: s.env.clone(),
            value: value.clone(),
            hosts: s.hosts.clone(),
        })
        .collect()
}

/// The block appended to a colony's prompt: names and hosts only, never a value. Empty when the
/// colony has no colony secrets.
pub fn prompt_block(granted: &[&ColonySecret]) -> String {
    if granted.is_empty() {
        return String::new();
    }
    let mut p = String::from("\n<colony-secrets>\nThese secrets are available to you as environment variables:\n");
    for s in granted {
        p.push_str(&format!("- `{}`, for {} only\n", s.env, s.hosts.join(", ")));
    }
    p.push_str(
        "The value is substituted on the wire: inside this VM the variable holds a placeholder, and the real \
         value is sent only on TLS requests to the hosts listed. Use the variable where a request to those hosts \
         needs it. Do not print, log, commit or persist it, and do not send it anywhere else.\n</colony-secrets>\n",
    );
    p
}

// ---------------------------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct UpsertColonySecret {
    env: String,
    hosts: Vec<String>,
    scope: Scope,
    /// Required for a new secret; omitted keeps the saved value when only hosts or scope change.
    #[serde(default)]
    value: Option<String>,
}

/// `POST /api/secrets/colony`: adds a colony secret or changes its hosts, scope or value.
pub async fn upsert(State(app): State<Shared>, Json(body): Json<UpsertColonySecret>) -> ApiResult<Value> {
    let bad = |m: String| client_error(StatusCode::BAD_REQUEST, &m);
    let env = body.env.trim().to_string();
    validate_env(&env).map_err(bad)?;
    let hosts = validate_hosts(&body.hosts).map_err(bad)?;
    let scope = validate_scope(body.scope).map_err(bad)?;
    let value = body.value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    if let Some(v) = &value
        && (v.len() > 16 * 1024 || v.contains(['\n', '\r']))
    {
        return Err(bad("a secret is one line of at most 16 KB".into()));
    }
    let dir = app.cfg.config_dir.clone();
    let saved = tokio::task::spawn_blocking(move || -> Result<ColonySecret, (StatusCode, String)> {
        let internal = |e: anyhow::Error| (StatusCode::CONFLICT, format!("{e:#}"));
        let mut list = load(&dir).map_err(internal)?;
        let path = value_path(&dir, &env);
        let exists = list.iter().any(|s| s.env == env);
        if !exists && value.is_none() {
            return Err((StatusCode::BAD_REQUEST, "a new colony secret needs a value".into()));
        }
        if let Some(v) = &value {
            util::write_secret(&path, v).map_err(|e| (StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
        }
        let entry = ColonySecret {
            env: env.clone(),
            hosts,
            scope,
            updated_at: Some(Utc::now()),
        };
        list.retain(|s| s.env != env);
        list.push(entry.clone());
        list.sort_by(|a, b| a.env.cmp(&b.env));
        save(&dir, &list).map_err(internal)?;
        Ok(entry)
    })
    .await
    .map_err(|e| anyhow!(e))?;
    let entry = saved.map_err(|(code, m)| client_error(code, &m))?;
    Ok(Json(
        json!({ "id": entry.id(), "env": entry.env, "hosts": entry.hosts, "scope": entry.scope }),
    ))
}

/// Drops a colony secret's registry entry; the caller removes its value.
pub fn remove(config_dir: &FsPath, env: &str) -> Result<()> {
    let mut list = load(config_dir)?;
    let before = list.len();
    list.retain(|s| s.env != env);
    if list.len() != before {
        save(config_dir, &list)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(env: &str, hosts: &[&str], scope: Scope) -> ColonySecret {
        ColonySecret {
            env: env.into(),
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
            scope,
            updated_at: None,
        }
    }

    #[test]
    fn names_are_shell_variables_and_never_the_harnesss_own() {
        for ok in ["STRIPE_TEST_KEY", "_X", "A1"] {
            assert!(validate_env(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "stripe",
            "1A",
            "A-B",
            "A B",
            "PATH",
            "GITHUB_TOKEN",
            "ANTHROPIC_API_KEY",
            "CLAUDE_X",
            "COLONIZER_Y",
            "LD_PRELOAD",
        ] {
            assert!(validate_env(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn hosts_are_named_public_dns_names_only() {
        assert_eq!(
            validate_hosts(&[" API.Stripe.com. ".into(), "api.stripe.com".into(), "files.stripe.com".into()]).unwrap(),
            vec!["api.stripe.com", "files.stripe.com"],
            "trimmed, lower-cased, deduplicated"
        );
        for bad in [
            "*.stripe.com",
            "*",
            "https://api.stripe.com",
            "api.stripe.com:443",
            "api.stripe.com/v1",
            "10.0.0.1",
            "::1",
            "localhost",
            "db.internal",
            "printer.local",
            "stripe",
            "-bad.com",
        ] {
            assert!(validate_hosts(&[bad.into()]).is_err(), "{bad}");
        }
        assert!(validate_hosts(&[]).is_err(), "at least one host");
        assert!(validate_hosts(&["  ".into()]).is_err(), "blank is no host");
    }

    #[test]
    fn scope_admits_all_one_org_or_one_repo() {
        assert!(Scope::All.admits("acme/web"));
        let org = Scope::Org { org: "Acme".into() };
        assert!(org.admits("acme/web") && org.admits("ACME/api"));
        assert!(!org.admits("acmex/web") && !org.admits("other/acme"));
        let repo = Scope::Repo { repo: "acme/web".into() };
        assert!(repo.admits("Acme/Web"));
        assert!(!repo.admits("acme/api"));
        assert!(validate_scope(Scope::Repo { repo: "noslash".into() }).is_err());
        assert!(validate_scope(Scope::Org { org: "".into() }).is_err());
    }

    #[test]
    fn the_boot_spec_substitutes_per_host_and_the_prompt_names_but_never_shows() {
        let stripe = secret("STRIPE_TEST_KEY", &["api.stripe.com"], Scope::All);
        let granted = vec![(stripe.clone(), "sk_test_supersecret".to_string())];
        let boot = boot_secrets(&granted);
        assert_eq!(boot.len(), 1);
        assert_eq!(boot[0].env, "STRIPE_TEST_KEY");
        assert_eq!(boot[0].hosts, vec!["api.stripe.com"]);
        assert_eq!(boot[0].value, "sk_test_supersecret");

        let prompt = prompt_block(&[&stripe]);
        assert!(prompt.contains("`STRIPE_TEST_KEY`, for api.stripe.com only"), "{prompt}");
        assert!(prompt.contains("placeholder") && prompt.contains("Do not print"), "{prompt}");
        assert!(!prompt.contains("sk_test"), "never the value: {prompt}");
        assert!(prompt_block(&[]).is_empty(), "no block without secrets");
    }

    #[test]
    fn the_registry_keeps_metadata_and_for_colony_filters_by_scope() {
        let dir = std::env::temp_dir().join(format!("colonizer-colony-secrets-{}", util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let list = vec![
            secret("ALL_KEY", &["a.example.com"], Scope::All),
            secret("ORG_KEY", &["b.example.com"], Scope::Org { org: "acme".into() }),
            secret("REPO_KEY", &["c.example.com"], Scope::Repo { repo: "acme/web".into() }),
            secret("NO_VALUE", &["d.example.com"], Scope::All),
        ];
        save(&dir, &list).unwrap();
        for (env, value) in [("ALL_KEY", "v1"), ("ORG_KEY", "v2"), ("REPO_KEY", "v3")] {
            util::write_secret(&value_path(&dir, env), value).unwrap();
        }
        let raw = std::fs::read_to_string(registry_file(&dir)).unwrap();
        assert!(
            !raw.contains("v1") && !raw.contains("v2"),
            "the registry holds no values: {raw}"
        );

        let names = |repo: &str| for_colony(&dir, repo).into_iter().map(|(s, _)| s.env).collect::<Vec<_>>();
        assert_eq!(names("acme/web"), vec!["ALL_KEY", "ORG_KEY", "REPO_KEY"]);
        assert_eq!(names("acme/api"), vec!["ALL_KEY", "ORG_KEY"]);
        assert_eq!(names("other/web"), vec!["ALL_KEY"], "and a secret with no value is left out");

        remove(&dir, "ORG_KEY").unwrap();
        assert_eq!(load(&dir).unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
