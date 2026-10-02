//! Service relaunch on a resumed boot and the in-guest `colonizer-svc` registry (issue #700). On a
//! resumed boot agentd relaunches what the host declared in session.json's `restore` block before
//! the agent sees its brief, waits on every readiness probe, and prefixes the first user message
//! with a short report. While the colony runs, the agent registers long-lived commands with
//! `colonizer-svc start` (this binary under that name); each drops a ServiceSpec record in
//! COLONIZER_SERVICES_DIR for the host to fold into the next resume. Records carry env var NAMES,
//! never values — the colony env's secrets do not travel through /colonizer/services.

use crate::{
    config::{Restore, ServiceSpec, SessionConfig},
    harden,
    store::{EventStore, log_event},
};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    process::Command,
    time::Instant,
};

/// How often a readiness probe retries.
const POLL: Duration = Duration::from_millis(200);
const SERVICES_DIR_VAR: &str = "COLONIZER_SERVICES_DIR";

const SVC_USAGE: &str =
    "usage: colonizer-svc start NAME [--ready PORT|URL] [--cwd DIR] [--env NAME]... [--timeout SECS] -- CMD [ARGS...]
       colonizer-svc stop NAME
  start  records the service in COLONIZER_SERVICES_DIR and runs it detached via `sh -c`, logging to
         /tmp/colonizer-svc-NAME.log; --env takes bare variable NAMES only — a record never carries a value.
  stop   removes the record; the running process, if any, is left alone.";

/// The guest dir holding agentd (a read-only single-file mount) and, beside it, the `colonizer-svc`
/// link the boot script makes.
pub const BIN_DIR: &str = "/opt/colonizer/bin";

/// The warn event for a guest whose boot script could not link `colonizer-svc` into `bin_dir`
/// (the script runs `set -u`, not `set -e`, so a failed link does not stop the boot). `None` when
/// the link resolves to the binary, or when `bin_dir` holds no agentd at all — not a colony guest,
/// e.g. agentd run by hand on a host.
pub fn svc_link_warning(bin_dir: &Path) -> Option<String> {
    if !bin_dir.join("colonizer-agentd").is_file() {
        return None;
    }
    let link = bin_dir.join("colonizer-svc");
    (!link.is_file()).then(|| {
        format!(
            "`colonizer-svc` is missing from {}: the boot script could not link it, so the agent cannot \
             register services for a resume under that name; `colonizer-agentd svc ...` is the same command",
            bin_dir.display()
        )
    })
}

/// What became of one declared service on a resumed boot.
struct Restored {
    spec: ServiceSpec,
    outcome: Outcome,
}

enum Outcome {
    /// Relaunched and answering after this many seconds (`None`: no probe, taken on faith).
    Ready(Option<f64>),
    NotReady,       // spawned but never answered inside the spec's timeout
    Failed(String), // could not be spawned at all
    Lost,           // restart: false — left dead on purpose, reported so the agent can rerun it
}

/// Relaunches `restore.services` before the agent's first message and returns the report to
/// prepend to the brief. A session without `restore` (every fresh boot) does nothing.
pub async fn relaunch(config: &SessionConfig, store: &EventStore) -> Option<String> {
    let restore = config.restore.as_ref()?;
    let results = run(restore, &config.workspace, &config.agent.env).await;
    let message = restore_message(restore.suspended, &results);
    (!message.is_empty()).then(|| {
        store.append(log_event("info", message.clone()));
        message
    })
}

/// Spawns everything marked `restart` and waits on all readiness probes concurrently; the rest are
/// reported lost.
async fn run(restore: &Restore, workspace: &Path, agent_env: &BTreeMap<String, String>) -> Vec<Restored> {
    let (mut waits, mut results) = (Vec::new(), Vec::new());
    for spec in &restore.services {
        if spec.restart {
            let (spec, workspace, env) = (spec.clone(), workspace.to_path_buf(), agent_env.clone());
            waits.push(tokio::spawn(async move {
                let outcome = match spawn(&spec, &workspace, &env) {
                    Err(error) => Outcome::Failed(error),
                    Ok(()) => match wait_ready(&spec).await {
                        Ok(secs) => Outcome::Ready(secs),
                        Err(()) => Outcome::NotReady,
                    },
                };
                Restored { spec, outcome }
            }));
        } else {
            results.push(Restored {
                spec: spec.clone(),
                outcome: Outcome::Lost,
            });
        }
    }
    for wait in waits {
        if let Ok(restored) = wait.await {
            results.push(restored);
        }
    }
    results
}

/// Detached relaunch of one service: `sh -c` in its own session (so a group kill of the daemon or
/// its PTY never reaches a dev server), the runner child's hardening (it runs the same untrusted
/// code the agent would), output to a /tmp log, and from the colony env only the variable names
/// the spec lists plus the services dir. The child handle is dropped rather than waited on: tokio
/// reaps it at exit, and nothing here ever kills it — the service outlives the readiness wait.
fn spawn(spec: &ServiceSpec, workspace: &Path, agent_env: &BTreeMap<String, String>) -> Result<(), String> {
    let log = svc_log(&spec.name);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("cannot open {}: {e}", log.display()))?;
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(&spec.cmd)
        .current_dir(resolve_cwd(workspace, spec.cwd.as_deref()))
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            file.try_clone().map_err(|e| format!("cannot share the log fd: {e}"))?,
        ))
        .stderr(Stdio::from(file));
    // Names only, and the values are looked up in the colony env — never carried anywhere.
    for name in spec.env.iter().chain([&SERVICES_DIR_VAR.to_string()]) {
        if let Some(value) = agent_env.get(name.as_str()) {
            command.env(name, value);
        }
    }
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
        command.pre_exec(harden::Hardening::prepare().guard());
    }
    command.spawn().map_err(|e| format!("`{}`: {e}", spec.cmd))?;
    Ok(())
}

/// `cwd` is relative to the worktree (the runner's directory); an absolute path passes through.
fn resolve_cwd(workspace: &Path, cwd: Option<&str>) -> PathBuf {
    match cwd {
        Some(cwd) if Path::new(cwd).is_absolute() => PathBuf::from(cwd),
        Some(cwd) => workspace.join(cwd),
        None => workspace.to_path_buf(),
    }
}

/// Polls the probe every 200 ms; `Ok` is the seconds it took (`None` when nothing was declared to
/// wait for), `Err` the timeout.
async fn wait_ready(spec: &ServiceSpec) -> Result<Option<f64>, ()> {
    let Some(ready) = spec.ready.as_deref() else { return Ok(None) };
    let (probe, started) = (probe(ready), Instant::now());
    loop {
        if answering(&probe, ready).await {
            return Ok(Some(started.elapsed().as_secs_f64()));
        }
        if started.elapsed() >= Duration::from_secs(spec.timeout_secs) {
            return Err(());
        }
        tokio::time::sleep(POLL).await;
    }
}

/// How a `ready` probe connects: a decimal port on loopback, or an http(s) URL's host:port (https
/// is only TCP-checked — there is no TLS stack this small).
#[derive(Debug, PartialEq)]
enum Probe {
    Port(u16),
    Url { host: String, port: u16 },
}

fn probe(ready: &str) -> Probe {
    if let Ok(port) = ready.parse() {
        return Probe::Port(port);
    }
    let (scheme, rest) = ready.split_once("://").unwrap_or(("http", ready));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let default_port = if scheme == "https" { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host.trim_matches(['[', ']']), port.parse().unwrap_or(default_port))
        }
        _ => (authority, default_port),
    };
    Probe::Url { host: host.into(), port }
}

/// One readiness attempt: a TCP connect, and for http a GET that must draw a status line — any
/// status counts, a 500 is as much an answer as the agent needs.
async fn answering(probe: &Probe, ready: &str) -> bool {
    let (host, port) = match probe {
        Probe::Port(port) => return TcpStream::connect(("127.0.0.1", *port)).await.is_ok(),
        Probe::Url { host, port } => (host, *port),
    };
    let Ok(mut stream) = TcpStream::connect((host.as_str(), port)).await else {
        return false;
    };
    if ready.starts_with("https") {
        return true;
    }
    let (_, rest) = ready.split_once("://").unwrap_or(("http", ready));
    let path = match rest.find(['/', '?', '#']) {
        Some(start) => &rest[start..],
        None => "/",
    };
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let mut response = [0u8; 16];
    stream.write_all(request.as_bytes()).await.is_ok()
        && matches!(stream.read(&mut response).await, Ok(n) if response[..n].starts_with(b"HTTP/"))
}

/// The first message of a resumed turn: what came back, what never answered, what failed to start,
/// and what is gone. Pure, so the tests pin every clause; empty when there is nothing to say.
fn restore_message(suspended: bool, results: &[Restored]) -> String {
    if results.is_empty() {
        // A suspension with nothing to bring back still opens the turn with one short line; a
        // non-suspended resume with no services has nothing to say at all.
        return if suspended {
            "Restored from suspension. No services to restart."
        } else {
            ""
        }
        .into();
    }
    let (mut restarted, mut not_ready, mut failed, mut lost) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for result in results {
        let spec = &result.spec;
        let at = match spec.ready.as_deref() {
            Some(ready) if ready.parse::<u16>().is_ok() => format!(" on :{ready}"),
            Some(url) => format!(" at {url}"),
            None => String::new(),
        };
        match &result.outcome {
            Outcome::Ready(Some(secs)) => restarted.push(format!("`{}`{at} (ready in {secs:.1} s)", spec.name)),
            Outcome::Ready(None) => restarted.push(format!("`{}`", spec.name)),
            Outcome::NotReady => not_ready.push(format!(
                "`{}` (no answer{at} after {} s, log {})",
                spec.name,
                spec.timeout_secs,
                svc_log(&spec.name).display()
            )),
            Outcome::Failed(error) => failed.push(format!("`{}` ({error})", spec.name)),
            // A background command's only identity is its text; a record has a name.
            Outcome::Lost if spec.source == "background" => lost.push(format!("background `{}`", spec.cmd)),
            Outcome::Lost => lost.push(format!("`{}`", spec.name)),
        }
    }
    let mut parts = vec![
        if suspended {
            "Restored from suspension."
        } else {
            "Resumed on a fresh machine."
        }
        .to_string(),
    ];
    for (label, entries, tail) in [
        ("Restarted:", &restarted, "."),
        ("Not ready:", &not_ready, "."),
        ("Failed to start:", &failed, "."),
        ("Lost:", &lost, ", rerun if needed."),
    ] {
        if !entries.is_empty() {
            parts.push(format!("{label} {}{tail}", entries.join(", ")));
        }
    }
    parts.join(" ")
}

/// Where a service's output lands; the report names it so the agent can read why nothing answered.
fn svc_log(name: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/colonizer-svc-{name}.log"))
}

/// The `svc` entrypoint, reached as `colonizer-agentd svc ...` or by invoking this binary through
/// a `colonizer-svc` symlink (argv0). Exit codes follow agentd's usage convention.
pub async fn svc_main(argv: Vec<String>) -> std::process::ExitCode {
    let outcome = match argv.split_first() {
        None => Err(SVC_USAGE.to_string()),
        Some((verb, rest)) => match verb.as_str() {
            "start" => svc_start(rest).await,
            "stop" => svc_stop(rest),
            "help" | "--help" | "-h" => {
                println!("{SVC_USAGE}");
                Ok(())
            }
            _ => Err(format!("unknown svc command: {verb}\n\n{SVC_USAGE}")),
        },
    };
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("colonizer-svc: {error}");
            std::process::ExitCode::from(2)
        }
    }
}

/// `svc start` argument parsing, straight into the spec the record serializes from. Pure, so the
/// tests pin the no-secrets rule: `--env` takes a bare NAME, and `NAME=value` is refused.
fn parse_start(argv: &[String]) -> Result<ServiceSpec, String> {
    let mut iter = argv.iter();
    let name = iter.next().ok_or("start needs a service name")?;
    ensure_valid_name(name)?;
    let mut spec = ServiceSpec {
        name: name.clone(),
        restart: true,
        source: "registered".into(),
        ..Default::default()
    };
    let mut cmd = Vec::new();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--ready" => spec.ready = Some(take(flag, &mut iter)?),
            "--cwd" => spec.cwd = Some(take(flag, &mut iter)?),
            "--env" => {
                let name = take(flag, &mut iter)?;
                if name.is_empty() || name.contains('=') {
                    return Err("--env takes a bare variable NAME; a record never carries a value".into());
                }
                spec.env.push(name);
            }
            "--timeout" => {
                let secs = take(flag, &mut iter)?;
                spec.timeout_secs = secs
                    .parse()
                    .map_err(|_| format!("--timeout wants whole seconds, got `{secs}`"))?;
            }
            "--" => {
                cmd = iter.cloned().collect();
                break;
            }
            _ => return Err(format!("unknown argument: {flag}")),
        }
    }
    if cmd.is_empty() {
        return Err("start needs a command after `--`".into());
    }
    spec.cmd = shell_join(&cmd);
    Ok(spec)
}

fn take(flag: &str, iter: &mut std::slice::Iter<'_, String>) -> Result<String, String> {
    iter.next().cloned().ok_or_else(|| format!("{flag} needs a value"))
}

/// `svc start`: write the record (env as names only), then run the command detached exactly like a
/// relaunch does. Here the command inherits the runner env it was typed in — the agent inside chose
/// it; only the record is filtered, and its names are for the next resume.
async fn svc_start(argv: &[String]) -> Result<(), String> {
    let spec = parse_start(argv)?;
    let dir = services_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let record_path = dir.join(format!("{}.json", spec.name));
    let record = serde_json::to_string(&spec).map_err(|e| e.to_string())?;
    std::fs::write(&record_path, record).map_err(|e| format!("cannot write {}: {e}", record_path.display()))?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    spawn(&spec, &cwd, &BTreeMap::new())?;
    println!("started `{}` log {}", spec.name, svc_log(&spec.name).display());
    Ok(())
}

/// `svc stop`: remove the record so the next resume stops naming the service. The running process,
/// if any, is left alone — killing it is the agent's call, and on a fresh VM it is gone anyway.
fn svc_stop(argv: &[String]) -> Result<(), String> {
    let name = argv.first().ok_or("stop needs a service name")?;
    ensure_valid_name(name)?;
    let record_path = services_dir()?.join(format!("{name}.json"));
    std::fs::remove_file(&record_path).map_err(|e| format!("cannot remove {}: {e}", record_path.display()))?;
    println!("stopped `{name}`; the record is gone, the process if any is left running");
    Ok(())
}

fn services_dir() -> Result<PathBuf, String> {
    std::env::var_os(SERVICES_DIR_VAR)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .ok_or(format!(
            "{SERVICES_DIR_VAR} is not set; services cannot be registered without it"
        ))
}

/// Record names become file names the host reads back; anything outside this set is refused before
/// anything is written.
fn ensure_valid_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    ok.then_some(())
        .ok_or_else(|| format!("service names are [A-Za-z0-9._-] up to 64 characters, got {name:?}"))
}

/// Quotes argv into one `sh -c` string: the record's `cmd` is exactly what runs.
fn shell_join(cmd: &[String]) -> String {
    cmd.iter()
        .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn spec(name: &str, cmd: &str, ready: Option<&str>) -> ServiceSpec {
        ServiceSpec {
            name: name.into(),
            cmd: cmd.into(),
            ready: ready.map(str::to_string),
            ..Default::default()
        }
    }

    fn restored(spec: ServiceSpec, outcome: Outcome) -> Restored {
        Restored { spec, outcome }
    }

    /// Issue #700: a colony guest whose boot script could not link `colonizer-svc` gets a warn
    /// event naming the fallback; a working link, or a dir without agentd (not a guest), gets none.
    #[cfg(unix)]
    #[test]
    fn a_missing_svc_link_is_a_warning_only_in_a_colony_guest() {
        let dir = std::env::temp_dir().join(format!("colonizer-agentd-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(svc_link_warning(&dir), None, "no agentd here: not a colony guest");
        std::fs::write(dir.join("colonizer-agentd"), b"").unwrap();
        let warning = svc_link_warning(&dir).expect("agentd without its link warns");
        assert!(warning.contains("colonizer-agentd svc"), "{warning}");
        std::os::unix::fs::symlink(dir.join("colonizer-agentd"), dir.join("colonizer-svc")).unwrap();
        assert_eq!(svc_link_warning(&dir), None, "the link resolves to agentd");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn argv<'a>(args: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        args.into_iter().map(str::to_string).collect()
    }

    #[test]
    fn the_report_pins_every_clause() {
        let mut gone = spec("gone", "cargo test --workspace", None);
        gone.restart = false;
        gone.source = "background".into();
        let fail = "No such file or directory";
        let message = restore_message(
            true,
            &[
                restored(spec("web", "npm run dev", Some("5173")), Outcome::Ready(Some(3.24))),
                restored(spec("worker", "./worker", None), Outcome::Ready(None)),
                restored(spec("api", "npm run api", Some("8080")), Outcome::NotReady),
                restored(spec("bad", "definitely-not-a-binary", None), Outcome::Failed(fail.into())),
                restored(gone, Outcome::Lost),
            ],
        );
        assert_eq!(
            message,
            "Restored from suspension. \
             Restarted: `web` on :5173 (ready in 3.2 s), `worker`. \
             Not ready: `api` (no answer on :8080 after 60 s, log /tmp/colonizer-svc-api.log). \
             Failed to start: `bad` (No such file or directory). \
             Lost: background `cargo test --workspace`, rerun if needed."
        );
        assert_eq!(
            restore_message(true, &[]),
            "Restored from suspension. No services to restart."
        );
        assert_eq!(restore_message(false, &[]), "");
        let mut queue = spec("queue", "runq", None);
        queue.restart = false;
        assert_eq!(
            restore_message(false, &[restored(queue, Outcome::Lost)]),
            "Resumed on a fresh machine. Lost: `queue`, rerun if needed."
        );
    }

    #[test]
    fn probe_parsing_and_cwd_resolution() {
        assert!(matches!(probe("5173"), Probe::Port(5173)));
        let url = |host: &str, port: u16| Probe::Url { host: host.into(), port };
        assert_eq!(probe("http://127.0.0.1:9000/x"), url("127.0.0.1", 9000));
        assert_eq!(probe("https://example.com"), url("example.com", 443));
        assert_eq!(probe("http://localhost/"), url("localhost", 80));
        assert_eq!(resolve_cwd(Path::new("/w"), Some("web")), PathBuf::from("/w/web"));
        assert_eq!(resolve_cwd(Path::new("/w"), Some("/tmp/x")), PathBuf::from("/tmp/x"));
        assert_eq!(resolve_cwd(Path::new("/w"), None), PathBuf::from("/w"));
    }

    #[test]
    fn start_parses_flags_and_refuses_values_and_bad_names() {
        let args = argv("web --ready 5173 --env PORT --timeout 30 -- npm run dev".split_whitespace());
        let spec = parse_start(&args).unwrap();
        assert_eq!(spec.name, "web");
        assert_eq!(spec.ready.as_deref(), Some("5173"));
        assert_eq!(spec.env, ["PORT"]);
        assert_eq!(
            (spec.timeout_secs, spec.restart, spec.source.as_str()),
            (30, true, "registered")
        );
        assert_eq!(spec.cmd, "'npm' 'run' 'dev'");
        let error = parse_start(&argv(["web", "--env", "PORT=3000", "--", "npm", "run", "dev"])).unwrap_err();
        assert!(error.contains("bare variable NAME"), "{error}");
        let long = "a".repeat(65);
        for bad in [
            vec!["../evil", "--", "sh"],
            vec![long.as_str(), "--", "sh"],
            vec!["web"],
            vec!["web", "--"],
        ] {
            assert!(parse_start(&argv(bad.iter().copied())).is_err(), "{bad:?}");
        }
    }

    /// The no-secrets rule end to end: the process env carries a secret-looking variable, and the
    /// record `svc start` writes still only ever names it.
    #[tokio::test]
    async fn svc_start_writes_a_record_without_env_values() {
        let dir = std::env::temp_dir().join(format!("colonizer-agentd-svc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let secret = "COLONIZER_AGENTD_SVC_TEST_SECRET";
        // SAFETY: test-only, uniquely named variables; removed again right after the call.
        unsafe {
            std::env::set_var(SERVICES_DIR_VAR, &dir);
            std::env::set_var(secret, "sk-ant-VERY-secret-value")
        }
        // `--cwd` lands verbatim in the record; an absolute path into the scratch dir keeps the
        // spawn's chdir satisfiable.
        let cwd = dir.join("web");
        std::fs::create_dir_all(&cwd).unwrap();
        let args = format!(
            "web --ready 5173 --cwd {} --env {secret} --timeout 30 -- npm run dev",
            cwd.display()
        );
        let started = svc_start(&argv(args.split_whitespace())).await;
        unsafe {
            std::env::remove_var(SERVICES_DIR_VAR);
            std::env::remove_var(secret)
        }
        started.unwrap();
        let written = std::fs::read_to_string(dir.join("web.json")).unwrap();
        let record: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(record["name"], "web");
        assert_eq!(record["cmd"], "'npm' 'run' 'dev'");
        assert_eq!(record["env"], json!([secret]), "the name is recorded");
        assert!(!written.contains("sk-ant-VERY-secret-value"), "the value never is: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The relaunch path against a real listener: a python http.server is reported ready, and a
    /// port nobody answers reports not ready inside its timeout.
    #[tokio::test]
    async fn relaunch_waits_for_a_real_service_and_reports_a_dead_one() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut live = spec(
            "web",
            &format!("exec python3 -m http.server {port} --bind 127.0.0.1"),
            Some(&port.to_string()),
        );
        live.timeout_secs = 15;
        let mut dead = spec("api", "exec true", Some("1")); // nothing listens on 1
        dead.timeout_secs = 1; // a refused connect must time out inside a second, not hang
        let restore = Restore {
            suspended: true,
            services: vec![live, dead],
        };
        let results = run(&restore, &std::env::temp_dir(), &BTreeMap::new()).await;
        // The relaunch is detached on purpose, so kill it before this test ends.
        let _ = std::process::Command::new("pkill")
            .arg("-f")
            .arg(format!("http.server {port}"))
            .status();
        let message = restore_message(true, &results);
        assert!(
            message.contains(&format!("Restarted: `web` on :{port} (ready in")),
            "{message}"
        );
        assert!(
            message.contains("Not ready: `api` (no answer on :1 after 1 s, log /tmp/colonizer-svc-api.log)"),
            "{message}"
        );
    }
}
