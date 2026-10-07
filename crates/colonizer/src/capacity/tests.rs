use super::*;
use crate::queue::has_room;
use crate::sessions::tests::colony;
use std::collections::VecDeque;

/// A probe that plays a scripted series, one sample per call, repeating the last.
struct Series(Mutex<VecDeque<HostSample>>);

impl Series {
    fn new(samples: impl IntoIterator<Item = HostSample>) -> Self {
        Self(Mutex::new(samples.into_iter().collect()))
    }
}

impl HostProbe for Series {
    fn sample(&self) -> HostSample {
        let mut queue = self.0.lock().unwrap();
        if queue.len() > 1 {
            queue.pop_front().unwrap()
        } else {
            *queue.front().expect("a series has a sample")
        }
    }
}

fn gib(n: u64) -> u64 {
    n * GIB
}

/// The 32-core, 124 GiB box of the issue, with `free` GiB available and the given 1-minute load.
fn omarchy(free: u64, load1: f64) -> HostSample {
    HostSample {
        cores: Some(32),
        memory_total: Some(gib(124)),
        memory_available: Some(gib(free)),
        load1: Some(load1),
    }
}

fn running(n: usize, status: SessionStatus) -> Vec<Session> {
    (0..n)
        .map(|i| {
            let mut s = colony("acme", status);
            s.id = format!("c{i}");
            s
        })
        .collect()
}

fn auto_modules() -> ModulesConfig {
    ModulesConfig::default()
}

#[test]
fn sizes_colonies_for_these_host_shapes() {
    // (cores, RAM GiB) -> (vCPUs, GiB, planned colonies, reserve GiB)
    for ((cores, ram), (cpus, memory_gib, slots, reserve_gib)) in [
        ((8, 16), (2, 4, 3, 3)),
        ((4, 16), (2, 4, 3, 3)),
        ((4, 8), (2, 6, 1, 2)),
        ((32, 124), (3, 11, 10, 12)),
        ((40, 125), (3, 11, 10, 12)),
        ((10, 32), (2, 6, 4, 8)),
    ] {
        let size = AutoSize::for_host(cores, gib(ram), 32);
        assert_eq!(
            (size.cpus, size.memory_gib, size.slots, size.reserve_gib),
            (cpus, memory_gib, slots, reserve_gib),
            "{cores} cores / {ram} GiB"
        );
        assert_eq!(size.reserve_cpus, 2);
    }
}

#[test]
fn sizing_stays_inside_sane_bounds_on_extreme_hosts() {
    let tiny = AutoSize::for_host(1, gib(4), 32);
    assert_eq!((tiny.cpus, tiny.slots), (2, 1), "never fewer than 2 vCPUs or 1 colony");
    assert!(tiny.memory_gib >= 2);
    let huge = AutoSize::for_host(256, gib(2048), 32);
    assert_eq!(huge.slots, 32, "the planned count honours the cap");
    assert!(huge.cpus <= MAX_COLONY_CPUS);
}

#[test]
fn an_explicit_cpus_or_memory_wins_over_the_computed_size() {
    let size = AutoSize::for_host(32, gib(124), 32);
    let mut choice = ModulesConfig::default().sandbox;
    choice.settings.insert("cpus".into(), json!(6));
    let merged = crate::config::with_preset(&choice, &size.defaults());
    let schema = schema_for("sandbox", &merged.provider, &[]);
    assert_eq!(setting_u64(&merged, &schema, "cpus"), 6, "explicit cpus stands");
    assert_eq!(
        crate::config::setting_str(&merged, &schema, "memory"),
        "11G",
        "the rest is computed"
    );
}

#[test]
fn only_the_automatic_preset_without_a_fixed_number_is_auto() {
    let mut modules = auto_modules();
    assert!(is_auto(&modules), "the shipped default is auto");
    modules.sandbox.settings.insert("max_parallel".into(), json!(5));
    assert!(!is_auto(&modules), "a fixed number switches back to the static limit");
    let mut modules = auto_modules();
    modules.sandbox.settings.insert("preset".into(), json!("rust"));
    assert!(!is_auto(&modules), "a named stack is not auto");
}

#[test]
fn a_fixed_limit_ignores_the_host_entirely() {
    let mut modules = auto_modules();
    modules.sandbox.settings.insert("max_parallel".into(), json!(5));
    let limit = evaluate(&modules, omarchy(1, 99.0), &running(2, SessionStatus::Running));
    assert_eq!((limit.mode(), limit.max_parallel, limit.room_for()), ("fixed", 5, 3));
    assert_eq!(limit.status_json()["waiting_reason"], Value::Null);
}

#[test]
fn a_host_that_cannot_be_measured_keeps_the_static_limit() {
    let limit = evaluate(&auto_modules(), HostSample::default(), &running(1, SessionStatus::Running));
    assert_eq!((limit.mode(), limit.max_parallel), ("fixed", 3), "the schema default of 3");
}

#[test]
fn admission_follows_free_memory_load_and_the_cap() {
    let size = AutoSize::for_host(32, gib(124), 32);
    // 100 GiB free, the host keeps 12, each colony takes 11: (100 - 12) / 11 = 8.
    let a = admission(&size, &omarchy(100, 2.0), 0, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!((a.room_for, a.waiting_reason), (8, None));
    // The load leaves (32 - 2 - 18) / 3 = 4 colonies of 3 vCPUs.
    let a = admission(&size, &omarchy(100, 18.0), 0, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!(a.room_for, 4);
    // Both fit, the cap does not.
    let a = admission(&size, &omarchy(100, 2.0), 11, 0, 13, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!((a.room_for, a.waiting_reason), (2, None));
    let a = admission(&size, &omarchy(100, 2.0), 32, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!((a.room_for, a.waiting_reason), (0, Some(WaitReason::Cap)));
    // At the reserve, memory is what the next colony waits on; at the load ceiling, the CPU.
    let a = admission(&size, &omarchy(12, 2.0), 5, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!((a.room_for, a.waiting_reason), (0, Some(WaitReason::Memory)));
    let a = admission(&size, &omarchy(100, 29.0), 5, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!((a.room_for, a.waiting_reason), (0, Some(WaitReason::Cpu)));
}

/// The CI runner of the v0.2.9 regression: 4 cores, 16 GiB, `free` GiB available.
fn ci_runner(free: u64, load1: f64) -> HostSample {
    HostSample {
        cores: Some(4),
        memory_total: Some(gib(16)),
        memory_available: Some(gib(free)),
        load1: Some(load1),
    }
}

#[test]
fn a_16g_ci_host_admits_one_then_a_second_only_if_memory_allows() {
    let size = AutoSize::for_host(4, gib(16), 32);
    assert_eq!((size.cpus, size.memory_gib, size.reserve_gib), (2, 4, 3));
    // Idle, 14 GiB free: (14 - 3) / 4 = 2 fit, but the load leaves CPU for one 2-vCPU colony.
    let a = admission(&size, &ci_runner(14, 0.0), 0, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!(a.room_for, 1);
    // Nothing running, memory short and the host loaded: still one.
    let a = admission(&size, &ci_runner(3, 9.0), 0, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!((a.room_for, a.waiting_reason), (1, None));
    // One running and 9 GiB free: (9 - 3) / 4 = 1 more, CPU permitting.
    let a = admission(&size, &ci_runner(9, 0.0), 1, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!(a.room_for, 1);
    // One running and 6 GiB free: a second would eat the reserve, so it waits.
    let a = admission(&size, &ci_runner(6, 0.0), 1, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!((a.room_for, a.waiting_reason), (0, Some(WaitReason::Memory)));
    // The first colony never waits on CPU either, and the cap still holds.
    let a = admission(&size, &ci_runner(14, 40.0), 0, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!(a.room_for, 1);
    let a = admission(&size, &ci_runner(14, 0.0), 0, 0, 0, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!((a.room_for, a.waiting_reason), (0, Some(WaitReason::Cap)));
}

#[test]
fn an_8g_host_admits_one_colony_at_a_reduced_size() {
    let host = HostSample {
        cores: Some(4),
        memory_total: Some(gib(8)),
        memory_available: Some(gib(5)),
        load1: Some(0.5),
    };
    let limit = evaluate(&auto_modules(), host, &[]);
    assert_eq!((limit.mode(), limit.max_parallel), ("auto", 1));
    let planned = AutoSize::for_host(4, gib(8), 32);
    assert_eq!((planned.memory_gib, planned.slots), (6, 1));
    let fitted = planned.fit_to(&host);
    assert_eq!(fitted.memory_gib, 4, "5 GiB free less the 1 GiB small reserve");
    let starved = planned.fit_to(&HostSample {
        memory_available: Some(gib(1)),
        ..host
    });
    assert_eq!(starved.memory_gib, 2, "never under 2 GiB");
    assert!(starved.cpus >= 1);
}

#[test]
fn a_probe_that_reads_zero_or_garbage_falls_back_to_fixed() {
    let zeros = HostSample {
        cores: Some(0),
        memory_total: Some(0),
        memory_available: Some(0),
        load1: Some(f64::NAN),
    };
    let limit = evaluate(&auto_modules(), zeros, &[]);
    assert_eq!((limit.mode(), limit.max_parallel), ("fixed", 3));
    let negative = HostSample {
        load1: Some(-1.0),
        ..zeros
    };
    assert_eq!(evaluate(&auto_modules(), negative, &[]).mode(), "fixed");
}

#[test]
fn colonies_still_booting_are_charged_before_the_host_notices_them() {
    let size = AutoSize::for_host(32, gib(124), 32);
    // 100 GiB free, but 8 colonies were admitted a moment ago and have not taken their memory.
    let a = admission(&size, &omarchy(100, 2.0), 8, 8, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!((a.room_for, a.waiting_reason), (0, Some(WaitReason::Memory)));
}

/// The queue's view, over a scripted host: admit what fits each tick, and let colonies boot one
/// tick later. Returns `(admitted, limit)` per tick.
fn play(modules: &ModulesConfig, series: Vec<HostSample>, sessions: &mut Vec<Session>) -> Vec<(usize, Limit)> {
    let probe = Series::new(series.clone());
    let mut out = Vec::new();
    for _ in 0..series.len() {
        for s in sessions.iter_mut().filter(|s| s.status == SessionStatus::Starting) {
            s.status = SessionStatus::Running;
        }
        let limit = evaluate(modules, probe.sample(), sessions);
        let mut admitted = 0;
        // What `start_queued` does: each admission re-checks `has_room` against the one number.
        while has_room(
            sessions,
            "acme",
            &format!("acme/repo{}", sessions.len()),
            limit.max_parallel,
            None,
            99,
        ) {
            let mut s = colony("acme", SessionStatus::Starting);
            s.id = format!("n{}", sessions.len());
            s.repo = format!("acme/repo{}", sessions.len());
            sessions.push(s);
            admitted += 1;
        }
        out.push((admitted, limit));
    }
    out
}

#[test]
fn admission_stops_at_the_reserve_resumes_when_memory_frees_and_never_evicts() {
    let mut sessions = Vec::new();
    // Tick 1: plenty free. Tick 2: the 8 colonies have taken their memory and 12 GiB is left, the
    // reserve. Tick 3: another workload takes more, below the reserve. Tick 4: it frees up to 40 GiB.
    let series = vec![omarchy(100, 1.0), omarchy(12, 9.0), omarchy(6, 9.0), omarchy(40, 9.0)];
    let ticks = play(&auto_modules(), series, &mut sessions);
    assert_eq!(ticks[0].0, 8, "admits what fits");
    assert_eq!(ticks[1].0, 0, "free memory at the reserve admits nothing");
    assert_eq!(ticks[2].0, 0, "below the reserve admits nothing");
    assert_eq!(ticks[2].1.waiting_reason(), Some(WaitReason::Memory));
    assert_eq!(ticks[2].1.max_parallel, 8, "the limit never drops under what is running");
    assert_eq!(ticks[3].0, 2, "(40 - 12) / 11 = 2 once memory frees");
    assert_eq!(
        sessions.iter().filter(|s| s.holds_slot()).count(),
        10,
        "no colony was stopped along the way"
    );
}

#[test]
fn the_safety_cap_holds_however_much_the_host_has_free() {
    let mut modules = auto_modules();
    modules.sandbox.settings.insert("auto_max_parallel".into(), json!(9));
    let mut sessions = Vec::new();
    let big = HostSample {
        memory_available: Some(gib(1000)),
        memory_total: Some(gib(1000)),
        ..omarchy(900, 0.5)
    };
    let ticks = play(&modules, vec![big, big, big], &mut sessions);
    assert_eq!(ticks[0].0, 9, "the cap ends it");
    assert_eq!(ticks[2].1.waiting_reason(), Some(WaitReason::Cap));
    assert_eq!(auto_cap(&auto_modules()), 32, "the default cap");
}

#[test]
fn the_status_object_names_the_mode_the_size_and_what_the_next_colony_waits_on() {
    let limit = evaluate(&auto_modules(), omarchy(14, 9.0), &running(7, SessionStatus::Running));
    let v = limit.status_json();
    assert_eq!(v["mode"], "auto");
    assert_eq!(
        v["size"],
        json!({"cpus": 3, "memory_gb": 11, "slots": 10, "reserve_gb": 12, "reserve_cpus": 2})
    );
    assert_eq!(
        (v["room_for"].as_u64(), v["waiting_reason"].as_str()),
        (Some(0), Some("memory"))
    );
    assert_eq!((v["running"].as_u64(), v["free_bytes"].as_u64()), (Some(7), Some(gib(14))));
    assert_eq!((v["load"].as_f64(), v["cpu_cores"].as_u64()), (Some(9.0), Some(32)));
}

#[tokio::test]
async fn the_capacity_reads_its_probe_and_remembers_the_verdict() {
    let capacity = Capacity::with_probe(Arc::new(Series::new([omarchy(100, 1.0), omarchy(14, 1.0)])));
    assert_eq!(capacity.last(), None);
    let first = capacity.limit(&auto_modules(), &[]).await;
    assert_eq!(first.room_for(), 8);
    let busy = running(1, SessionStatus::Running);
    let second = capacity.limit(&auto_modules(), &busy).await;
    assert_eq!(second.room_for(), 0);
    assert_eq!(capacity.last(), Some(second));
}

#[test]
fn parses_meminfo_and_vm_stat() {
    let text = "MemTotal:       130023424 kB\nMemFree:  1 kB\nMemAvailable:   71303168 kB\n";
    assert_eq!(meminfo(text), (Some(130023424 * 1024), Some(71303168 * 1024)));
    assert_eq!(
        meminfo("MemTotal: 1 kB\nMemAvailable: 2 kB\n"),
        (None, None),
        "available above total is dropped"
    );
    let vm = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:                               1000.\nPages active:                            5000.\nPages inactive:                          3000.\n";
    assert_eq!(vm_stat_available(vm), Some(4000 * 16384), "free + inactive pages");
    assert_eq!(vm_stat_available("nonsense"), None);
}

/// A host whose free memory barely moves per admission: colonies are charged once they are Running,
/// but they only touch `touched` GiB each, so `MemAvailable` stays high.
fn lazy_host(running: usize, touched: u64) -> HostSample {
    omarchy(124u64.saturating_sub(12 + running as u64 * touched), 1.0)
}

#[test]
fn lazily_allocating_colonies_stop_at_the_commit_limit_not_the_cap() {
    let modules = auto_modules();
    let mut sessions = Vec::new();
    let mut ticks = Vec::new();
    for _ in 0..8 {
        let live = sessions.iter().filter(|s: &&Session| s.holds_slot()).count();
        let t = play(&modules, vec![lazy_host(live, 1)], &mut sessions);
        ticks.push(t.into_iter().next().unwrap());
    }
    let live = sessions.iter().filter(|s| s.holds_slot()).count();
    // (124 - 12) / (11 * 0.75) = 13.57: 13 colonies, though free memory would admit all 32.
    assert_eq!(live, 13);
    let last = ticks.last().unwrap().1;
    assert_eq!(last.auto.unwrap().admission.limited_by, Limiter::MemoryCommit);
    assert_eq!(last.waiting_reason(), Some(WaitReason::Memory));
    let v = last.status_json();
    assert_eq!(
        (v["committed_gb"].as_u64(), v["limited_by"].as_str()),
        (Some(143), Some("memory-commit"))
    );
}

#[test]
fn a_host_with_real_free_memory_still_fills_to_its_commit_size() {
    let size = AutoSize::for_host(32, gib(124), 32);
    let a = admission(&size, &omarchy(100, 1.0), 0, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!((a.room_for, a.limited_by), (8, Limiter::Free), "free memory binds first");
    let a = admission(&size, &omarchy(120, 1.0), 5, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!(
        (a.room_for, a.limited_by),
        (8, Limiter::MemoryCommit),
        "13 commit slots less 5 running"
    );
}

#[test]
fn the_overcommit_factor_is_clamped_and_changes_the_commit_limit() {
    let size = AutoSize::for_host(32, gib(124), 32);
    let at = |f| admission(&size, &omarchy(1000, 1.0), 0, 0, 32, f).room_for;
    assert_eq!((at(0.5), at(0.75), at(1.0)), (9, 9, 9), "load binds first at this load");
    let idle = |f| admission(&size, &omarchy(1000, 0.0), 0, 0, 32, f).room_for;
    assert_eq!((idle(0.5), idle(0.75), idle(1.0)), (10, 10, 10));
    let loaded = |f| {
        admission(
            &size,
            &HostSample {
                cores: Some(128),
                ..omarchy(1000, 0.0)
            },
            0,
            0,
            40,
            f,
        )
        .room_for
    };
    assert_eq!((loaded(0.5), loaded(0.75), loaded(1.0)), (20, 13, 10));
    let mut modules = auto_modules();
    for (set, want) in [(0.1, 0.5), (0.8, 0.8), (3.0, 1.0)] {
        modules.sandbox.settings.insert("auto_overcommit".into(), json!(set));
        assert_eq!(auto_overcommit(&modules), want);
    }
    assert_eq!(auto_overcommit(&auto_modules()), DEFAULT_AUTO_OVERCOMMIT);
}

#[test]
fn running_colonies_over_the_commit_limit_admit_nothing_and_are_never_evicted() {
    let sessions = running(20, SessionStatus::Running);
    let limit = evaluate(&auto_modules(), omarchy(100, 1.0), &sessions);
    assert_eq!((limit.max_parallel, limit.room_for()), (20, 0));
    assert_eq!(limit.waiting_reason(), Some(WaitReason::Memory));
}

#[test]
fn the_cpu_commit_check_limits_vcpus_on_top_of_load() {
    // 8 cores: 12 vCPUs committed at 2 per colony = 6 colonies, while the idle load alone allows 3 more.
    let size = AutoSize::for_host(8, gib(124), 32);
    let host = HostSample {
        cores: Some(8),
        ..omarchy(110, 0.0)
    };
    let a = admission(&size, &host, 6, 0, 32, DEFAULT_AUTO_OVERCOMMIT);
    assert_eq!((a.room_for, a.limited_by), (0, Limiter::CpuCommit));
    assert_eq!(a.waiting_reason, Some(WaitReason::Cpu));
}
