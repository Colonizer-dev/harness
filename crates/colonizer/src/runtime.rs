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

/// The bound on one host measurement that spawns a subprocess — the `hostname` fallback where there
/// is no `/proc`, and `df` for the data disk. Same reasoning as [`TOOL_PROBE_TIMEOUT`]: a wedged
/// mount must not hold the poll, and only its own field degrades when it does.
const HOST_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// Additive display info about the host OS. `platform` stays the supported/unsupported gate;
    /// `os` only says what the machine is running and is always present.
    pub os: Os,
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

/// The `os` object of `GET /api/status`: what host OS this machine runs, for display. `platform`
/// stays the gate — supported or not — and `os` only adds colour, always present, never guessed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Os {
    /// The mapped vendor: the os-release `ID` against the known-distro map, its `ID_LIKE` tried
    /// as the fallback, "apple" on macOS, "linux" for a distro the map does not know, "unknown"
    /// off every OS that is neither Linux nor macOS.
    pub vendor: String,
    /// The display name: `NAME` from os-release, "macOS" on macOS, the raw os string elsewhere,
    /// "Linux" on Linux when the os-release file is missing or unreadable.
    pub name: String,
    /// `VERSION_ID` on Linux, `sw_vers -productVersion` on macOS, `null` when not found.
    pub version: Option<String>,
    /// The raw os-release `ID` value, e.g. `linuxmint` for a distro the map does not know; Linux
    /// only, `null` elsewhere.
    pub id: Option<String>,
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
    let (git, gh, host, os) = tokio::join!(
        probe_tool("git"),
        probe_tool("gh"),
        host_bin(app, HOST_BIN_TIMEOUT),
        probe_os()
    );
    Runtime {
        platform: telemetry::platform(),
        os,
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

/// The `host` object of `GET /api/status`: what kind of machine this walk lives on — its name and
/// size — and how full it is right now. These field names are the JSON contract the web UI is
/// written against. Every measurement that fails is omitted rather than faked as zero: a Mac has no
/// `/proc`, so `memory_total_bytes`, `load` and `uptime_secs` are simply absent there, and a disk
/// that cannot be read drops all three disk numbers.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Host {
    /// Stable per install: persisted in `<config_dir>/host_id`, generated once and kept, so the host
    /// panel shows the same id across restarts. Telemetry's `install_id` is ephemeral by design —
    /// it is forgotten when the live map is switched off — so this panel keys on its own file. A
    /// UUID keeps the shape per-host: a second machine can be aggregated later without a rewrite
    /// (issue #205).
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_used_bytes: Option<u64>,
    /// The 1, 5 and 15 minute load averages, as `/proc/loadavg` reports them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_used_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_free_bytes: Option<u64>,
    /// When the probe ran, so a "check again" can tell freshness without trusting its own watch.
    pub checked_at: chrono::DateTime<chrono::Utc>,
}

/// The last host probe, cached on `App` the same 10 s as the runtime probe: every open tab polls
/// `/api/status` every 30 s, and re-reading `/proc` and spawning `df` on each poll is the same
/// waste the runtime cache exists to avoid.
#[derive(Clone)]
pub struct HostCached {
    probed_at: Instant,
    value: Host,
}

/// The `host` object of `GET /api/status`: from the cache when it is warm, re-probed when it is
/// cold, stale or when `fresh` asks. Mirrors [`status_runtime`]'s cache.
pub async fn status_host(app: &App, fresh: bool) -> Host {
    host_cached(app, fresh, || probe_host(app)).await
}

/// The host half of [`cached`]: the same decision, keyed on `App.host_cache`, so concurrent polls
/// share one probe instead of stacking several.
async fn host_cached<C, Fut>(app: &App, fresh: bool, mut run: C) -> Host
where
    C: FnMut() -> Fut,
    Fut: Future<Output = Host>,
{
    let mut cache = app.host_cache.lock().await;
    if !fresh
        && let Some(hit) = cache.as_ref()
        && hit.probed_at.elapsed() < RUNTIME_CACHE_TTL
    {
        return hit.value.clone();
    }
    let value = run().await;
    *cache = Some(HostCached {
        probed_at: Instant::now(),
        value: value.clone(),
    });
    value
}

/// Measures what kind of machine this is and how full it is. The `/proc` reads and the two bounded
/// subprocesses are independent, so they race; each field degrades on its own. A measurement that
/// fails is `None` — and so omitted from the JSON — never a fabricated zero.
pub async fn probe_host(app: &App) -> Host {
    let (hostname, disk, darwin) = tokio::join!(probe_hostname(), probe_df(&app.cfg.data_dir), probe_darwin());
    let (memory_total, memory_used) = read_proc("/proc/meminfo")
        .map(|text| meminfo_total_used(&text))
        .unwrap_or((darwin.memory_total, darwin.memory_used));
    Host {
        id: host_id(app),
        hostname,
        cpu_cores: std::thread::available_parallelism().ok().map(|n| n.get()),
        memory_total_bytes: memory_total,
        memory_used_bytes: memory_used,
        load: read_proc("/proc/loadavg").and_then(|text| loadavg(&text)).or(darwin.load),
        uptime_secs: read_proc("/proc/uptime")
            .and_then(|text| uptime_secs(&text))
            .or(darwin.uptime_secs),
        disk_total_bytes: disk.0,
        disk_used_bytes: disk.1,
        disk_free_bytes: disk.2,
        checked_at: chrono::Utc::now(),
    }
}

/// The host's name: `/proc/sys/kernel/hostname` on Linux, which needs no subprocess; a bounded
/// `hostname` exec elsewhere. Trimmed to the name; a failure is `None`. Shared with the
/// cross-mothership issue claims, which name the host in their comment.
pub(crate) async fn probe_hostname() -> Option<String> {
    if std::env::consts::OS == "linux" {
        return read_proc("/proc/sys/kernel/hostname").map(|s| s.trim().to_string());
    }
    let mut cmd = Command::new("hostname");
    match tokio::time::timeout(HOST_PROBE_TIMEOUT, util::exec(&mut cmd)).await {
        Ok(Ok(name)) => {
            let name = name.trim().to_string();
            if name.is_empty() { None } else { Some(name) }
        }
        _ => None,
    }
}

/// macOS's answers to what `/proc` gives Linux: load, memory and uptime. Every field is `None`
/// on other systems, or wherever the bounded `sysctl` / `vm_stat` exec fails or does not parse.
#[derive(Debug, Default, PartialEq)]
struct DarwinHost {
    memory_total: Option<u64>,
    memory_used: Option<u64>,
    load: Option<[f64; 3]>,
    uptime_secs: Option<u64>,
}

async fn probe_darwin() -> DarwinHost {
    if std::env::consts::OS != "macos" {
        return DarwinHost::default();
    }
    let run = |program: &'static str, args: &'static [&'static str]| async move {
        let mut cmd = Command::new(program);
        cmd.args(args);
        match tokio::time::timeout(HOST_PROBE_TIMEOUT, util::exec(&mut cmd)).await {
            Ok(Ok(out)) => Some(out),
            _ => None,
        }
    };
    let (sysctl, vm) = tokio::join!(
        run("sysctl", &["-n", "vm.loadavg", "hw.memsize", "kern.boottime"]),
        run("vm_stat", &[])
    );
    let now = chrono::Utc::now().timestamp();
    darwin_host(sysctl.as_deref().unwrap_or(""), vm.as_deref().unwrap_or(""), now)
}

/// The parse half of [`probe_darwin`]. `sysctl -n vm.loadavg hw.memsize kern.boottime` prints one
/// line each: `{ 1.2 1.4 1.6 }`, a byte count, and `{ sec = 1790220675, usec = … } <date>`.
/// Memory in use is what Activity Monitor counts as used: active + wired + compressor pages.
fn darwin_host(sysctl: &str, vm_stat: &str, now: i64) -> DarwinHost {
    let mut lines = sysctl.lines();
    let load = lines
        .next()
        .and_then(|l| loadavg(l.trim().trim_start_matches('{').trim_end_matches('}')));
    let memory_total = lines.next().and_then(|l| l.trim().parse::<u64>().ok());
    let uptime_secs = lines
        .next()
        .and_then(|l| l.split("sec = ").nth(1))
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|sec| sec.parse::<i64>().ok())
        .and_then(|boot| u64::try_from(now - boot).ok());
    let page = vm_stat
        .lines()
        .next()
        .and_then(|l| l.split("page size of ").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse::<u64>().ok());
    let pages = |name: &str| {
        vm_stat
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|rest| rest.trim().trim_end_matches('.').parse::<u64>().ok())
    };
    let memory_used = match (
        page,
        pages("Pages active:"),
        pages("Pages wired down:"),
        pages("Pages occupied by compressor:"),
    ) {
        (Some(page), Some(active), Some(wired), Some(compressed)) => {
            let used = (active + wired + compressed) * page;
            // Accounting that does not add up is not shipped as a number.
            memory_total.filter(|total| used <= *total).map(|_| used)
        }
        _ => None,
    };
    DarwinHost {
        memory_total,
        memory_used,
        load,
        uptime_secs,
    }
}

/// One `/proc` read, `None` where `/proc` does not exist (macOS) or the file cannot be read. A
/// missing figure is later omitted from the JSON, never replaced with a zero.
fn read_proc(path: &str) -> Option<String> {
    if std::env::consts::OS != "linux" {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Stable per-install host id, from `<config_dir>/host_id`. Created on first call, persisted with
/// the private write helper so the file is 0600 like the other local state, and read back on every
/// later call so the id cannot drift within one install. A failed persist is ignored: the id still
/// answers this run, and the next call retries it.
pub fn host_id(app: &App) -> String {
    let path = app.cfg.config_dir.join("host_id");
    if let Some(existing) = util::read_trimmed(&path) {
        return existing;
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = util::write_private(&path, id.as_bytes());
    id
}

/// The data disk's `(total, used, free)` bytes, from `df -kP <dir>` under [`HOST_PROBE_TIMEOUT`].
/// Everything is 1024-byte blocks, so the parse scales the three numbers to bytes.
pub(crate) async fn probe_df(dir: &std::path::Path) -> (Option<u64>, Option<u64>, Option<u64>) {
    let mut cmd = Command::new("df");
    cmd.arg("-kP").arg(dir);
    match tokio::time::timeout(HOST_PROBE_TIMEOUT, util::exec(&mut cmd)).await {
        Ok(Ok(output)) => df_bytes(&output),
        _ => (None, None, None),
    }
}

/// The parse half of `df -kP`, sealed from the exec so it is testable without a real disk. The
/// second line holds the data: fs, 1024-blocks, used, available, capacity%, mount point. Wrapped or
/// odd output yields three `None`s, not guesses.
pub(crate) fn df_bytes(text: &str) -> (Option<u64>, Option<u64>, Option<u64>) {
    let fields: Vec<&str> = text
        .lines()
        .nth(1)
        .map(|line| line.split_whitespace().collect())
        .unwrap_or_default();
    let bytes = |i: usize| fields.get(i).and_then(|w| w.parse::<u64>().ok()).map(|v| v * 1024);
    (bytes(1), bytes(2), bytes(3))
}

/// `/proc/meminfo` in kB as `(total, used)` bytes, where used is total minus `MemAvailable`. Both
/// figures must parse and available must not exceed total, or the pair is `None` — memory
/// accounting that does not add up is not shipped as a number. The kB the file reports are scaled
/// to bytes.
fn meminfo_total_used(text: &str) -> (Option<u64>, Option<u64>) {
    let mut total_kb = None;
    let mut available_kb = None;
    for line in text.lines().take(32) {
        let kvalue = |prefix: &str| {
            line.strip_prefix(prefix)
                .and_then(|rest| rest.split_whitespace().next().and_then(|w| w.parse::<u64>().ok()))
        };
        if let Some(kb) = kvalue("MemTotal:") {
            total_kb = Some(kb);
        } else if let Some(kb) = kvalue("MemAvailable:") {
            available_kb = Some(kb);
        }
    }
    let (Some(total_kb), Some(available_kb)) = (total_kb, available_kb) else {
        return (None, None);
    };
    if available_kb > total_kb {
        return (None, None);
    }
    (Some(total_kb * 1024), Some((total_kb - available_kb) * 1024))
}

/// `/proc/loadavg`'s first three floats — the 1, 5 and 15 minute load averages. Anything that does
/// not parse as three numbers is `None` (and omitted), never a partial triple.
fn loadavg(text: &str) -> Option<[f64; 3]> {
    let mut words = text.split_whitespace();
    let a: f64 = words.next()?.parse().ok()?;
    let b: f64 = words.next()?.parse().ok()?;
    let c: f64 = words.next()?.parse().ok()?;
    Some([a, b, c])
}

/// `/proc/uptime`'s first float, as whole seconds.
fn uptime_secs(text: &str) -> Option<u64> {
    let secs: f64 = text.split_whitespace().next()?.parse().ok()?;
    Some(secs as u64)
}

/// The `host` object of `GET /api/status`, with the colony-admission numbers this host's operator
/// cares about set next to it. `kvm_ok` is omitted entirely where there is no `/dev/kvm` to check
/// (non-Linux), matching how `runtime.kvm` is `null` there.
pub fn host_json(host: &Host, kvm: Option<&Kvm>, microvms_live: usize, microvms_ceiling: u64) -> serde_json::Value {
    let mut value = serde_json::to_value(host).expect("Host serialises: its fields are all plain data");
    value["microvms_live"] = serde_json::Value::from(microvms_live);
    value["microvms_ceiling"] = serde_json::Value::from(microvms_ceiling);
    if let Some(kvm) = kvm {
        value["kvm_ok"] = serde_json::Value::from(kvm.ok);
    }
    value
}

/// The os answer as a pure function of what the probe would find, so the mapping and the
/// os-release parsing are testable without a real `/etc/os-release` or `sw_vers`. Never guesses:
/// a distro the map does not know stays vendor `linux` with its raw `id` — we know it is Linux,
/// just not which distro — and an OS beyond Linux and macOS is vendor `unknown` with the raw os
/// string, name only.
fn os_for(os: &str, os_release: Option<&str>, product_version: Option<&str>) -> Os {
    if os == "linux" {
        return match os_release {
            Some(body) => os_from_release(body),
            None => Os {
                vendor: "linux".into(),
                name: "Linux".into(),
                version: None,
                id: None,
            },
        };
    }
    if os == "macos" {
        let version = product_version
            .map(str::trim)
            .filter(|version| !version.is_empty())
            .map(str::to_owned);
        return Os {
            vendor: "apple".into(),
            name: "macOS".into(),
            version,
            id: None,
        };
    }
    Os {
        vendor: "unknown".into(),
        name: os.to_owned(),
        version: None,
        id: None,
    }
}

/// The linux answer from an os-release body: `NAME` for display, `VERSION_ID` for the version,
/// the raw `ID` kept for clients that want it, and the vendor from `ID` with each whitespace
/// token of `ID_LIKE` tried as the fallback, in order, so `ID_LIKE="ubuntu debian"` maps to
/// `ubuntu` and not the first token guessed at.
fn os_from_release(body: &str) -> Os {
    let id = os_release_field(body, "ID");
    let vendor = id
        .and_then(vendor_for)
        .or_else(|| os_release_field(body, "ID_LIKE").and_then(|like| like.split_whitespace().find_map(vendor_for)))
        .unwrap_or("linux");
    Os {
        vendor: vendor.to_owned(),
        name: os_release_field(body, "NAME").unwrap_or("Linux").to_owned(),
        version: os_release_field(body, "VERSION_ID").map(str::to_owned),
        id: id.map(str::to_owned),
    }
}

/// One field of an os-release body: `KEY=VALUE` lines only, the first occurrence of each key wins,
/// comments and blank lines skipped, double quotes and a trailing CRLF stripped, and an empty
/// value treated as absent so the caller falls back rather than reporting blank.
fn os_release_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .find_map(|line| {
            let (k, value) = line.split_once('=')?;
            if k != key {
                return None;
            }
            let value = value.trim();
            let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
            if value.is_empty() { None } else { Some(value) }
        })
}

/// The vendored string for one os-release `ID` (or `ID_LIKE` token), or None when the map does
/// not know it.
fn vendor_for(id: &str) -> Option<&'static str> {
    Some(match id {
        "ubuntu" => "ubuntu",
        "debian" => "debian",
        "fedora" => "fedora",
        "rhel" => "rhel",
        "centos" => "centos",
        "rocky" => "rocky",
        "almalinux" => "almalinux",
        "arch" => "arch",
        "omarchy" => "omarchy",
        "manjaro" => "manjaro",
        "endeavouros" => "endeavouros",
        "nixos" => "nixos",
        "alpine" => "alpine",
        _ => return id.starts_with("opensuse").then_some("opensuse"),
    })
}

/// What the probe finds on this machine: `/etc/os-release` read on Linux, `sw_vers -productVersion`
/// on macOS. Either can fail — an unreadable file or a missing command degrades only that field,
/// to vendor `linux` with name `Linux` on Linux, to no version on macOS — and anywhere else stays
/// `unknown`.
async fn probe_os() -> Os {
    let os = std::env::consts::OS;
    let os_release = if os == "linux" {
        tokio::fs::read_to_string("/etc/os-release").await.ok()
    } else {
        None
    };
    let product_version = if os == "macos" { sw_vers().await } else { None };
    os_for(os, os_release.as_deref(), product_version.as_deref())
}

/// `sw_vers -productVersion` stdout, accepted only when the command exits successfully and prints
/// a non-empty version; anything else is None, which degrades the version field alone.
async fn sw_vers() -> Option<String> {
    let mut cmd = Command::new("sw_vers");
    cmd.arg("-productVersion");
    let output = cmd.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout);
    let version = version.trim();
    if version.is_empty() { None } else { Some(version.to_owned()) }
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
            os: Os {
                vendor: "ubuntu".into(),
                name: "Ubuntu".into(),
                version: Some("24.04".into()),
                id: Some("ubuntu".into()),
            },
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
    fn every_known_distro_id_maps_to_its_vendor() {
        for (id, vendor) in [
            ("ubuntu", "ubuntu"),
            ("debian", "debian"),
            ("fedora", "fedora"),
            ("rhel", "rhel"),
            ("centos", "centos"),
            ("rocky", "rocky"),
            ("almalinux", "almalinux"),
            ("arch", "arch"),
            ("omarchy", "omarchy"),
            ("manjaro", "manjaro"),
            ("endeavouros", "endeavouros"),
            ("nixos", "nixos"),
            ("alpine", "alpine"),
            ("opensuse-leap", "opensuse"),
            ("opensuse-tumbleweed", "opensuse"),
        ] {
            let os = os_for("linux", Some(&format!("ID={id}\n")), None);
            assert_eq!(os.vendor, vendor, "os-release id {id}");
        }
    }

    #[test]
    fn an_unmapped_id_falls_back_to_id_like_in_order() {
        let mint = os_for("linux", Some("ID=linuxmint\nID_LIKE=ubuntu\nNAME=\"Linux Mint\"\n"), None);
        assert_eq!(mint.vendor, "ubuntu", "ID_LIKE maps when ID does not");
        let pop = os_for("linux", Some("ID=pop\nID_LIKE=\"ubuntu debian\"\nNAME=Pop\n"), None);
        assert_eq!(pop.vendor, "ubuntu", "the first ID_LIKE token wins");
        let something = os_for("linux", Some("ID=something\nID_LIKE=fedora\n"), None);
        assert_eq!(something.vendor, "fedora", "a single-token ID_LIKE works too");
    }

    #[test]
    fn an_unmapped_distro_is_vendor_linux_with_its_raw_id_kept() {
        let mint = os_for(
            "linux",
            Some("ID=linuxmint\nNAME=\"Linux Mint\"\nVERSION_ID=\"21.3\"\n"),
            None,
        );
        assert_eq!(mint.vendor, "linux", "vendor is linux, not a guess");
        assert_eq!(mint.id.as_deref(), Some("linuxmint"), "the raw id is kept");
        assert_eq!(mint.name, "Linux Mint");
        assert_eq!(mint.version.as_deref(), Some("21.3"));
        let flavour = os_for(
            "linux",
            Some("ID=flavour\nID_LIKE=zzz\nNAME=Flavour\nVERSION_ID=\"1\"\n"),
            None,
        );
        assert_eq!(flavour.vendor, "linux", "a missing and an unmapped ID_LIKE both fall back");
        assert_eq!(flavour.id.as_deref(), Some("flavour"));
        assert_eq!(flavour.name, "Flavour");
        assert_eq!(flavour.version.as_deref(), Some("1"));
    }

    #[test]
    fn os_release_values_are_unquoted_and_comments_blanks_and_crlf_are_handled() {
        let body = "# the distro marker\n\nNAME=\"Ubuntu\"\r\nVERSION_ID=\"24.04\"\r\nID=ubuntu\n";
        let os = os_for("linux", Some(body), None);
        assert_eq!(os.vendor, "ubuntu");
        assert_eq!(os.name, "Ubuntu", "surrounding quotes are stripped");
        assert_eq!(os.version.as_deref(), Some("24.04"));
        let quoted_id = os_for("linux", Some("ID=\"ubuntu\"\nNAME=\"Ubuntu\"\n"), None);
        assert_eq!(quoted_id.vendor, "ubuntu", "an unquoted and a quoted ID both map");
        assert_eq!(quoted_id.id.as_deref(), Some("ubuntu"));
    }

    #[test]
    fn linux_without_an_os_release_is_just_linux() {
        let os = os_for("linux", None, None);
        assert_eq!(os.vendor, "linux");
        assert_eq!(os.name, "Linux");
        assert_eq!(os.version, None);
        assert_eq!(os.id, None);
    }

    #[test]
    fn macos_reports_apple_with_the_product_version_or_none() {
        let os = os_for("macos", None, Some("14.5\n"));
        assert_eq!(os.vendor, "apple");
        assert_eq!(os.name, "macOS");
        assert_eq!(os.version.as_deref(), Some("14.5"), "the newline is trimmed");
        assert_eq!(os.id, None);
        let os = os_for("macos", None, None);
        assert_eq!(os.version, None, "no product version, no version field");
        let os = os_for("macos", None, Some("   \n"));
        assert_eq!(os.version, None, "blank output is treated as no version");
    }

    #[test]
    fn an_unknown_os_is_vendor_unknown_with_the_raw_name() {
        let os = os_for("freebsd", None, None);
        assert_eq!(os.vendor, "unknown");
        assert_eq!(os.name, "freebsd", "the raw os string, never a guess");
        assert_eq!(os.version, None);
        assert_eq!(os.id, None);
    }

    #[test]
    fn the_runtime_object_serialises_as_documented() {
        let value = serde_json::to_value(runtime_with(Some(Kvm { ok: true, error: None }))).unwrap();
        assert_eq!(value["platform"], "linux-x86_64");
        assert_eq!(
            value["os"],
            json!({"vendor": "ubuntu", "name": "Ubuntu", "version": "24.04", "id": "ubuntu"})
        );
        assert_eq!(value["kvm"], json!({"ok": true, "error": null}));
        assert_eq!(value["git"], json!({"ok": true, "version": "2.45.0", "error": null}));
        assert_eq!(value["gh"], json!({"ok": true, "version": "2.60.0", "error": null}));
        assert_eq!(value["host_claude_bin"], "/Users/me/.local/bin/claude");
        assert_eq!(value["host_claude_bin_error"], Value::Null);
        // version and id are Options that hold Nothing here, advertised as null so the web UI
        // skips nothing.
        let bare = Runtime {
            os: Os {
                vendor: "linux".into(),
                name: "Linux".into(),
                version: None,
                id: None,
            },
            ..runtime_with(None)
        };
        let value = serde_json::to_value(bare).unwrap();
        assert_eq!(
            value["os"],
            json!({"vendor": "linux", "name": "Linux", "version": null, "id": null})
        );
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

    /// A Host with every measurable field filled, so serialisation tests read only what they name.
    fn host_with() -> Host {
        Host {
            id: "5e1347a6-5f2e-4b8b-9c1a-0d4b7c8e9f10".into(),
            hostname: Some("picard".into()),
            cpu_cores: Some(8),
            memory_total_bytes: Some(17_179_869_184),
            memory_used_bytes: Some(5_368_709_120),
            load: Some([0.75, 0.31, 0.13]),
            uptime_secs: Some(43_200),
            disk_total_bytes: Some(246_177_628_160),
            disk_used_bytes: Some(109_088_034_816),
            disk_free_bytes: Some(124_592_496_640),
            checked_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn meminfo_parses_total_and_used_out_of_available() {
        let text =
            "MemTotal:       16242820 kB\nMemFree:        14047940 kB\nMemAvailable:   15574860 kB\nBuffers:         123456 kB\n";
        let (total, used) = meminfo_total_used(text);
        assert_eq!(total, Some(16_242_820 * 1024));
        assert_eq!(used, Some((16_242_820 - 15_574_860) * 1024));
    }

    #[test]
    fn meminfo_missing_available_yields_no_memory_figures() {
        let text = "MemTotal:       16242820 kB\nMemFree:        14047940 kB\n";
        assert_eq!(meminfo_total_used(text), (None, None), "no MemAvailable, no memory pair");
    }

    #[test]
    fn meminfo_available_beyond_total_is_not_reported() {
        let text = "MemTotal:       1000 kB\nMemAvailable:   2000 kB\n";
        assert_eq!(
            meminfo_total_used(text),
            (None, None),
            "fabricated figures are worse than none"
        );
    }

    #[test]
    fn darwin_host_reads_sysctl_and_vm_stat() {
        let sysctl = "{ 18.37 11.52 8.77 }\n38654705664\n{ sec = 1790220675, usec = 616546 } Thu Sep 24 11:31:15 2026\n";
        let vm = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:      15774.\nPages active:     630993.\nPages wired down:     316167.\nPages occupied by compressor:     704694.\n";
        let host = darwin_host(sysctl, vm, 1_790_220_775);
        assert_eq!(host.load, Some([18.37, 11.52, 8.77]));
        assert_eq!(host.memory_total, Some(38_654_705_664));
        assert_eq!(host.memory_used, Some((630_993 + 316_167 + 704_694) * 16_384));
        assert_eq!(host.uptime_secs, Some(100));
        assert_eq!(
            darwin_host("", "", 0),
            DarwinHost::default(),
            "no output is no figures, never zeros"
        );
    }

    #[test]
    fn loadavg_parses_the_first_three_floats() {
        assert_eq!(loadavg("0.75 0.31 0.13 2/834 51234\n"), Some([0.75, 0.31, 0.13]));
        assert_eq!(loadavg("garbage"), None, "not a number is None, never a partial triple");
    }

    #[test]
    fn uptime_parses_the_first_float_as_whole_seconds() {
        assert_eq!(uptime_secs("43200.10 99831.32\n"), Some(43_200));
        assert_eq!(uptime_secs("nonsense"), None);
    }

    #[test]
    fn df_parses_the_second_line_into_three_bytes_figures() {
        let text = "Filesystem     1024-blocks      Used Available Capacity Mounted on\n/dev/sda1        240407840  106531284 121672360      47% /\n";
        assert_eq!(
            df_bytes(text),
            (Some(240_407_840 * 1024), Some(106_531_284 * 1024), Some(121_672_360 * 1024))
        );
    }

    #[test]
    fn df_that_does_not_parse_yields_all_nones() {
        assert_eq!(
            df_bytes("Filesystem 1024-blocks Used Available Capacity Mounted on\nnot really a df line\n"),
            (None, None, None)
        );
        assert_eq!(df_bytes(""), (None, None, None), "no second line at all");
    }

    #[test]
    fn host_json_adds_the_microvm_counts_and_kvm() {
        let value = host_json(&host_with(), Some(&Kvm { ok: true, error: None }), 3, 4);
        assert_eq!(value["id"], host_with().id);
        assert_eq!(value["microvms_live"], 3);
        assert_eq!(value["microvms_ceiling"], 4);
        assert_eq!(value["kvm_ok"], true, "{value}");
        assert_eq!(value["hostname"], "picard");
        assert_eq!(value["load"], json!([0.75, 0.31, 0.13]));
        assert!(
            value["checked_at"].is_string(),
            "checked_at is the RFC 3339 string the contract sends: {value}"
        );
    }

    #[test]
    fn host_json_omits_kvm_ok_where_there_is_no_kvm() {
        let value = host_json(&host_with(), None, 0, 1);
        assert!(
            !value.as_object().unwrap().contains_key("kvm_ok"),
            "no /dev/kvm to check, so no such key: {value}"
        );
    }

    #[test]
    fn unmeasurable_host_fields_are_absent_not_null_or_zero() {
        let mut host = host_with();
        host.hostname = None;
        host.load = None;
        host.memory_used_bytes = None;
        let value = host_json(&host, None, 0, 1);
        for key in ["hostname", "load", "memory_used_bytes", "kvm_ok"] {
            assert!(
                !value.as_object().unwrap().contains_key(key),
                "{key} should be omitted: {value}"
            );
        }
        assert!(
            value.as_object().unwrap().contains_key("cpu_cores"),
            "a measured field stays: {value}"
        );
    }

    /// A probe that counts how often it ran, like `counting` but answering a `Host`.
    fn counting_host(runs: &Arc<AtomicUsize>) -> impl FnMut() -> std::future::Ready<Host> {
        let runs = runs.clone();
        move || {
            runs.fetch_add(1, Ordering::SeqCst);
            std::future::ready(host_with())
        }
    }

    #[tokio::test]
    async fn a_repeat_host_poll_within_the_ttl_answers_from_the_cache_without_reprobing() {
        let root = temp_root();
        let app = test_app(&root);
        let runs = Arc::new(AtomicUsize::new(0));
        let first = host_cached(&app, false, counting_host(&runs)).await;
        let second = host_cached(&app, false, counting_host(&runs)).await;
        assert_eq!(first, second);
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the second host poll inside the TTL must not re-probe"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_fresh_host_request_bypasses_the_cache_and_reprobes() {
        let root = temp_root();
        let app = test_app(&root);
        let runs = Arc::new(AtomicUsize::new(0));
        host_cached(&app, false, counting_host(&runs)).await;
        let fresh = host_cached(&app, true, counting_host(&runs)).await;
        let expected = host_with();
        // `checked_at` is a real clock reading, so it differs between two probes; everything else
        // the cache shares is identical.
        assert_eq!(
            Host {
                checked_at: fresh.checked_at,
                ..expected
            },
            fresh,
            "a bypassed cache still answers, with what the probe found"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            2,
            "?fresh=1 re-probes the host even though the cache is warm"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_host_id_is_created_once_and_persisted_in_the_config_dir() {
        let root = temp_root();
        let app = test_app(&root);
        let first = host_id(&app);
        let second = host_id(&app);
        assert_eq!(first, second, "the id is stable within one install");
        assert!(uuid::Uuid::parse_str(&first).is_ok(), "the id is a uuid, not a word: {first}");
        assert!(
            app.cfg.config_dir.join("host_id").exists(),
            "the id is persisted in the config dir"
        );
        // A second app over the same config dir keeps the id: a restart must not shuffle it.
        let again = test_app(&root);
        assert_eq!(host_id(&again), first, "the id survives a restart");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn probe_host_measures_this_machines_basics() {
        let root = temp_root();
        let app = test_app(&root);
        let host = probe_host(&app).await;
        assert!(!host.id.is_empty(), "the id is always answered, however the probe goes");
        if cfg!(target_os = "linux") {
            assert_eq!(host.cpu_cores, std::thread::available_parallelism().ok().map(|n| n.get()));
            assert!(host.cpu_cores.is_some(), "cpu_cores is measured on Linux: {host:?}");
            assert!(
                host.hostname.as_deref().is_some_and(|h| !h.is_empty()),
                "the hostname reads from /proc/sys/kernel/hostname on Linux: {host:?}"
            );
            assert!(host.memory_total_bytes.is_some(), "MemTotal is measured on Linux: {host:?}");
        } else {
            assert!(host.cpu_cores.is_some(), "available_parallelism answers anywhere");
        }
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
