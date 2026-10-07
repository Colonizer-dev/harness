//! Automatic colony sizing and admission (issue #1141).
//!
//! With the sandbox preset `auto` and no fixed `max_parallel` in Settings, the harness sizes each
//! colony from the host (cores and RAM, less a reserve for the host itself) and admits colonies from
//! what the machine has free RIGHT NOW: `MemAvailable` on Linux, free + inactive pages on macOS, and
//! the 1-minute load average against the cores. There is no fixed parallel number in this mode, only
//! the safety cap `auto_max_parallel` (default 32).
//!
//! The admission rule is a pure function of a [`HostSample`], so tests feed it series of fake samples
//! through the [`HostProbe`] trait. It reaches the queue as one number: [`Limit::max_parallel`] is
//! "colonies running now + colonies that fit now", which `queue::has_room` already knows how to
//! count. Admission also checks COMMITTED resources (issue #1158): microVMs allocate memory lazily, so
//! live free memory alone lets a burst of colonies through that the host cannot hold once they work.
//! A host that is low on memory therefore stops NEW admissions only — the number never
//! drops below what is running, and nothing is ever stopped for it.
//!
//! A fixed `max_parallel` in Settings, or any preset other than `auto`, is today's static behaviour,
//! untouched. So is an install whose host cannot be measured at all.

use crate::{
    config::{ModulesConfig, setting_u64},
    modules::schema_for,
    orgs,
    sessions::{Session, SessionStatus},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

pub const GIB: u64 = 1 << 30;

/// The safety cap on colonies in auto mode when Settings does not name one.
pub const DEFAULT_AUTO_MAX_PARALLEL: u64 = 32;

/// vCPUs the host keeps for itself in auto mode.
const RESERVE_CPUS: u64 = 2;
/// The floor of the memory reserve on a host of 32 GiB or more, in GiB; the reserve there is the
/// larger of this and a tenth of RAM. Below 32 GiB it is a fifth of RAM, between 2 and 8 GiB.
const RESERVE_FLOOR_GIB: u64 = 8;
/// What a lone colony leaves the host when it has to be squeezed in, in GiB.
const SMALL_RESERVE_GIB: u64 = 1;
/// The smallest colony auto mode will boot, in GiB.
const MIN_COLONY_GIB: u64 = 2;
/// The most vCPUs one colony is given (the Settings schema's own bound).
const MAX_COLONY_CPUS: u64 = 64;
/// What one colony gets where the host cannot be measured: the `node` preset's size.
const FALLBACK_CPUS: u64 = 4;
const FALLBACK_MEMORY_GIB: u64 = 8;
/// Default share of RAM that committed colony sizes may use (`auto_overcommit`), and its bounds.
pub const DEFAULT_AUTO_OVERCOMMIT: f64 = 0.75;
const MIN_OVERCOMMIT: f64 = 0.5;
const MAX_OVERCOMMIT: f64 = 1.0;
/// vCPUs may be committed to this multiple of the cores.
const COMMIT_CPU_FACTOR: f64 = 1.5;
/// A probe that outlasts this counts as absent: the queue tick must never wait on it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// One reading of the host. A field that could not be measured is `None`, never a zero.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HostSample {
    pub cores: Option<usize>,
    pub memory_total: Option<u64>,
    /// Memory a new colony could take now: `MemAvailable` on Linux, free + inactive pages on macOS.
    pub memory_available: Option<u64>,
    /// The 1-minute load average.
    pub load1: Option<f64>,
}

impl HostSample {
    /// A reading with the impossible values dropped: a zero core count, total or available memory,
    /// and a negative or non-finite load are a probe that failed, not a host with nothing.
    pub fn sanitized(self) -> Self {
        Self {
            cores: self.cores.filter(|&n| n > 0),
            memory_total: self.memory_total.filter(|&n| n > 0),
            memory_available: self.memory_available.filter(|&n| n > 0),
            load1: self.load1.filter(|n| n.is_finite() && *n >= 0.0),
        }
    }
}

/// Where the live host numbers come from. The real one reads `/proc` or `vm_stat`; tests feed a
/// scripted series. Blocking on purpose: [`Capacity::sample`] runs it off the async threads.
pub trait HostProbe: Send + Sync {
    fn sample(&self) -> HostSample;
}

/// The machine this process runs on.
#[cfg_attr(test, allow(dead_code))]
pub struct SystemProbe;

impl HostProbe for SystemProbe {
    fn sample(&self) -> HostSample {
        let cores = std::thread::available_parallelism().ok().map(|n| n.get());
        if std::env::consts::OS == "linux" {
            let (memory_total, memory_available) = std::fs::read_to_string("/proc/meminfo")
                .map(|text| meminfo(&text))
                .unwrap_or_default();
            let load1 = std::fs::read_to_string("/proc/loadavg")
                .ok()
                .and_then(|text| text.split_whitespace().next().and_then(|w| w.parse().ok()));
            return HostSample {
                cores,
                memory_total,
                memory_available,
                load1,
            };
        }
        if std::env::consts::OS == "macos" {
            let sysctl = run("sysctl", &["-n", "hw.memsize", "vm.loadavg"]).unwrap_or_default();
            let mut lines = sysctl.lines();
            let memory_total = lines.next().and_then(|l| l.trim().parse::<u64>().ok());
            let load1 = lines.next().and_then(|l| {
                l.trim()
                    .trim_start_matches('{')
                    .split_whitespace()
                    .next()
                    .and_then(|w| w.parse().ok())
            });
            let memory_available = run("vm_stat", &[]).and_then(|text| vm_stat_available(&text));
            return HostSample {
                cores,
                memory_total,
                memory_available,
                load1,
            };
        }
        HostSample {
            cores,
            ..HostSample::default()
        }
    }
}

#[cfg_attr(test, allow(dead_code))]
fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `(MemTotal, MemAvailable)` in bytes from `/proc/meminfo`. Available above total is accounting that
/// does not add up, so the pair is dropped.
pub fn meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let kb = |prefix: &str| {
        text.lines().take(32).find_map(|line| {
            line.strip_prefix(prefix)
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|w| w.parse::<u64>().ok())
        })
    };
    match (kb("MemTotal:"), kb("MemAvailable:")) {
        (Some(total), Some(available)) if available <= total => (Some(total * 1024), Some(available * 1024)),
        _ => (None, None),
    }
}

/// Free + inactive pages of `vm_stat`, in bytes: what macOS can hand a new process without swapping.
pub fn vm_stat_available(text: &str) -> Option<u64> {
    let page = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    let pages = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|rest| rest.trim().trim_end_matches('.').parse::<u64>().ok())
    };
    Some((pages("Pages free:")? + pages("Pages inactive:")?) * page)
}

/// A probe that measures nothing: auto mode degrades to the static limit on it. What tests run on
/// unless they install a scripted one.
#[cfg_attr(not(test), allow(dead_code))]
pub struct NoProbe;

impl HostProbe for NoProbe {
    fn sample(&self) -> HostSample {
        HostSample::default()
    }
}

/// What one colony is given in auto mode, and the reserve it was derived from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoSize {
    pub cpus: u64,
    pub memory_gib: u64,
    /// How many colonies of this size the host holds when idle otherwise: the size was derived
    /// from it. Informational — admission is live, not this number.
    pub slots: u64,
    pub reserve_gib: u64,
    pub reserve_cpus: u64,
}

impl AutoSize {
    /// Sizes colonies from a host's totals. `cap` bounds the planned slot count.
    ///
    /// Below 32 GiB of RAM, `reserve = clamp(20% of RAM, 2, 8 GiB)`; from 32 GiB,
    /// `max(8 GiB, 10% of RAM)`. 2 vCPUs stay with the host. The colonies share the rest:
    /// `slots = clamp((RAM - reserve) / target, 1, cap)` with a target of 4 GiB up to 16 GiB of RAM,
    /// 6 up to 32, 8 under 48, 10 under 96 and 11 above; then `memory = (RAM - reserve) / slots` and
    /// `cpus = max(2, (cores - 2) / slots)`.
    pub fn for_host(cores: usize, memory_total: u64, cap: u64) -> Self {
        let ram = memory_total / GIB;
        let reserve_gib = if ram < 32 {
            (ram / 5).clamp(2, RESERVE_FLOOR_GIB)
        } else {
            (ram / 10).max(RESERVE_FLOOR_GIB)
        };
        let usable = ram.saturating_sub(reserve_gib);
        let target = match ram {
            0..=16 => 4,
            17..=32 => 6,
            33..=47 => 8,
            48..=95 => 10,
            _ => 11,
        };
        let slots = (usable / target).clamp(1, cap.max(1));
        let memory_gib = (usable / slots).max(2);
        let cpus = ((cores as u64).saturating_sub(RESERVE_CPUS) / slots).clamp(2, MAX_COLONY_CPUS);
        Self {
            cpus,
            memory_gib,
            slots,
            reserve_gib,
            reserve_cpus: RESERVE_CPUS,
        }
    }

    /// The size where the host is not known: what colonies boot with without auto.
    fn fallback() -> Self {
        Self {
            cpus: FALLBACK_CPUS,
            memory_gib: FALLBACK_MEMORY_GIB,
            slots: 1,
            reserve_gib: RESERVE_FLOOR_GIB,
            reserve_cpus: RESERVE_CPUS,
        }
    }

    /// The size of a colony that must start on this host right now: the planned size, squeezed to
    /// what is free less a small reserve (never under 2 GiB), on at most the cores there are.
    pub fn fit_to(self, sample: &HostSample) -> Self {
        let mut size = self;
        if let Some(available) = sample.memory_available {
            let room = (available / GIB).saturating_sub(SMALL_RESERVE_GIB);
            let fitted = size.memory_gib.min(room.max(MIN_COLONY_GIB));
            if fitted < size.memory_gib {
                size.slots = 1;
            }
            size.memory_gib = fitted;
        }
        if let Some(cores) = sample.cores {
            size.cpus = size.cpus.min((cores as u64).max(1));
        }
        size
    }

    pub fn from_sample(sample: &HostSample, cap: u64) -> Option<Self> {
        Some(Self::for_host(sample.cores?, sample.memory_total?, cap))
    }
}

/// What the next colony is waiting on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    Memory,
    Cpu,
    Cap,
}

impl WaitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Cpu => "cpu",
            Self::Cap => "cap",
        }
    }
}

/// What bounds the next admission most tightly. Wider than [`WaitReason`]: it also names the
/// committed-resource checks, and is reported even while colonies still fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limiter {
    Cap,
    /// Committed memory (colony sizes, overcommitted) against total RAM.
    MemoryCommit,
    /// Committed vCPUs (colony sizes, overcommitted) against the cores.
    CpuCommit,
    /// Live free memory.
    Free,
    /// Live load average.
    Load,
}

impl Limiter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cap => "cap",
            Self::MemoryCommit => "memory-commit",
            Self::CpuCommit => "cpu-commit",
            Self::Free => "free",
            Self::Load => "load",
        }
    }

    /// The coarser reason the cockpit already words ("memory", "cpu", "cap").
    pub fn wait_reason(self) -> WaitReason {
        match self {
            Self::Cap => WaitReason::Cap,
            Self::MemoryCommit | Self::Free => WaitReason::Memory,
            Self::CpuCommit | Self::Load => WaitReason::Cpu,
        }
    }
}

/// How many more colonies fit right now, and what stops the next one when none does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Admission {
    pub room_for: usize,
    pub waiting_reason: Option<WaitReason>,
    /// The tightest check, whether or not it has reached zero.
    pub limited_by: Limiter,
}

/// The admission rule. A colony starts only when ALL of these hold:
///
/// - the cap has room;
/// - committed memory fits: `colonies × memory × memory_factor + reserve ≤ total RAM`, counting every
///   colony holding a slot (`running`, booting ones included) at its full size. Free memory cannot
///   say this, because a microVM touches memory lazily: a just-booted colony costs 1-2 GB of an 11 GB
///   size, so `MemAvailable` stays high through a whole burst of admissions and then falls at once
///   when the colonies start building (issue #1158);
/// - committed vCPUs fit: `colonies × cpus ≤ cores × COMMIT_CPU_FACTOR` (CPU is cheap to oversubscribe;
///   the host's own share is held back by the load check);
/// - the live free memory, less the host reserve, holds its memory;
/// - the load leaves its vCPUs room below the cores less the reserve.
///
/// `booting` colonies have been admitted but have not taken their memory and CPU yet, so the live
/// checks charge them in full; without that, a burst of admissions in the seconds before the host
/// notices them would all pass. A measurement that is missing does not gate: the cap still does.
/// Running colonies are never evicted: past a limit the room is zero, not negative. When nothing is
/// running the first colony is always admitted (sized to fit by [`AutoSize::fit_to`]), whatever
/// memory and load say: a small host must not deadlock waiting for room it never has.
pub fn admission(
    size: &AutoSize,
    sample: &HostSample,
    running: usize,
    booting: usize,
    cap: usize,
    memory_factor: f64,
) -> Admission {
    let booting = booting as u64;
    let cap_room = cap.saturating_sub(running);
    let commit_memory_room = sample.memory_total.map(|total| {
        let usable = total.saturating_sub(size.reserve_gib * GIB) as f64;
        let per_colony = (size.memory_gib * GIB) as f64 * memory_factor;
        ((usable / per_colony + 1e-9).floor() as usize).saturating_sub(running)
    });
    let commit_cpu_room = sample.cores.map(|cores| {
        let usable = cores as f64 * COMMIT_CPU_FACTOR;
        ((usable / size.cpus as f64 + 1e-9).floor() as usize).saturating_sub(running)
    });
    let free_room = sample.memory_available.map(|available| {
        let committed = (booting * size.memory_gib + size.reserve_gib) * GIB;
        (available.saturating_sub(committed) / (size.memory_gib * GIB)) as usize
    });
    let load_room = sample.load1.zip(sample.cores).map(|(load, cores)| {
        let capacity = (cores as u64).saturating_sub(size.reserve_cpus) as f64;
        let headroom = capacity - load - (booting * size.cpus) as f64;
        (headroom.max(0.0) / size.cpus as f64).floor() as usize
    });
    // Ties go to the earlier entry: the cap, then the commits, then the live checks.
    let (limited_by, room_for) = [
        (Limiter::Cap, Some(cap_room)),
        (Limiter::MemoryCommit, commit_memory_room),
        (Limiter::CpuCommit, commit_cpu_room),
        (Limiter::Free, free_room),
        (Limiter::Load, load_room),
    ]
    .into_iter()
    .filter_map(|(limiter, room)| room.map(|r| (limiter, r)))
    .min_by_key(|&(_, room)| room)
    .unwrap_or((Limiter::Cap, cap_room));
    let room_for = if running == 0 && cap_room > 0 {
        room_for.max(1)
    } else {
        room_for
    };
    Admission {
        room_for,
        waiting_reason: (room_for == 0).then(|| limited_by.wait_reason()),
        limited_by,
    }
}

/// Whether the sandbox module is in auto mode: the preset is `auto`, and Settings holds no fixed
/// `max_parallel`. A number written there is the operator's word and keeps today's behaviour.
pub fn is_auto(modules: &ModulesConfig) -> bool {
    let schema = schema_for("sandbox", &modules.sandbox.provider, &[]);
    crate::config::setting_str(&modules.sandbox, &schema, "preset") == crate::presets::AUTO
        && !modules.sandbox.settings.contains_key("max_parallel")
}

/// The safety cap of auto mode.
pub fn auto_cap(modules: &ModulesConfig) -> u64 {
    let schema = schema_for("sandbox", &modules.sandbox.provider, &[]);
    setting_u64(&modules.sandbox, &schema, "auto_max_parallel").max(1)
}

/// The share of RAM committed colony sizes may reach in auto mode, clamped to 0.5-1.0.
pub fn auto_overcommit(modules: &ModulesConfig) -> f64 {
    let schema = schema_for("sandbox", &modules.sandbox.provider, &[]);
    let factor = crate::config::setting_f64(&modules.sandbox, &schema, "auto_overcommit");
    if factor.is_finite() && factor > 0.0 {
        factor.clamp(MIN_OVERCOMMIT, MAX_OVERCOMMIT)
    } else {
        DEFAULT_AUTO_OVERCOMMIT
    }
}

/// Everything the live picture holds, for `/api/status` and the cockpit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoState {
    pub size: AutoSize,
    pub cap: usize,
    pub running: usize,
    pub admission: Admission,
    pub sample: HostSample,
    /// The memory overcommit factor in force (the setting, clamped).
    pub overcommit: f64,
}

/// The parallel limit in force now, and, in auto mode, how it was reached.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limit {
    pub max_parallel: usize,
    pub auto: Option<AutoState>,
    /// Colonies holding a slot when this was computed.
    pub running: usize,
}

impl Limit {
    pub fn mode(&self) -> &'static str {
        if self.auto.is_some() { "auto" } else { "fixed" }
    }

    pub fn room_for(&self) -> usize {
        self.auto
            .map_or_else(|| self.max_parallel.saturating_sub(self.running), |a| a.admission.room_for)
    }

    pub fn waiting_reason(&self) -> Option<WaitReason> {
        match self.auto {
            Some(auto) => auto.admission.waiting_reason,
            None => (self.room_for() == 0).then_some(WaitReason::Cap),
        }
    }

    /// The `sandbox` keys `/api/status` adds: `mode`, the computed `size` (auto only), `room_for`,
    /// `waiting_reason`, and the live numbers the cockpit prints beside them.
    pub fn status_json(&self) -> Value {
        let mut value = json!({
            "mode": self.mode(),
            "room_for": self.room_for(),
            "waiting_reason": self.waiting_reason().map(WaitReason::as_str),
            "running": self.running,
        });
        if let Some(auto) = self.auto {
            value["size"] = json!({
                "cpus": auto.size.cpus,
                "memory_gb": auto.size.memory_gib,
                "slots": auto.size.slots,
                "reserve_gb": auto.size.reserve_gib,
                "reserve_cpus": auto.size.reserve_cpus,
            });
            // In auto mode the static settings are stale: the effective ceiling and the auto colony
            // size take their keys (the configured values stay under `configured_*`, see status.rs).
            value["max_parallel"] = json!(self.max_parallel);
            value["cpus"] = json!(auto.size.cpus);
            value["memory"] = json!(format!("{}G", auto.size.memory_gib));
            value["auto_max_parallel"] = json!(auto.cap);
            value["committed_gb"] = json!(auto.running as u64 * auto.size.memory_gib);
            value["overcommit"] = json!(auto.overcommit);
            value["limited_by"] = json!(auto.admission.limited_by.as_str());
            for (key, v) in [
                ("free_bytes", auto.sample.memory_available.map(|n| json!(n))),
                ("load", auto.sample.load1.map(|n| json!(n))),
                ("cpu_cores", auto.sample.cores.map(|n| json!(n))),
            ] {
                if let Some(v) = v {
                    value[key] = v;
                }
            }
        }
        value
    }
}

/// The limit for a given sample and the colonies now holding a slot: the pure core of [`limit`].
pub fn evaluate(modules: &ModulesConfig, sample: HostSample, sessions: &[Session]) -> Limit {
    let sample = sample.sanitized();
    let running = sessions.iter().filter(|s| s.holds_slot()).count();
    let fixed = Limit {
        max_parallel: orgs::global_max_parallel(modules) as usize,
        auto: None,
        running,
    };
    // An unmeasurable host (nothing but cores, or not even those) cannot be admitted against.
    let measured = sample.memory_available.is_some() || sample.load1.is_some();
    if !is_auto(modules) || !measured {
        return fixed;
    }
    let cap = auto_cap(modules);
    let size = AutoSize::from_sample(&sample, cap).unwrap_or_else(AutoSize::fallback);
    let booting = sessions
        .iter()
        .filter(|s| s.holds_slot() && s.status == SessionStatus::Starting)
        .count();
    let cap = cap as usize;
    let overcommit = auto_overcommit(modules);
    let admission = admission(&size, &sample, running, booting, cap, overcommit);
    Limit {
        max_parallel: running + admission.room_for,
        auto: Some(AutoState {
            size,
            cap,
            running,
            admission,
            sample,
            overcommit,
        }),
        running,
    }
}

/// The probe, and the last limit it produced (for readers that must not probe: the unauthenticated
/// status body and fleet peers).
pub struct Capacity {
    probe: Arc<dyn HostProbe>,
    last: Mutex<Option<Limit>>,
}

impl Capacity {
    pub fn new() -> Self {
        // Tests run on a host that measures nothing, so the static limits they were written against
        // hold; the ones that exercise auto install a scripted probe.
        #[cfg(test)]
        return Self::with_probe(Arc::new(NoProbe));
        #[cfg(not(test))]
        Self::with_probe(Arc::new(SystemProbe))
    }

    pub fn with_probe(probe: Arc<dyn HostProbe>) -> Self {
        Self {
            probe,
            last: Mutex::new(None),
        }
    }

    /// One reading, taken off the async threads and bounded: a probe that hangs reads as nothing.
    pub async fn sample(&self) -> HostSample {
        let probe = self.probe.clone();
        match tokio::time::timeout(PROBE_TIMEOUT, tokio::task::spawn_blocking(move || probe.sample())).await {
            Ok(Ok(sample)) => sample.sanitized(),
            _ => HostSample::default(),
        }
    }

    /// The limit from the latest reading, remembered for [`Capacity::last`].
    pub async fn limit(&self, modules: &ModulesConfig, sessions: &[Session]) -> Limit {
        // A fixed install pays for no probe at all.
        let sample = if is_auto(modules) {
            self.sample().await
        } else {
            HostSample::default()
        };
        let limit = evaluate(modules, sample, sessions);
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(limit);
        limit
    }

    pub fn last(&self) -> Option<Limit> {
        *self.last.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for Capacity {
    fn default() -> Self {
        Self::new()
    }
}

/// The parallel limit to admit against now: the fixed `max_parallel`, or in auto mode running colonies
/// plus the ones that fit the live host.
pub async fn max_parallel(app: &crate::app::App, modules: &ModulesConfig) -> usize {
    if !is_auto(modules) {
        return orgs::global_max_parallel(modules) as usize;
    }
    let sessions = app.sessions.read().await.clone();
    app.capacity.limit(modules, &sessions).await.max_parallel
}

/// The full limit, for status.
pub async fn limit(app: &crate::app::App, modules: &ModulesConfig) -> Limit {
    let sessions = app.sessions.read().await.clone();
    app.capacity.limit(modules, &sessions).await
}

/// The machine size for a colony booting now, as preset-default keys, when auto mode sizes it: `None`
/// where Settings pins a stack or a fixed limit, or the host is unknown.
pub async fn boot_size(app: &crate::app::App, modules: &ModulesConfig) -> Option<AutoSize> {
    if !is_auto(modules) {
        return None;
    }
    let sample = app.capacity.sample().await;
    Some(AutoSize::from_sample(&sample, auto_cap(modules))?.fit_to(&sample))
}

impl AutoSize {
    /// `cpus` and `memory` as the sandbox settings spell them.
    pub fn defaults(&self) -> Value {
        json!({"cpus": self.cpus, "memory": format!("{}G", self.memory_gib)})
    }
}

#[cfg(test)]
mod tests;
