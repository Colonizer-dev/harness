//! The `runtime` half of `GET /api/status`: whether this machine can actually boot a colony. These
//! are the checks `scripts/install-release.sh` makes at install time — the platform, `/dev/kvm` on
//! Linux, the two commands colonies need, the Claude Code binary the mothership itself runs —
//! reported live instead of only once, at install.

use crate::{App, resolve_host_claude_bin, telemetry, util};
use serde::Serialize;
use std::{
    future::Future,
    time::{Duration, Instant},
};
use tokio::process::Command;

/// How long the status poll reuses one probe result. The probes spawn subprocesses, and every open
/// tab polls `/api/status` every 30 s; `?fresh=1` bypasses this so a "Check again" button always
/// gets a real answer.
pub const RUNTIME_CACHE_TTL: Duration = Duration::from_secs(10);

/// The bound on one `--version` probe. A version exec is spawn-and-print, done in well under a
/// second on a healthy machine; two seconds leaves it room to be slow once without letting a wedged
/// binary hold the poll. The timeout wraps (and so drops) the `exec` future, which is what makes its
/// `kill_on_drop` actually kill the child — the shape github's `viewer` uses. `util::exec` alone
/// applies no timeout: dropped is the only way its child dies.
const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The bound on the whole host Claude binary walk. `find_claude_bin` is shared with the guest path
/// and its per-candidate execs are unbounded — that is the pre-existing trade, filed separately, and
/// deliberately left alone here — but the *call* from this probe can be bounded without touching it:
/// dropping the walk drops whichever exec is in flight, killing the child. Five seconds, the bound
/// claude_login gives its own polled lookup, lets the walk try its several candidates.
const HOST_BIN_TIMEOUT: Duration = Duration::from_secs(5);

/// The device colonies boot through. Linux only, as in the installer.
const KVM_DEVICE: &str = "/dev/kvm";

/// The `runtime` object of `GET /api/status`. These field names are the JSON contract the web UI is
/// written against.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Runtime {
    /// The released platform name, the same string the live map heartbeat sends.
    pub platform: &'static str,
    /// Linux only; `null` elsewhere, where there is no `/dev/kvm` to fix.
    pub kvm: Option<Kvm>,
    pub git: Tool,
    pub gh: Tool,
    /// The native Claude Code binary the mothership runs itself, for subscription sign-in — never
    /// the Linux binary mounted into colonies, which `sandbox.claude_bin` already reports.
    pub host_claude_bin: Option<String>,
    /// Set when `host_claude_bin` is None, saying plainly why.
    pub host_claude_bin_error: Option<String>,
}

/// `/dev/kvm`, as the installer's `[ -r /dev/kvm ] && [ -w /dev/kvm ]` sees it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Kvm {
    pub ok: bool,
    pub error: Option<String>,
}

/// A command colonies need, answered by `--version`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tool {
    pub ok: bool,
    /// The bare version number when it could be parsed out of the output.
    pub version: Option<String>,
    /// Set when `ok` is false, saying what was missing or went wrong.
    pub error: Option<String>,
}

/// The last probe, cached on `App` so the poll does not re-spawn the subprocesses. Follows the
/// `claude_account` pattern: everything needed to decide freshness travels with the answer.
#[derive(Clone)]
pub struct Cached {
    probed_at: Instant,
    value: Runtime,
}

/// Runs every probe at once. They are independent, so they race. Each subprocess this probe spawns
/// carries its own bound, so a wedged binary degrades only its own answer and the poll always
/// returns: the `--version` execs get [`TOOL_PROBE_TIMEOUT`], the host binary walk gets
/// [`HOST_BIN_TIMEOUT`]. Nothing here awaits an unbounded exec.
pub async fn probe(app: &App) -> Runtime {
    let (git, gh, host) = tokio::join!(probe_tool("git"), probe_tool("gh"), host_bin(app, HOST_BIN_TIMEOUT));
    Runtime {
        platform: telemetry::platform(),
        kvm: probe_kvm().await,
        git,
        gh,
        host_claude_bin: host.0,
        host_claude_bin_error: host.1,
    }
}

/// The host Claude binary answer as `(path, error)`, exactly one set. The bound covers the whole
/// walk, which may exec several candidates: a timeout answers `None` with a sentence naming it,
/// degrading this one field, not the probe.
async fn host_bin(app: &App, budget: Duration) -> (Option<String>, Option<String>) {
    match tokio::time::timeout(budget, resolve_host_claude_bin(app)).await {
        Ok(Ok(path)) => (Some(path.display().to_string()), None),
        Ok(Err(e)) => (None, Some(format!("{e:#}"))),
        Err(_) => (
            None,
            Some(format!("finding the host Claude Code binary timed out after {budget:?}")),
        ),
    }
}

/// The `runtime` object of `GET /api/status`: from the cache when it is warm, re-probed when it is
/// cold, stale or when `fresh` asks.
pub async fn status_runtime(app: &App, fresh: bool) -> Runtime {
    cached(app, fresh, || probe(app)).await
}

/// The cache decision, split from the probe itself so tests can inject one that counts. The lock is
/// held across the probe, as claude_login's account lookup does, so concurrent polls share one run
/// instead of stacking several.
async fn cached<C, Fut>(app: &App, fresh: bool, mut run: C) -> Runtime
where
    C: FnMut() -> Fut,
    Fut: Future<Output = Runtime>,
{
    let mut cache = app.runtime_cache.lock().await;
    if !fresh
        && let Some(hit) = cache.as_ref()
        && hit.probed_at.elapsed() < RUNTIME_CACHE_TTL
    {
        return hit.value.clone();
    }
    let value = run().await;
    *cache = Some(Cached {
        probed_at: Instant::now(),
        value: value.clone(),
    });
    value
}

/// One `--version` probe: `ok` with the bare version number, or `ok: false` with a sentence, never
/// both blank. A command that is not installed is the common failure, and `exec` words it as one.
async fn probe_tool(name: &str) -> Tool {
    let mut cmd = Command::new(name);
    cmd.arg("--version");
    tool_from(name, util::exec(&mut cmd), TOOL_PROBE_TIMEOUT).await
}

/// Maps one bounded exec to the [`Tool`] shape. The timeout wraps (and so drops) the exec future,
/// which is what fires its `kill_on_drop`; a tool that does not answer in time is `ok: false` with a
/// sentence naming the timeout, and the rest of the probe answers regardless.
async fn tool_from(name: &str, run: impl Future<Output = anyhow::Result<String>>, budget: Duration) -> Tool {
    match tokio::time::timeout(budget, run).await {
        Ok(Ok(output)) => {
            let version = parse_version(&output);
            if version.is_empty() {
                Tool {
                    ok: false,
                    version: None,
                    error: Some(format!("`{name} --version` printed nothing")),
                }
            } else {
                Tool {
                    ok: true,
                    version: Some(version),
                    error: None,
                }
            }
        }
        Ok(Err(e)) => Tool {
            ok: false,
            version: None,
            error: Some(format!("{e:#}")),
        },
        Err(_) => Tool {
            ok: false,
            version: None,
            error: Some(format!("`{name} --version` timed out after {budget:?}")),
        },
    }
}

/// `git version 2.45.0` becomes `2.45.0`; `gh version 2.60.0 (2024-11-05)` becomes `2.60.0`. The
/// first word after `version` that starts with a digit is taken as the number, and anything that
/// does not fit that shape falls back to the raw first line, which still says something.
fn parse_version(output: &str) -> String {
    let first = output.lines().next().unwrap_or_default().trim();
    let mut words = first.split_whitespace();
    while let Some(word) = words.next() {
        if word.eq_ignore_ascii_case("version")
            && let Some(candidate) = words.next()
            && candidate.starts_with(|c: char| c.is_ascii_digit())
        {
            return candidate.to_string();
        }
    }
    first.to_string()
}

/// The kvm answer as a pure function of what the probe would find, so the decision is testable
/// without a real `/dev/kvm`. Linux only — colonies are KVM microVMs, so nowhere else has anything
/// to check and the key is `null` there. Read and write are both required, as in the installer, and
/// a failure names the user in the installer's own words. The fix is not offered here: that is the
/// web UI's business.
fn kvm_for(os: &str, readable: bool, writable: bool, user: &str) -> Option<Kvm> {
    if os != "linux" {
        return None;
    }
    if readable && writable {
        return Some(Kvm { ok: true, error: None });
    }
    Some(Kvm {
        ok: false,
        error: Some(format!(
            "/dev/kvm is not readable and writable by {user}; colonies are KVM microVMs"
        )),
    })
}

/// What the probe finds on this machine. Opening the device twice — once to read, once to write —
/// is the ground truth for "readable and writable by this process", which is exactly what `[ -r ]`
/// and `[ -w ]` ask of the installer's own process. Opening `/dev/kvm` changes nothing; a VM is
/// only created by an ioctl afterwards.
async fn probe_kvm() -> Option<Kvm> {
    if std::env::consts::OS != "linux" {
        return None;
    }
    let readable = std::fs::OpenOptions::new().read(true).open(KVM_DEVICE).is_ok();
    let writable = std::fs::OpenOptions::new().write(true).open(KVM_DEVICE).is_ok();
    // The username only decorates the failure, so it is resolved only then.
    let user = if readable && writable {
        String::new()
    } else {
        current_user().await
    };
    kvm_for(std::env::consts::OS, readable, writable, &user)
}

/// The user a kvm failure names: `id -un`, the same answer the installer's message interpolates,
/// with the environment as a fallback and a plain phrase when neither answers.
async fn current_user() -> String {
    let mut cmd = Command::new("id");
    cmd.arg("-un");
    if let Ok(user) = util::exec(&mut cmd).await {
        let user = user.trim();
        if !user.is_empty() {
            return user.to_string();
        }
    }
    util::env_nonempty("USER")
        .or_else(|| util::env_nonempty("LOGNAME"))
        .unwrap_or_else(|| "the current user".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{test_app, test_app_with};
    use serde_json::{Value, json};
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    /// A Runtime with every non-kvm field filled, so serialisation tests read only what they name.
    fn runtime_with(kvm: Option<Kvm>) -> Runtime {
        Runtime {
            platform: "linux-x86_64",
            kvm,
            git: Tool {
                ok: true,
                version: Some("2.45.0".into()),
                error: None,
            },
            gh: Tool {
                ok: true,
                version: Some("2.60.0".into()),
                error: None,
            },
            host_claude_bin: Some("/Users/me/.local/bin/claude".into()),
            host_claude_bin_error: None,
        }
    }

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-runtime-{}", util::short_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        dir
    }

    #[test]
    fn git_versions_trim_to_the_bare_number() {
        assert_eq!(parse_version("git version 2.45.0\n"), "2.45.0");
    }

    #[test]
    fn gh_versions_drop_the_date_and_the_trailing_lines() {
        assert_eq!(
            parse_version("gh version 2.60.0 (2024-11-05)\nhttps://github.com/cli/cli\n"),
            "2.60.0"
        );
    }

    #[test]
    fn unexpected_version_output_falls_back_to_the_first_line() {
        assert_eq!(parse_version("2.45.0\n"), "2.45.0", "a bare number needs no trimming");
        assert_eq!(
            parse_version("the program cannot be executed"),
            "the program cannot be executed"
        );
        assert_eq!(
            parse_version("a version of something else entirely"),
            "a version of something else entirely"
        );
        assert_eq!(
            parse_version(""),
            "",
            "nothing printed parses to nothing, which probe_tool calls a failure"
        );
    }

    #[test]
    fn kvm_is_reported_only_on_linux() {
        assert!(
            kvm_for("linux", true, true, "ada").is_some(),
            "on Linux kvm is an object either way"
        );
        assert_eq!(kvm_for("macos", true, true, "ada"), None, "a Mac has no /dev/kvm to fix");
        assert_eq!(kvm_for("darwin", false, false, "ada"), None);
        assert_eq!(kvm_for("freebsd", true, true, "ada"), None);
    }

    #[test]
    fn a_readable_and_writable_dev_kvm_passes_with_no_error() {
        let kvm = kvm_for("linux", true, true, "ada").unwrap();
        assert!(kvm.ok);
        assert_eq!(kvm.error, None);
    }

    #[test]
    fn a_dev_kvm_missing_either_permission_fails_with_the_installer_sentence() {
        for (readable, writable) in [(false, false), (true, false), (false, true)] {
            let kvm = kvm_for("linux", readable, writable, "ada").unwrap();
            assert!(!kvm.ok, "readable={readable} writable={writable}");
            assert_eq!(
                kvm.error.as_deref(),
                Some("/dev/kvm is not readable and writable by ada; colonies are KVM microVMs"),
                "readable={readable} writable={writable}"
            );
        }
    }

    #[test]
    fn the_runtime_object_serialises_as_documented() {
        let value = serde_json::to_value(runtime_with(Some(Kvm { ok: true, error: None }))).unwrap();
        assert_eq!(value["platform"], "linux-x86_64");
        assert_eq!(value["kvm"], json!({"ok": true, "error": null}));
        assert_eq!(value["git"], json!({"ok": true, "version": "2.45.0", "error": null}));
        assert_eq!(value["gh"], json!({"ok": true, "version": "2.60.0", "error": null}));
        assert_eq!(value["host_claude_bin"], "/Users/me/.local/bin/claude");
        assert_eq!(value["host_claude_bin_error"], Value::Null);
    }

    #[test]
    fn off_linux_the_kvm_key_is_null_not_an_object() {
        let value = serde_json::to_value(runtime_with(None)).unwrap();
        assert_eq!(value["kvm"], Value::Null);
    }

    #[test]
    fn a_failed_tool_is_ok_false_with_an_error_and_no_version() {
        let tool = Tool {
            ok: false,
            version: None,
            error: Some("failed to start `git --version`".into()),
        };
        let value = serde_json::to_value(&tool).unwrap();
        assert_eq!(
            value,
            json!({"ok": false, "version": null, "error": "failed to start `git --version`"})
        );
    }

    #[tokio::test]
    async fn a_tool_that_never_answers_times_out_to_ok_false_with_a_sentence_naming_the_timeout() {
        // A future that never resolves stands in for a wedged binary; a zero budget elapses on the
        // first poll, so the mapping is exercised without waiting real seconds.
        let tool = tool_from("git", std::future::pending(), Duration::ZERO).await;
        assert!(!tool.ok);
        assert_eq!(tool.version, None, "a timeout answers no version");
        let error = tool.error.unwrap();
        assert!(error.contains("timed out"), "{error}");
        assert!(
            error.contains("git --version"),
            "the sentence names the command that hung: {error}"
        );
    }

    #[tokio::test]
    async fn a_hanging_version_exec_is_dropped_by_its_timeout_instead_of_hanging_the_probe() {
        // `exec sleep 30` really does hang, and only dies if the timeout drops the exec future, which
        // is what fires `kill_on_drop`. Were the timeout absent or beside the exec rather than around
        // it, this test would sit for the full 30 s.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exec sleep 30"]);
        let tool = tool_from("git", util::exec(&mut cmd), Duration::from_millis(50)).await;
        assert!(!tool.ok);
        assert_eq!(tool.version, None);
        let error = tool.error.unwrap();
        assert!(error.contains("timed out"), "{error}");
    }

    #[tokio::test]
    async fn a_hanging_host_binary_walk_times_out_and_degrades_only_that_field() {
        let root = temp_root();
        let bin = root.join("claude");
        std::fs::write(&bin, "#!/bin/sh\nexec sleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let app = test_app_with(&root, |cfg| cfg.claude_bin = Some(bin.display().to_string()));
        // 50 ms, so the walk is cut off mid-exec; the drop is what kills the sleeping child.
        let (path, error) = host_bin(&app, Duration::from_millis(50)).await;
        assert_eq!(path, None, "nothing was resolved, so the field degrades to null");
        let error = error.unwrap();
        assert!(error.contains("timed out"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A probe that counts how often it ran. One counter can serve several calls of `cached`,
    /// which takes its probe by value, so each call gets its own closure over a shared counter.
    fn counting(runs: &Arc<AtomicUsize>) -> impl FnMut() -> std::future::Ready<Runtime> {
        let runs = runs.clone();
        move || {
            runs.fetch_add(1, Ordering::SeqCst);
            std::future::ready(runtime_with(None))
        }
    }

    #[tokio::test]
    async fn a_repeat_within_the_ttl_answers_from_the_cache_without_reprobing() {
        let root = temp_root();
        let app = test_app(&root);
        let runs = Arc::new(AtomicUsize::new(0));
        let first = cached(&app, false, counting(&runs)).await;
        let second = cached(&app, false, counting(&runs)).await;
        assert_eq!(first, second);
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the second call inside the TTL must not re-probe"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_fresh_request_bypasses_the_cache_and_reprobes() {
        let root = temp_root();
        let app = test_app(&root);
        let runs = Arc::new(AtomicUsize::new(0));
        cached(&app, false, counting(&runs)).await;
        let fresh = cached(&app, true, counting(&runs)).await;
        assert_eq!(
            fresh,
            runtime_with(None),
            "a bypassed cache still answers, with what the probe found"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            2,
            "?fresh=1 re-probes even though the cache is warm"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_entry_older_than_the_ttl_is_reprobed() {
        let root = temp_root();
        let app = test_app(&root);
        let runs = Arc::new(AtomicUsize::new(0));
        cached(&app, false, counting(&runs)).await;
        app.runtime_cache.lock().await.as_mut().unwrap().probed_at =
            Instant::now() - (RUNTIME_CACHE_TTL + Duration::from_secs(1));
        cached(&app, false, counting(&runs)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 2, "a stale entry is probed again");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_real_probe_reports_the_telemetry_platform_and_the_platforms_kvm_shape() {
        let root = temp_root();
        let app = test_app(&root);
        let runtime = probe(&app).await;
        assert_eq!(runtime.platform, telemetry::platform());
        if cfg!(target_os = "linux") {
            assert!(
                runtime.kvm.is_some(),
                "on Linux kvm is an object, however the device is permissioned"
            );
        } else {
            assert!(runtime.kvm.is_none(), "nowhere but Linux has a kvm answer");
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
