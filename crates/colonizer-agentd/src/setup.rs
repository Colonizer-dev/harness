//! The repository's setup hook (issue #753): before the agent runner starts, run
//! `<workspace>/.colonizer/setup.sh` with `sh` as root, so a repository can install the tooling its
//! own build needs. It runs on every boot — a colony's microVM rootfs does not survive a suspend —
//! with a timeout, output on a /tmp log (services.rs's convention), and its whole process group
//! killed if it overruns or a shutdown cancels it. Nothing here fails the boot: a missing hook and a spawn error are both
//! outcomes that are reported and lived with. The agent hears about it, and about the tools the VM
//! already carries, in [`note`], appended to its first prompt.

use crate::store::{EventStore, log_event};
use std::{
    future::Future,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::process::Command;

/// How long the hook may run before it is killed.
const DEFAULT_TIMEOUT_SECS: u64 = 600;
/// The hook's path, relative to the worktree.
const SCRIPT: &str = ".colonizer/setup.sh";
/// How many bytes of the hook's output the log event and the failure note quote.
const TAIL_BYTES: u64 = 2000;
/// The toolbox commands [`note`] reports on, probed by name on PATH.
const TOOLBOX: [&str; 8] = ["python3", "pip3", "jq", "rg", "curl", "git", "make", "unzip"];

/// Where the hook's output lands; the failure note names it so the agent can read it.
fn log_path() -> PathBuf {
    PathBuf::from("/tmp/colonizer-setup.log")
}

/// What became of the setup hook.
#[derive(Debug)]
pub enum Outcome {
    /// No `.colonizer/setup.sh` in the worktree: nothing to run.
    NotPresent,
    /// Ran to completion, in this long.
    Ok(Duration),
    /// Did not run to a clean exit: the exit code (`None` if a signal killed it) and the log tail.
    Failed(Option<i32>, String),
    /// Ran past its timeout and was killed; the timeout, in seconds.
    TimedOut(u64),
    /// A shutdown arrived while it ran: its process group was killed and the daemon is stopping.
    Cancelled,
}

impl Outcome {
    /// The colony-log line for this outcome — `None` when the hook was absent, because nothing
    /// happened. `info` when it ran, `warn` when it did not finish.
    pub fn log_line(&self) -> Option<(&'static str, String)> {
        let (level, what) = match self {
            Outcome::NotPresent | Outcome::Cancelled => return None,
            Outcome::Ok(elapsed) => ("info", format!("completed in {} ms", elapsed.as_millis())),
            Outcome::Failed(code, tail) if tail.is_empty() => ("warn", format!("failed ({})", exit_text(*code))),
            Outcome::Failed(code, tail) => ("warn", format!("failed ({}):\n{tail}", exit_text(*code))),
            Outcome::TimedOut(secs) => ("warn", format!("timed out after {secs} s and was killed")),
        };
        Some((level, format!("`{SCRIPT}` {what}; output in {}", log_path().display())))
    }
}

/// Runs the hook (if present) and logs the outcome to the colony's event log. Never fails the boot.
/// `cancel` is the daemon's shutdown race: when it fires the hook's process group is killed and the
/// outcome is [`Outcome::Cancelled`], so the caller can stop before launching the agent.
pub async fn run(workspace: &Path, store: &EventStore, cancel: impl Future<Output = ()>) -> Outcome {
    let outcome = run_with_timeout(workspace, Duration::from_secs(DEFAULT_TIMEOUT_SECS), &log_path(), cancel).await;
    if let Some((level, message)) = outcome.log_line() {
        store.append(log_event(level, message));
    }
    outcome
}

/// The hook itself: `sh` in its own session (so a timeout or `cancel` kills the script and whatever
/// it spawned, not just `sh`), cwd the worktree, stdin closed, stdout and stderr written to `log`,
/// which is truncated per run. Timeout and cancellation are parameters so the tests can drive them.
async fn run_with_timeout(workspace: &Path, timeout: Duration, log: &Path, cancel: impl Future<Output = ()>) -> Outcome {
    let script = workspace.join(SCRIPT);
    if !script.is_file() {
        return Outcome::NotPresent;
    }
    let file = match std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(log) {
        Ok(file) => file,
        Err(e) => return Outcome::Failed(None, format!("cannot open {}: {e}", log.display())),
    };
    let err_file = match file.try_clone() {
        Ok(file) => file,
        Err(e) => return Outcome::Failed(None, format!("cannot share {}: {e}", log.display())),
    };
    let mut command = Command::new("sh");
    command
        .arg(&script)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err_file));
    // SAFETY: a single async-signal-safe call before exec; a failure surfaces as a spawn error below.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return Outcome::Failed(None, format!("cannot start `sh {SCRIPT}`: {e}")),
    };
    let pid = child.id();
    let started = Instant::now();
    tokio::select! {
        result = tokio::time::timeout(timeout, child.wait()) => match result {
            Ok(Ok(status)) if status.success() => Outcome::Ok(started.elapsed()),
            Ok(Ok(status)) => Outcome::Failed(status.code(), read_tail(log)),
            Ok(Err(e)) => Outcome::Failed(None, format!("wait failed: {e}")),
            Err(_) => {
                kill_group(pid);
                let _ = child.wait().await;
                Outcome::TimedOut(timeout.as_secs())
            }
        },
        _ = cancel => {
            // A shutdown arrived: kill what the hook spawned rather than orphan it, so the caller
            // can stop the daemon without leaving it behind.
            kill_group(pid);
            let _ = child.wait().await;
            Outcome::Cancelled
        }
    }
}

/// Kills the hook's whole process group — a negative pid, because the script leads its own session —
/// before the caller reaps `sh`; a group already gone is ESRCH and harmless.
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        // SAFETY: signalling a process group this process created.
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
    }
}

/// The note lines appended to the agent's first prompt: the hook's failure (if any), then the
/// toolbox commands found on PATH. Pure, so the tests pin every clause; `present`/`missing` come
/// from [`probe_toolbox`], and only what was probed is ever claimed.
pub fn note(outcome: &Outcome, present: &[&str], missing: &[&str]) -> String {
    let mut notes = Vec::new();
    match outcome {
        Outcome::Failed(code, _) => notes.push(format!(
            "Note: the repository's setup hook `{SCRIPT}` failed ({}); its output is in {}. \
             Tools it installs may be missing.",
            exit_text(*code),
            log_path().display()
        )),
        Outcome::TimedOut(secs) => notes.push(format!(
            "Note: the repository's setup hook `{SCRIPT}` timed out after {secs} s; its output is in {}. \
             Tools it installs may be missing.",
            log_path().display()
        )),
        Outcome::NotPresent | Outcome::Ok(_) | Outcome::Cancelled => {}
    }
    let found: String = if present.is_empty() {
        "none".into()
    } else {
        present.join(", ")
    };
    let absent = if missing.is_empty() {
        String::new()
    } else {
        format!(" Missing: {}.", missing.join(", "))
    };
    notes.push(format!("Preinstalled in this VM: {found}.{absent}"));
    notes.join("\n\n")
}

/// Which [`TOOLBOX`] commands are on PATH, in two lists in TOOLBOX order. In-process probing, no
/// subprocess: an executable of that name in any PATH entry is a hit.
pub fn probe_toolbox() -> (Vec<&'static str>, Vec<&'static str>) {
    let (mut present, mut missing) = (Vec::new(), Vec::new());
    for name in TOOLBOX {
        if on_path(name) {
            present.push(name);
        } else {
            missing.push(name);
        }
    }
    (present, missing)
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| is_executable(&dir.join(name))))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn exit_text(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("exit {code}"),
        None => "killed by a signal".into(),
    }
}

/// The last [`TAIL_BYTES`] of the log, lossily decoded so a cut inside a multi-byte character costs
/// that character and not the note.
fn read_tail(path: &Path) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES))).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-agentd-setup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn hook(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir.join(".colonizer")).unwrap();
        std::fs::write(dir.join(SCRIPT), body).unwrap();
    }

    #[tokio::test]
    async fn an_absent_hook_is_not_present() {
        let dir = scratch("absent");
        let log = dir.join("setup.log");
        assert!(matches!(
            run_with_timeout(&dir, Duration::from_secs(5), &log, std::future::pending()).await,
            Outcome::NotPresent
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_clean_hook_is_ok_and_its_output_reaches_the_log() {
        let dir = scratch("ok");
        hook(&dir, "#!/bin/sh\necho installing deps\n");
        let log = dir.join("setup.log");
        let outcome = run_with_timeout(&dir, Duration::from_secs(30), &log, std::future::pending()).await;
        assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");
        assert!(std::fs::read_to_string(&log).unwrap().contains("installing deps"));
        assert_eq!(outcome.log_line().unwrap().0, "info");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_failed_with_the_code_and_the_log_tail() {
        let dir = scratch("failed");
        hook(&dir, "#!/bin/sh\necho boom >&2\nexit 3\n");
        let log = dir.join("setup.log");
        // A previous run in the same VM must not colour this one's tail: the log is truncated.
        std::fs::write(&log, "stale output from an earlier boot\n").unwrap();
        match run_with_timeout(&dir, Duration::from_secs(30), &log, std::future::pending()).await {
            Outcome::Failed(code, tail) => {
                assert_eq!(code, Some(3));
                assert!(tail.contains("boom"), "{tail}");
                assert!(!tail.contains("stale"), "{tail}");
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_hook_that_overruns_its_timeout_is_killed() {
        let dir = scratch("timeout");
        let pid_file = dir.join("pid");
        hook(&dir, &format!("#!/bin/sh\necho $$ > {}\nsleep 30\n", pid_file.display()));
        let log = dir.join("setup.log");
        let outcome = run_with_timeout(&dir, Duration::from_secs(1), &log, std::future::pending()).await;
        assert!(matches!(outcome, Outcome::TimedOut(1)), "{outcome:?}");
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        // SAFETY: `kill(pid, 0)` only probes whether the process still exists.
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the hook's group must be dead, pid {pid} lives"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_hook_cancelled_by_a_shutdown_is_killed() {
        let dir = scratch("cancel");
        let pid_file = dir.join("pid");
        hook(&dir, &format!("#!/bin/sh\necho $$ > {}\nsleep 30\n", pid_file.display()));
        let log = dir.join("setup.log");
        // Cancel only once the hook has recorded its pid, so the kill has a group to reach.
        let cancel = async {
            while !pid_file.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let outcome = run_with_timeout(&dir, Duration::from_secs(30), &log, cancel).await;
        assert!(matches!(outcome, Outcome::Cancelled), "{outcome:?}");
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        // SAFETY: `kill(pid, 0)` only probes whether the process still exists.
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "a cancelled hook's group must not be orphaned, pid {pid} lives"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_first_launch_note_pins_every_clause() {
        let all = ["python3", "pip3", "jq", "rg", "curl", "git", "make", "unzip"];
        assert_eq!(
            note(&Outcome::Ok(Duration::from_millis(12)), &all, &[]),
            "Preinstalled in this VM: python3, pip3, jq, rg, curl, git, make, unzip."
        );
        assert_eq!(
            note(&Outcome::NotPresent, &["git"], &["rg", "make"]),
            "Preinstalled in this VM: git. Missing: rg, make."
        );
        let failed = note(&Outcome::Failed(Some(3), "boom".into()), &["git"], &[]);
        assert!(
            failed.contains("setup hook `.colonizer/setup.sh` failed (exit 3)"),
            "{failed}"
        );
        assert!(failed.contains("output is in /tmp/colonizer-setup.log"), "{failed}");
        assert!(failed.contains("Tools it installs may be missing"), "{failed}");
        let timed_out = note(&Outcome::TimedOut(600), &["git"], &[]);
        assert!(timed_out.contains("timed out after 600 s"), "{timed_out}");
    }
}
