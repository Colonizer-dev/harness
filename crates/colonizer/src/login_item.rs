//! Start the mothership at login: a macOS LaunchAgent or a Linux systemd user unit, installed by
//! `colonizer login-item enable` or the cockpit's Settings → Desktop switch.
//!
//! The agent restarts the mothership only when it crashes. A second copy (one started at login
//! while another already runs by hand, or the reverse) finds the port taken before it touches any
//! colony, says so, and — started by the agent — exits cleanly so the agent does not retry it.
//!
//! Disabling never stops a running mothership, and so never touches a live colony: it removes the
//! agent so the next login does not start one, and leaves the current process to the operator.

use crate::{ApiResult, Shared};
use anyhow::{Context, Result, anyhow, bail};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The launchd label and plist name on macOS.
pub const LABEL: &str = "dev.colonizer.mothership";
/// The systemd user unit on Linux.
pub const UNIT: &str = "colonizer.service";
/// Set in the agent's environment, so a mothership can tell it was started at login.
pub const MARKER_ENV: &str = "COLONIZER_LOGIN_ITEM";

/// Whether this process was started by the login agent.
pub fn started_as_login_item() -> bool {
    std::env::var(MARKER_ENV).is_ok_and(|v| v == "1")
}

/// What a second mothership prints when the port is taken.
pub fn already_running_message(bind: &str) -> String {
    format!(
        "another Colonizer mothership is already running on {bind} (started by hand, or at login). \
         Not starting a second one against the same data. Open the cockpit with `colonizer open`, \
         or stop the running one first."
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Linux,
    Unsupported,
}

pub fn platform() -> Platform {
    match std::env::consts::OS {
        "macos" => Platform::Macos,
        "linux" => Platform::Linux,
        _ => Platform::Unsupported,
    }
}

/// Where the agent's definition lives, and what it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub definition: PathBuf,
    pub binary: PathBuf,
    pub log: PathBuf,
}

/// `GET /api/login-item`, and `colonizer login-item status`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    pub platform: Platform,
    /// The plist or unit file exists.
    pub installed: bool,
    /// launchd / systemd has it loaded and enabled for the next login.
    pub enabled: bool,
    /// The pid the agent is running, if it is running one.
    pub pid: Option<u32>,
    pub definition: String,
    pub binary: String,
    pub log: String,
    /// Something the operator should know (e.g. linger on a headless Linux host).
    pub note: Option<String>,
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")
}

/// The stable launcher (`~/.local/bin/colonizer`, a link into the current app slot) when it exists,
/// so an update that swaps slots keeps working at the next login; else this binary.
fn binary(home: &Path) -> PathBuf {
    let stable = home.join(".local/bin/colonizer");
    if stable.exists() {
        return stable;
    }
    std::env::current_exe().unwrap_or(stable)
}

pub fn layout(data_dir: &Path) -> Result<Layout> {
    let home = home()?;
    let definition = match platform() {
        Platform::Macos => home.join("Library/LaunchAgents").join(format!("{LABEL}.plist")),
        Platform::Linux => {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            base.join("systemd/user").join(UNIT)
        }
        Platform::Unsupported => bail!("start at login is only available on macOS and Linux"),
    };
    Ok(Layout {
        definition,
        binary: binary(&home),
        log: data_dir.join("mothership.out"),
    })
}

/// The environment the agent runs with: this shell's `PATH` (launchd's default has no Homebrew,
/// `gh` or `msb`), every `COLONIZER_*` setting except anything that looks like a secret (the plist
/// and unit are world-readable files; secrets belong in the keychain), and the two markers.
pub fn agent_env(vars: impl IntoIterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vars
        .into_iter()
        .filter(|(k, _)| {
            let upper = k.to_ascii_uppercase();
            (k == "PATH" || k.starts_with("COLONIZER_"))
                && !["KEY", "TOKEN", "SECRET", "PASSWORD", "PASS"]
                    .iter()
                    .any(|w| upper.contains(w))
                && k != MARKER_ENV
                && k != "COLONIZER_NO_BROWSER"
        })
        .collect();
    out.push(("COLONIZER_NO_BROWSER".into(), "1".into()));
    out.push((MARKER_ENV.into(), "1".into()));
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The LaunchAgent: runs at load, restarts only after a crash (a clean exit — including the
/// "already running" one — is left alone), and appends to the mothership log.
pub fn render_plist(layout: &Layout, env: &[(String, String)]) -> String {
    let env_xml: String = env
        .iter()
        .map(|(k, v)| format!("\t\t<key>{}</key>\n\t\t<string>{}</string>\n", xml_escape(k), xml_escape(v)))
        .collect();
    let bin = xml_escape(&layout.binary.display().to_string());
    let log = xml_escape(&layout.log.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{bin}</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
{env_xml}	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ThrottleInterval</key>
	<integer>10</integer>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    )
}

/// systemd quotes: a value with spaces or quotes is wrapped and escaped.
fn systemd_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The user unit: starts with the user session, restarts only on failure, appends to the log.
pub fn render_unit(layout: &Layout, env: &[(String, String)]) -> String {
    let env_lines: String = env
        .iter()
        .map(|(k, v)| format!("Environment={}\n", systemd_quote(&format!("{k}={v}"))))
        .collect();
    format!(
        "[Unit]\nDescription=Colonizer mothership\nAfter=network-online.target\nWants=network-online.target\n\n\
         [Service]\nType=simple\nExecStart={}\n{env_lines}Restart=on-failure\nRestartSec=10\n\
         StandardOutput=append:{}\nStandardError=append:{}\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(&layout.binary.display().to_string()),
        layout.log.display(),
        layout.log.display(),
    )
}

/// The `pid = N` line of `launchctl print`.
pub fn launchctl_pid(print: &str) -> Option<u32> {
    print
        .lines()
        .find_map(|l| l.trim().strip_prefix("pid = "))
        .and_then(|p| p.trim().parse().ok())
}

fn uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".into())
}

fn run(program: &str, args: &[&str]) -> (bool, String) {
    match Command::new(program).args(args).output() {
        Ok(o) => (
            o.status.success(),
            format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
        ),
        Err(e) => (false, e.to_string()),
    }
}

fn write_definition(path: &Path, body: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

pub fn status(data_dir: &Path) -> Result<Status> {
    let layout = layout(data_dir)?;
    let installed = layout.definition.exists();
    let (enabled, pid, note) = match platform() {
        Platform::Macos => {
            let domain = format!("gui/{}/{LABEL}", uid());
            let (loaded, out) = run("launchctl", &["print", &domain]);
            (installed && loaded, if loaded { launchctl_pid(&out) } else { None }, None)
        }
        Platform::Linux => {
            let (on, _) = run("systemctl", &["--user", "is-enabled", "--quiet", UNIT]);
            let (_, pid) = run("systemctl", &["--user", "show", "-p", "MainPID", "--value", UNIT]);
            let pid = pid.trim().parse::<u32>().ok().filter(|p| *p > 0);
            let (linger, out) = run("loginctl", &["show-user", &whoami(), "-p", "Linger", "--value"]);
            let note = (on && !(linger && out.trim() == "yes")).then(|| {
                "on a headless host, run `loginctl enable-linger` so it starts at boot, not only when you log in".to_string()
            });
            (installed && on, pid, note)
        }
        Platform::Unsupported => (false, None, None),
    };
    Ok(Status {
        platform: platform(),
        installed,
        enabled,
        pid,
        definition: layout.definition.display().to_string(),
        binary: layout.binary.display().to_string(),
        log: layout.log.display().to_string(),
        note,
    })
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_default()
}

/// Installs and loads the agent. Idempotent. The agent starts a mothership right away; when one is
/// already running, that copy sees the port taken and exits cleanly, so nothing runs twice.
pub fn enable(data_dir: &Path) -> Result<Status> {
    let layout = layout(data_dir)?;
    let env = agent_env(std::env::vars());
    match platform() {
        Platform::Macos => {
            write_definition(&layout.definition, &render_plist(&layout, &env))?;
            let domain = format!("gui/{}", uid());
            let target = format!("{domain}/{LABEL}");
            let _ = run("launchctl", &["enable", &target]);
            let (loaded, _) = run("launchctl", &["print", &target]);
            if !loaded {
                let plist = layout.definition.display().to_string();
                let (ok, out) = run("launchctl", &["bootstrap", &domain, &plist]);
                if !ok {
                    // Older launchctl: the legacy verb.
                    let (ok, out2) = run("launchctl", &["load", "-w", &plist]);
                    if !ok {
                        bail!("launchctl could not load {plist}: {} {}", out.trim(), out2.trim());
                    }
                }
            }
        }
        Platform::Linux => {
            write_definition(&layout.definition, &render_unit(&layout, &env))?;
            let _ = run("systemctl", &["--user", "daemon-reload"]);
            let (ok, out) = run("systemctl", &["--user", "enable", "--now", UNIT]);
            if !ok {
                bail!("systemctl --user enable {UNIT} failed: {}", out.trim());
            }
        }
        Platform::Unsupported => bail!("start at login is only available on macOS and Linux"),
    }
    status(data_dir)
}

/// Removes the agent so the next login does not start a mothership. The running mothership is left
/// alone — it keeps serving and its colonies keep running until you stop it.
pub fn disable(data_dir: &Path) -> Result<Status> {
    let layout = layout(data_dir)?;
    match platform() {
        Platform::Macos => {
            // `disable` persists "do not load at login" without stopping the loaded job (a
            // `bootout` would SIGTERM a mothership launchd started).
            let _ = run("launchctl", &["disable", &format!("gui/{}/{LABEL}", uid())]);
        }
        Platform::Linux => {
            // Without --now: the unit stays running until stopped.
            let _ = run("systemctl", &["--user", "disable", UNIT]);
        }
        Platform::Unsupported => bail!("start at login is only available on macOS and Linux"),
    }
    if layout.definition.exists() {
        std::fs::remove_file(&layout.definition).with_context(|| format!("cannot remove {}", layout.definition.display()))?;
    }
    let mut s = status(data_dir)?;
    s.enabled = false;
    Ok(s)
}

/// `colonizer login-item enable|disable|status`.
pub fn command(action: &str, data_dir: &Path) -> Result<()> {
    let s = match action {
        "enable" => enable(data_dir)?,
        "disable" => disable(data_dir)?,
        "status" => status(data_dir)?,
        other => bail!("unknown login-item command: {other} (enable, disable or status)"),
    };
    let state = if s.enabled { "on" } else { "off" };
    println!("start at login: {state}");
    println!(
        "  definition: {} ({})",
        s.definition,
        if s.installed { "installed" } else { "not installed" }
    );
    println!("  runs:       {}", s.binary);
    println!("  log:        {}", s.log);
    match s.pid {
        Some(pid) => println!("  running:    pid {pid} (started by the login agent)"),
        None => println!("  running:    not by the login agent"),
    }
    if action == "disable" {
        println!("  the running mothership, if any, keeps running; stop it yourself when you want to");
    }
    if let Some(note) = &s.note {
        println!("  note:       {note}");
    }
    Ok(())
}

/// `GET /api/login-item`: whether the mothership starts at login.
async fn login_item_status(State(app): State<Shared>) -> ApiResult<Status> {
    let data_dir = app.cfg.data_dir.clone();
    let status = tokio::task::spawn_blocking(move || status(&data_dir))
        .await
        .map_err(|e| anyhow!("{e}"))??;
    Ok(Json(status))
}

#[derive(Deserialize)]
struct LoginItemRequest {
    enabled: bool,
}

/// `POST /api/login-item {enabled}`: the Settings switch; the same code as `colonizer login-item`.
async fn login_item_set(State(app): State<Shared>, Json(req): Json<LoginItemRequest>) -> ApiResult<Status> {
    let data_dir = app.cfg.data_dir.clone();
    let status = tokio::task::spawn_blocking(move || if req.enabled { enable(&data_dir) } else { disable(&data_dir) })
        .await
        .map_err(|e| anyhow!("{e}"))??;
    Ok(Json(status))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/login-item", routing::get(login_item_status).post(login_item_set))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout {
            definition: PathBuf::from("/Users/a/Library/LaunchAgents/dev.colonizer.mothership.plist"),
            binary: PathBuf::from("/Users/a/.local/bin/colonizer"),
            log: PathBuf::from("/Users/a/.local/share/colonizer/mothership.out"),
        }
    }

    #[test]
    fn the_agent_env_keeps_path_and_settings_but_never_secrets() {
        let env = agent_env([
            ("PATH".into(), "/opt/homebrew/bin:/usr/bin".into()),
            ("COLONIZER_BIND".into(), "127.0.0.1:7878".into()),
            ("COLONIZER_MASTER_KEY".into(), "s3cret".into()),
            ("COLONIZER_API_TOKEN".into(), "t".into()),
            ("HOME".into(), "/Users/a".into()),
            ("COLONIZER_NO_BROWSER".into(), "0".into()),
        ]);
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            ["COLONIZER_BIND", "COLONIZER_LOGIN_ITEM", "COLONIZER_NO_BROWSER", "PATH"]
        );
        assert!(env.contains(&("COLONIZER_NO_BROWSER".into(), "1".into())));
    }

    #[test]
    fn the_plist_restarts_only_on_a_crash_and_logs_to_the_mothership_log() {
        let plist = render_plist(&layout(), &[("PATH".into(), "/usr/bin&x".into())]);
        assert!(plist.contains("<string>dev.colonizer.mothership</string>"));
        assert!(plist.contains("<string>/Users/a/.local/bin/colonizer</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(plist.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(plist.contains("<string>/usr/bin&amp;x</string>"), "values are XML-escaped");
        assert_eq!(plist.matches("mothership.out").count(), 2);
    }

    #[test]
    fn the_unit_restarts_on_failure_and_starts_with_the_user_session() {
        let unit = render_unit(&layout(), &[("COLONIZER_BIND".into(), "127.0.0.1:7878".into())]);
        assert!(unit.contains("ExecStart=\"/Users/a/.local/bin/colonizer\""));
        assert!(unit.contains("Environment=\"COLONIZER_BIND=127.0.0.1:7878\""));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("StandardOutput=append:/Users/a/.local/share/colonizer/mothership.out"));
    }

    #[test]
    fn launchctl_print_yields_the_pid() {
        let out = "gui/501/dev.colonizer.mothership = {\n\tactive count = 1\n\tpid = 4457\n\tstate = running\n}";
        assert_eq!(launchctl_pid(out), Some(4457));
        assert_eq!(launchctl_pid("state = not running"), None);
    }

    #[test]
    fn a_second_mothership_is_told_why_it_stops() {
        let msg = already_running_message("127.0.0.1:7878");
        assert!(msg.contains("already running on 127.0.0.1:7878") && msg.contains("colonizer open"));
    }
}
