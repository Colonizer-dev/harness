//! Burn-down module: max out the weekly token plan. Near the weekly reset the scheduler launches
//! bug-hunt colonies — paced across the window, not burst — until the estimated allowance is down
//! to whatever reserve the operator set, then stops. Every auto-launched colony is tagged
//! `origin: "burn_down"`, and `POST /api/burn-down/stop` kills the scheduler and every colony it
//! launched. The decision is a pure function so it can be tested with a fixed clock; the loop
//! around it runs once a minute, in the watchdog's shape.
//!
//! The budget is a *measured* window, not a quota the login exposes: `claude_login.rs` only ever
//! surfaces the subscription's identity, never its usage limits or reset schedules, so the only
//! authoritative number here is what sessions have actually cost since the last reset anchor. The
//! allowance is the operator's estimate and `status` says so (`estimate: true`).

use crate::{
    Shared,
    config::{ModuleChoice, ModulesConfig, setting, setting_f64, setting_str, setting_u64},
    lifecycle, modules,
    sessions::{self, Session, SessionStatus},
};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::{DateTime, Datelike, Duration, Utc};
use serde_json::{Map, Value, json};

/// What the scheduler reads off settings. `reset_weekday` is 0=Monday .. 6=Sunday; 7 is the
/// sentinel for a day name that never parsed, so no reset is ever found and the scheduler stays
/// outside the window.
#[derive(Clone, Debug)]
pub struct Cfg {
    /// The module's hard on/off switch.
    pub enabled: bool,
    pub reset_weekday: u32,
    pub reset_time: (u32, u32),
    pub lead_hours: f64,
    pub reserve_pct: f64,
    /// The operator's estimate of the weekly plan allowance. `None` means burn-down launches
    /// nothing: never invent a number.
    pub allowance_usd: Option<f64>,
    pub spend_usd_per_colony: f64,
    pub max_live: usize,
    pub repos: Vec<String>,
}

impl Cfg {
    /// Settings snapshot. `enabled` carries the module's on/off switch; a module that was never
    /// configured reads as off with every setting at its schema default. A `reset_time` that
    /// never parses is encoded as an impossible time (`and_hms_opt` refuses hour > 23), so
    /// `next_reset` reads null and the window never opens.
    fn from_choice(choice: &ModuleChoice, schema: &Value) -> Cfg {
        Cfg {
            enabled: choice.enabled,
            reset_weekday: parse_weekday(&setting_str(choice, schema, "reset_weekday")).unwrap_or(7),
            reset_time: parse_time(&setting_str(choice, schema, "reset_time")).unwrap_or((u32::MAX, u32::MAX)),
            lead_hours: setting_f64(choice, schema, "lead_hours"),
            reserve_pct: setting_f64(choice, schema, "reserve_pct"),
            allowance_usd: setting(choice, schema, "allowance_usd").and_then(Value::as_f64),
            spend_usd_per_colony: setting_f64(choice, schema, "spend_usd_per_colony"),
            max_live: setting_u64(choice, schema, "max_live") as usize,
            repos: setting_str(choice, schema, "repos")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        }
    }
}

/// What the scheduler measures off the session list, pure and cheap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observed {
    /// Sum of `total_cost_usd` over sessions created since the last reset anchor.
    pub spent: f64,
    /// Burn-down colonies created since the current window started.
    pub launches_done: usize,
    /// Of those, how many are currently live (holding slots).
    pub live: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Disabled,
    /// Repositories are unset: burn-down is not configured and launches nothing.
    Unconfigured,
    UnknownAllowance,
    OutsideWindow,
    AtReserve,
    Hold,
    Launch {
        repo_index: usize,
    },
}

/// "Monday".."Sunday" → 0..6; anything else is hand-edited junk → `None`, which becomes the 7
/// sentinel above so no reset is ever found.
fn parse_weekday(name: &str) -> Option<u32> {
    Some(match name {
        "Monday" => 0,
        "Tuesday" => 1,
        "Wednesday" => 2,
        "Thursday" => 3,
        "Friday" => 4,
        "Saturday" => 5,
        "Sunday" => 6,
        _ => return None,
    })
}

/// "HH:MM" → `(hour, minute)`; anything else → `None`.
fn parse_time(text: &str) -> Option<(u32, u32)> {
    let (h, m) = text.trim().split_once(':')?;
    let hour: u32 = h.trim().parse().ok()?;
    let minute: u32 = m.trim().parse().ok()?;
    (hour <= 23 && minute <= 59).then_some((hour, minute))
}

fn weekday(dt: DateTime<Utc>) -> u32 {
    dt.weekday().num_days_from_monday()
}

/// The next occurrence of the reset's weekday+time strictly after `now`, and the one before it —
/// the anchor spend is measured from. `None` when the weekday or time settings never resolve,
/// which reads as "no window ever opens": the schedule cannot be trusted to burn against.
fn resets(cfg: &Cfg, now: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let (hh, mm) = cfg.reset_time;
    // The next occurrence is at most 7 days away, so the search must include day 7: on the reset
    // weekday itself (after the last reset time) the next one is exactly a week out, and a range of
    // 0..6 would find nothing that day, blanking `next_reset` for the rest of the week.
    let next = (0..8).find_map(|days| {
        let candidate = (now.date_naive() + Duration::days(days)).and_hms_opt(hh, mm, 0)?.and_utc();
        (candidate > now && weekday(candidate) == cfg.reset_weekday).then_some(candidate)
    })?;
    // The previous anchor likewise at most a week back, and seven days when the reset is scheduled
    // after the current moment on the same weekday (Monday 12:00 probed at Monday 06:00).
    let prev = (0..8).find_map(|days| {
        let candidate = (now.date_naive() - Duration::days(days)).and_hms_opt(hh, mm, 0)?.and_utc();
        (candidate <= now && weekday(candidate) == cfg.reset_weekday).then_some(candidate)
    })?;
    Some((next, prev))
}

/// How many `spend_usd_per_colony`-sized launches the remaining spend (above the reserve) still
/// buys: at least one while there is anything left to burn, none once spend has reached the reserve.
/// One clamp for both `decide` and `status_at`, so the plan the UI draws is the plan the scheduler
/// paces.
fn launches_needed(cfg: &Cfg, spent: f64) -> u64 {
    let Some(allowance) = cfg.allowance_usd else {
        return 0;
    };
    if cfg.spend_usd_per_colony <= 0.0 {
        return 0;
    }
    let to_burn = allowance - spent - allowance * cfg.reserve_pct / 100.0;
    if to_burn <= 0.0 {
        return 0;
    }
    (to_burn / cfg.spend_usd_per_colony).ceil().max(1.0) as u64
}

/// The weekly-window decision, watchdog-style: pure, fixed-clock testable, no IO.
///
/// Pace rule: over the whole window the operator wants `ceiling(remaining / spend_per_colony)`
/// launches. By fraction `f` of the window, `floor(f × needed)` should have gone out already; a
/// tick that has fallen behind launches another (round-robining over the repositories), one that
/// is on pace or ahead holds. `max_live` caps how many may be out at once on top of that.
pub fn decide(cfg: &Cfg, now: DateTime<Utc>, obs: &Observed) -> Decision {
    if !cfg.enabled {
        return Decision::Disabled;
    }
    if cfg.repos.is_empty() {
        return Decision::Unconfigured;
    }
    let Some(allowance) = cfg.allowance_usd else {
        return Decision::UnknownAllowance;
    };
    let Some((next_reset, _)) = resets(cfg, now) else {
        return Decision::OutsideWindow;
    };
    let window_start = next_reset - Duration::hours(cfg.lead_hours as i64);
    if now < window_start || now >= next_reset {
        return Decision::OutsideWindow;
    }
    if cfg.spend_usd_per_colony <= 0.0 {
        // A zero or negative per-colony spend makes no plan to pace; never a burst.
        return Decision::Hold;
    }
    let remaining = allowance - obs.spent;
    let reserve = allowance * cfg.reserve_pct / 100.0;
    if remaining <= reserve {
        return Decision::AtReserve;
    }
    let needed = launches_needed(cfg, obs.spent);
    let window = next_reset - window_start;
    let f = if window <= Duration::zero() {
        1.0
    } else {
        ((now - window_start).num_milliseconds() as f64 / window.num_milliseconds() as f64).clamp(0.0, 1.0)
    };
    let expected = (f * needed as f64).floor() as usize;
    if obs.launches_done >= expected {
        return Decision::Hold;
    }
    if obs.live >= cfg.max_live {
        return Decision::Hold;
    }
    Decision::Launch {
        repo_index: obs.launches_done % cfg.repos.len(),
    }
}

/// The measured state the scheduler and `status` both work from.
fn observed(cfg: &Cfg, now: DateTime<Utc>, sessions: &[Session]) -> Observed {
    let Some((next_reset, prev_reset)) = resets(cfg, now) else {
        return Observed {
            spent: 0.0,
            launches_done: 0,
            live: 0,
        };
    };
    let window_start = next_reset - Duration::hours(cfg.lead_hours as i64);
    let launched = |s: &Session| s.origin.as_deref() == Some("burn_down") && s.created_at >= window_start;
    Observed {
        spent: sessions
            .iter()
            .filter(|s| s.created_at >= prev_reset)
            .map(|s| s.total_cost_usd())
            .sum(),
        launches_done: sessions.iter().filter(|s| launched(s)).count(),
        live: sessions.iter().filter(|s| launched(s) && s.status.is_live()).count(),
    }
}

/// The `GET /api/burn-down` body, computed as a pure function of a settings snapshot, a clock and
/// the session list so the UI and the scheduler can never disagree about what is happening.
pub fn status_at(cfg: &Cfg, now: DateTime<Utc>, sessions: &[Session]) -> Value {
    let obs = observed(cfg, now, sessions);
    let state = match decide(cfg, now, &obs) {
        Decision::Disabled => "disabled",
        Decision::Unconfigured => "unconfigured",
        Decision::UnknownAllowance => "unknown_allowance",
        Decision::OutsideWindow => "outside_window",
        Decision::AtReserve => "at_reserve",
        Decision::Hold | Decision::Launch { .. } => "burning",
    };
    let reset = resets(cfg, now);
    let next_reset = reset.map(|(next, _)| next);
    let window_start = reset.map(|(next, _)| next - Duration::hours(cfg.lead_hours as i64));
    let spent = reset.map_or(0.0, |(_, prev)| {
        sessions
            .iter()
            .filter(|s| s.created_at >= prev)
            .map(|s| s.total_cost_usd())
            .sum()
    });
    let allowance = cfg.allowance_usd;
    let remaining = allowance.map(|a| a - spent);
    let reserve_usd = allowance.map(|a| a * cfg.reserve_pct / 100.0);
    let launches_needed = if !cfg.enabled || cfg.repos.is_empty() || cfg.spend_usd_per_colony <= 0.0 || reset.is_none() {
        Value::Null
    } else if cfg.allowance_usd.is_some() {
        json!(launches_needed(cfg, spent))
    } else {
        Value::Null
    };
    let in_window = |s: &Session| window_start.is_some_and(|w| s.origin.as_deref() == Some("burn_down") && s.created_at >= w);
    let done = sessions.iter().filter(|s| in_window(s)).count();
    json!({
        "enabled": cfg.enabled,
        "state": state,
        "estimate": true,
        "now": now.to_rfc3339(),
        "next_reset": next_reset.map(|t| t.to_rfc3339()),
        "window_start": window_start.map(|t| t.to_rfc3339()),
        "spent_usd": spent,
        "allowance_usd": allowance,
        "remaining_usd": remaining,
        "reserve_usd": reserve_usd,
        "colonies": {
            "live": sessions.iter().filter(|s| in_window(s) && s.status.is_live()).count(),
            "queued": sessions.iter().filter(|s| in_window(s) && s.status == SessionStatus::Queued).count(),
            "total": done,
        },
        "launches_needed": launches_needed,
        "launches_done": done,
    })
}

fn cfg_and_instructions(modules: &ModulesConfig, agents: &[modules::AgentModule]) -> (Cfg, String) {
    let choice = modules.get("burn_down");
    let provider = choice.map(|c| c.provider.as_str()).unwrap_or("default");
    let schema = modules::schema_for("burn_down", provider, agents);
    let base = choice.cloned().unwrap_or_else(|| ModuleChoice {
        provider: "default".into(),
        enabled: false,
        settings: Map::new(),
    });
    let cfg = Cfg::from_choice(&base, &schema);
    let instructions = setting_str(&base, &schema, "instructions");
    (cfg, instructions)
}

/// The prompt a burn-down colony runs on when `instructions` is empty: find real bugs, verify
/// them before filing, keep pull requests small. It is generic on purpose — the dedicated
/// red-team runs of issue #212 are still being built; hunter colonies adopt those when they land.
const BUILTIN_HUNT_PROMPT: &str = "\
This colony is on a burn-down run: find real bugs in this repository and fix them, one small \
pull request per verified bug. Look for correctness defects, security gaps, crashes, data loss, \
deadlocks and clear regressions. Before acting on anything, verify it is a genuine defect — \
reproduce it if you can, and confirm it with a fresh subagent rather than trusting your first \
pass. Do not file style preferences or hypothetical issues as defects. Keep every pull request \
small and focused on one confirmed bug, with a pull request description stating what is wrong, \
how you verified it, and what you changed.";

/// The launch body the scheduler submits to the ordinary admission path. Pure so a test can pin the
/// contract: `origin: "burn_down"` marks the colony for the group stop, `autopilot` lets it run by
/// itself, `allow_duplicate` lets it land on a repository issue another colony is already on, and an
/// empty `instructions` falls back to the built-in hunt prompt. Built as JSON because `NewSession`'s
/// fields are private to the sessions module; only the scheduler fills in `origin`.
fn launch_body(repo: &str, instructions: &str) -> Value {
    let prompt = if instructions.trim().is_empty() {
        BUILTIN_HUNT_PROMPT.to_string()
    } else {
        instructions.to_string()
    };
    json!({
        "repo": repo,
        "title": "Burn-down hunt",
        "instructions": prompt,
        "autopilot": true,
        "allow_duplicate": true,
        "origin": "burn_down",
    })
}

/// Runs the launch body through the ordinary `sessions::create` admission path, so a burn-down
/// colony queues like any other past the parallel limit and needs nothing special to boot.
async fn launch(app: &Shared, repo: &str, instructions: &str) {
    let new_session = match serde_json::from_value::<sessions::NewSession>(launch_body(repo, instructions)) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("burn_down: could not build a launch body: {e:#}");
            return;
        }
    };
    match sessions::create(State(app.clone()), Json(new_session)).await {
        Ok(Json(session)) => {
            app.session_log(
                &session.id,
                "info",
                format!("burn-down: launched a bug-hunt colony on {repo}"),
            )
            .await;
        }
        // Logged and dropped: the next tick decides again, so a transient failure (an org
        // switched off, a login gap) is not a retry storm.
        Err(_) => eprintln!("burn_down: could not start a hunt colony on {repo}; will retry next tick"),
    }
}

async fn tick_once(app: &Shared) {
    let (modules, sessions) = {
        let modules = app.modules.read().await.clone();
        let sessions = app.sessions.read().await.clone();
        (modules, sessions)
    };
    let (cfg, instructions) = cfg_and_instructions(&modules, &app.agents);
    let now = Utc::now();
    let obs = observed(&cfg, now, &sessions);
    match decide(&cfg, now, &obs) {
        Decision::Launch { repo_index } => {
            if let Some(repo) = cfg.repos.get(repo_index).cloned() {
                launch(app, &repo, &instructions).await;
            }
        }
        Decision::Disabled
        | Decision::Unconfigured
        | Decision::UnknownAllowance
        | Decision::OutsideWindow
        | Decision::AtReserve
        | Decision::Hold => {}
    }
}

/// Runs the scheduler forever, once a minute. Launches go through `sessions::create`, so the
/// parallel-limit queue applies; the loop never panics, never retries a failed launch before the
/// next tick, and does nothing at all while the module is off.
pub async fn run(app: Shared) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        tick_once(&app).await;
    }
}

/// `GET /api/burn-down`: the measured window, next reset and launch plan, as seen now.
pub async fn status(State(app): State<Shared>) -> Json<Value> {
    Json(status_now(&app).await)
}

/// The live snapshot `GET /api/burn-down` answers with; split out so the payload is reachable
/// without an HTTP call.
pub async fn status_now(app: &Shared) -> Value {
    let modules = app.modules.read().await.clone();
    let (cfg, _) = cfg_and_instructions(&modules, &app.agents);
    let now = Utc::now();
    let sessions = app.sessions.read().await.clone();
    status_at(&cfg, now, &sessions)
}

/// `POST /api/burn-down/stop`: persistently switches the module off, then stops every colony it
/// launched — live ones through the ordinary stop, queued ones out of the queue — and every other
/// colony is left alone. Idempotent, and safe before burn-down was ever configured: the module
/// record is simply created with `enabled: false`.
pub async fn stop(State(app): State<Shared>) -> impl IntoResponse {
    {
        let mut modules = app.modules.write().await;
        if let Some(choice) = modules.get_mut("burn_down") {
            choice.enabled = false;
        }
        if let Err(e) = modules.save(&app.modules_file()).await {
            app.storage_failed("save modules.json", &e).await;
        }
    }
    let sessions = app.sessions.read().await.clone();
    for s in sessions.iter().filter(|s| s.origin.as_deref() == Some("burn_down")) {
        if s.status == SessionStatus::Queued {
            // A queued colony never started, so there is no microVM to remove — the same shape the
            // session stop handler uses.
            let mut attention = None;
            app.update_session(&s.id, |x| {
                x.status = SessionStatus::Stopped;
                attention = x.clear_attention();
            })
            .await;
            app.note_cleared_attention(&s.id, attention).await;
            app.session_log(
                &s.id,
                "info",
                "burn-down stop: halted by the operator before it started".into(),
            )
            .await;
        } else if s.status.is_live() {
            lifecycle::stop_colony(
                &app,
                s,
                |_| true,
                "stopped by the burn-down stop: the operator halted burn-down mode".into(),
                "burn-down stop: halted by the operator".into(),
            )
            .await;
        }
    }
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::colony;
    use axum::response::IntoResponse;

    const NO_OBS: Observed = Observed {
        spent: 0.0,
        launches_done: 0,
        live: 0,
    };

    /// The weekly reset in the tests: Monday 00:00 UTC (`2026-08-31` is a Monday).
    fn cfg() -> Cfg {
        Cfg {
            enabled: true,
            reset_weekday: 0,
            reset_time: (0, 0),
            lead_hours: 48.0,
            reserve_pct: 5.0,
            allowance_usd: Some(100.0),
            spend_usd_per_colony: 25.0,
            max_live: 2,
            repos: vec!["acme/app".into(), "acme/lib".into()],
        }
    }

    /// Midnight at 2026-08-31 UTC, a Monday; the probes are hour/minute on or near it.
    fn monday(hour: u32, minute: u32) -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 31)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
            .and_utc()
    }

    #[test]
    fn a_disabled_module_never_launches() {
        let mut c = cfg();
        c.enabled = false;
        assert_eq!(decide(&c, monday(12, 0), &NO_OBS), Decision::Disabled);
    }

    #[test]
    fn empty_repos_means_unconfigured() {
        let mut c = cfg();
        c.repos = Vec::new();
        assert_eq!(decide(&c, monday(12, 0), &NO_OBS), Decision::Unconfigured);
    }

    #[test]
    fn unknown_allowance_never_launches_even_inside_the_window() {
        let mut c = cfg();
        c.allowance_usd = None;
        let inside = monday(0, 0) - Duration::days(1); // Sunday: inside the window
        let d = decide(&c, inside, &NO_OBS);
        assert_eq!(d, Decision::UnknownAllowance);
    }

    #[test]
    fn the_window_arithmetic_is_exclusive_at_both_edges() {
        // Friday, before the window opens (Saturday 00:00) → outside.
        let friday = monday(0, 0) - Duration::days(3) + Duration::hours(12);
        assert_eq!(decide(&cfg(), friday, &NO_OBS), Decision::OutsideWindow);
        // The day after the reset → outside.
        let tuesday = monday(0, 0) + Duration::days(1) + Duration::hours(6);
        assert_eq!(decide(&cfg(), tuesday, &NO_OBS), Decision::OutsideWindow);
        // Saturday noon is inside, and not excused from the pace rule.
        let saturday = monday(0, 0) - Duration::days(2);
        assert!(matches!(decide(&cfg(), saturday, &NO_OBS), Decision::Hold));
        // A schedule that never resolves opens no window at all.
        let mut c = cfg();
        c.reset_time = (u32::MAX, u32::MAX);
        assert_eq!(decide(&c, monday(12, 0), &NO_OBS), Decision::OutsideWindow);
    }

    #[test]
    fn the_reset_weekday_still_finds_the_next_reset() {
        // The next reset is always at most 7 days away, so the day-7 candidate must be searched:
        // on the reset weekday itself (Monday) the next Monday 00:00 is exactly 7 days out. A
        // search limited to 0..6 days finds nothing that day, so `resets` reads null every week
        // between the reset and the week's end — the scheduler survives (the window is closed
        // anyway) but the status API loses `next_reset`, `spent` and the colony columns.
        let after_reset = monday(0, 0) + Duration::minutes(1); // Monday 00:01
        assert_eq!(
            resets(&cfg(), after_reset).map(|(next, _)| next),
            Some(monday(0, 0) + Duration::days(7)),
            "next_reset stays present on the reset weekday"
        );
        // The instant the reset lands: the window has closed, but the *next* reset is still known.
        assert_eq!(
            decide(&cfg(), monday(0, 0), &NO_OBS),
            Decision::OutsideWindow,
            "now == reset closes the window"
        );
        // And a reset past midday still finds the previous reset across the 7-day boundary.
        let mut noon = cfg();
        noon.reset_time = (12, 0);
        let monday_noon = monday(12, 0);
        assert_eq!(
            resets(&noon, monday_noon + Duration::minutes(1)),
            Some((monday_noon + Duration::days(7), monday_noon)),
        );
    }

    #[test]
    fn at_or_below_the_reserve_the_scheduler_stops() {
        let now = monday(0, 0) - Duration::days(2) + Duration::minutes(1); // Saturday 00:01
        let at_reserve = Observed {
            spent: 95.0,
            launches_done: 0,
            live: 0,
        };
        assert_eq!(decide(&cfg(), now, &at_reserve), Decision::AtReserve);
        let below = Observed {
            spent: 96.0,
            launches_done: 0,
            live: 0,
        };
        assert_eq!(decide(&cfg(), now, &below), Decision::AtReserve);
    }

    #[test]
    fn the_pace_is_proportional_not_a_burst() {
        // Window Sat 00:00 → Mon 00:00 (48 h). With allowance 100, reserve 5, $25 per colony,
        // 4 launches are needed; at the exact start the pace says hold so the window opens calm.
        let start = monday(0, 0) - Duration::days(2);
        assert_eq!(decide(&cfg(), start, &NO_OBS), Decision::Hold);
        // A quarter in, one launch should have gone out: none is behind, one is on pace.
        let quarter = start + Duration::hours(12);
        assert!(matches!(
            decide(
                &cfg(),
                quarter,
                &Observed {
                    spent: 0.0,
                    launches_done: 0,
                    live: 0
                }
            ),
            Decision::Launch { repo_index: 0 }
        ));
        assert_eq!(
            decide(
                &cfg(),
                quarter,
                &Observed {
                    spent: 0.0,
                    launches_done: 1,
                    live: 0
                }
            ),
            Decision::Hold
        );
        // Halfway, two should be out: still behind on one launches, on pace holds.
        let halfway = monday(0, 0) - Duration::days(1);
        assert!(matches!(
            decide(
                &cfg(),
                halfway,
                &Observed {
                    spent: 0.0,
                    launches_done: 0,
                    live: 0
                }
            ),
            Decision::Launch { repo_index: 0 }
        ));
        assert!(matches!(
            decide(
                &cfg(),
                halfway,
                &Observed {
                    spent: 0.0,
                    launches_done: 1,
                    live: 0
                }
            ),
            Decision::Launch { repo_index: 1 }
        ));
        assert_eq!(
            decide(
                &cfg(),
                halfway,
                &Observed {
                    spent: 0.0,
                    launches_done: 2,
                    live: 0
                }
            ),
            Decision::Hold
        );
    }

    #[test]
    fn max_live_caps_concurrent_colonies_before_launch() {
        let halfway = monday(0, 0) - Duration::days(1);
        // Two live colonies reach the cap of two even though the pace is behind.
        assert_eq!(
            decide(
                &cfg(),
                halfway,
                &Observed {
                    spent: 0.0,
                    launches_done: 0,
                    live: 2
                }
            ),
            Decision::Hold
        );
        assert!(matches!(
            decide(
                &cfg(),
                halfway,
                &Observed {
                    spent: 0.0,
                    launches_done: 0,
                    live: 1
                }
            ),
            Decision::Launch { .. }
        ));
    }

    #[test]
    fn launches_round_robin_across_the_repositories() {
        let halfway = monday(0, 0) - Duration::days(1);
        assert!(matches!(
            decide(
                &cfg(),
                halfway,
                &Observed {
                    spent: 0.0,
                    launches_done: 0,
                    live: 0
                }
            ),
            Decision::Launch { repo_index: 0 }
        ));
        assert!(matches!(
            decide(
                &cfg(),
                halfway,
                &Observed {
                    spent: 0.0,
                    launches_done: 1,
                    live: 0
                }
            ),
            Decision::Launch { repo_index: 1 }
        ));
        let mut three = cfg();
        three.repos = vec!["a/x".into(), "b/y".into(), "c/z".into()];
        // 75% in, expected = 3; the third launch goes to repo index 2.
        let three_quarters = monday(0, 0) - Duration::hours(12);
        assert!(matches!(
            decide(
                &three,
                three_quarters,
                &Observed {
                    spent: 0.0,
                    launches_done: 2,
                    live: 0
                }
            ),
            Decision::Launch { repo_index: 2 }
        ));
    }

    #[test]
    fn a_zero_per_colony_spend_holds_rather_than_bursting() {
        let mut c = cfg();
        c.spend_usd_per_colony = 0.0;
        let halfway = monday(0, 0) - Duration::days(1);
        assert_eq!(decide(&c, halfway, &NO_OBS), Decision::Hold);
    }

    #[test]
    fn the_launch_body_marks_its_origin_autopilot_and_repo_and_falls_back_to_the_builtin_prompt() {
        // The repo is the round-robin pick `decide` hands the launcher: at three quarters of the
        // window with two of four launches out, the next goes to repo index 2.
        let halfway = monday(0, 0) - Duration::days(1);
        let Decision::Launch { repo_index } = decide(
            &cfg(),
            halfway,
            &Observed {
                spent: 0.0,
                launches_done: 1,
                live: 0,
            },
        ) else {
            panic!("the tick must be launching");
        };
        let repo = &cfg().repos[repo_index];
        assert_eq!(repo_index, 1);

        let plain = launch_body(repo, "");
        assert_eq!(plain["repo"], "acme/lib");
        assert_eq!(plain["title"], "Burn-down hunt");
        assert_eq!(plain["autopilot"], true, "a scheduler-launched colony runs by itself");
        assert_eq!(
            plain["allow_duplicate"], true,
            "it may land on a repo issue another colony already holds"
        );
        assert_eq!(plain["origin"], "burn_down", "the group stop and the UI label rely on this");
        assert_eq!(
            plain["instructions"], BUILTIN_HUNT_PROMPT,
            "empty instructions get the built-in bug-hunt prompt"
        );
        assert!(
            serde_json::from_value::<sessions::NewSession>(plain).is_ok(),
            "the body must round-trip through the admission shape"
        );

        let custom = launch_body("acme/app", "Hunt for memory leaks only");
        assert_eq!(custom["instructions"], "Hunt for memory leaks only");
        // A whitespace-only prompt counts as unset.
        assert_eq!(launch_body("acme/app", "  \n")["instructions"], BUILTIN_HUNT_PROMPT);
    }

    #[test]
    fn the_plan_and_the_status_draw_the_same_launch_count() {
        // (100 − 5) / 25, ceil: the fresh-window plan is 4 launches, and the same helper the status
        // card draws (the existing payload test pins status's side with $20 spent → 3).
        assert_eq!(launches_needed(&cfg(), 0.0), 4);
        assert_eq!(
            launches_needed(&cfg(), 92.0),
            1,
            "a sliver above the reserve still pays for one"
        );
        assert_eq!(launches_needed(&cfg(), 95.0), 0, "at the reserve the plan owes nothing");
        assert_eq!(launches_needed(&cfg(), 96.0), 0, "below the reserve likewise");
        assert_eq!(
            decide(
                &cfg(),
                monday(0, 0) - Duration::days(1),
                &Observed {
                    spent: 95.0,
                    launches_done: 0,
                    live: 0
                }
            ),
            Decision::AtReserve,
            "decide stops where the helper's count hits zero"
        );
        let mut unknown = cfg();
        unknown.allowance_usd = None;
        assert_eq!(launches_needed(&unknown, 0.0), 0, "no allowance, no plan");
        let mut zero_spc = cfg();
        zero_spc.spend_usd_per_colony = 0.0;
        assert_eq!(launches_needed(&zero_spc, 0.0), 0, "no per-colony cost, no plan");
    }

    /// One burn-down colony since the window started, with whatever cost it has spent.
    fn burn_session(id: &str, status: SessionStatus, hours_into_window: i64, cost: f64) -> Session {
        let mut s = colony("acme", status);
        s.id = id.into();
        s.origin = Some("burn_down".into());
        s.created_at = monday(0, 0) - Duration::days(2) + Duration::hours(hours_into_window);
        s.cost_usd = Some(cost);
        s
    }

    #[test]
    fn the_status_payload_is_the_measured_window() {
        let halfway = monday(0, 0) - Duration::days(1);
        let sessions = vec![
            burn_session("live", SessionStatus::Running, 12, 10.0),
            burn_session("queued", SessionStatus::Queued, 13, 0.0),
            {
                let mut other = colony("acme", SessionStatus::Running);
                other.id = "manual".into();
                other.created_at = monday(0, 0) - Duration::days(2) + Duration::hours(13);
                other.cost_usd = Some(10.0); // counts toward spend, not toward launches
                other
            },
        ];
        let value = status_at(&cfg(), halfway, &sessions);
        assert_eq!(value["state"], "burning");
        assert_eq!(value["enabled"], true);
        assert_eq!(value["estimate"], true, "the allowance is an estimate, never a fact");
        assert_eq!(value["allowance_usd"], 100.0);
        assert_eq!(value["spent_usd"], 20.0);
        assert_eq!(value["remaining_usd"], 80.0);
        assert_eq!(value["reserve_usd"], 5.0);
        assert_eq!(value["launches_needed"], 3, "(100 − 20 − 5) / 25 rounded up");
        assert_eq!(value["launches_done"], 2, "burn-down colonies in the current window");
        assert_eq!(value["colonies"]["live"], 1);
        assert_eq!(value["colonies"]["queued"], 1);
        assert_eq!(value["colonies"]["total"], 2);
        assert!(value["next_reset"].is_string(), "{value}");
        assert!(value["window_start"].is_string(), "{value}");

        let disabled = {
            let mut c = cfg();
            c.enabled = false;
            c
        };
        let value = status_at(&disabled, halfway, &sessions);
        assert_eq!(value["state"], "disabled");
        assert_eq!(value["launches_needed"], Value::Null, "no plan while the module is off");

        let unknown = {
            let mut c = cfg();
            c.allowance_usd = None;
            c
        };
        let value = status_at(&unknown, halfway, &sessions);
        assert_eq!(value["state"], "unknown_allowance");
        assert_eq!(value["allowance_usd"], Value::Null);
        assert_eq!(value["remaining_usd"], Value::Null);
        assert_eq!(value["reserve_usd"], Value::Null);
    }

    #[tokio::test]
    async fn the_stop_halts_burn_down_colonies_and_saves_the_module_off() {
        let root = std::env::temp_dir().join(format!("colonizer-burn-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let mut live = colony("acme", SessionStatus::Running);
        live.id = "burn-live".into();
        live.origin = Some("burn_down".into());
        live.created_at = Utc::now();
        let mut queued = colony("acme", SessionStatus::Queued);
        queued.id = "burn-queued".into();
        queued.origin = Some("burn_down".into());
        queued.created_at = Utc::now();
        let mut manual = colony("acme", SessionStatus::Running);
        manual.id = "manual".into();
        manual.created_at = Utc::now();
        std::fs::create_dir_all(app.session_dir("burn-live")).unwrap();
        std::fs::create_dir_all(app.session_dir("burn-queued")).unwrap();
        *app.sessions.write().await = vec![live, queued, manual];

        let response = stop(State(app.clone())).await;
        assert_eq!(response.into_response().status(), StatusCode::NO_CONTENT);

        {
            let sessions = app.sessions.read().await;
            let by = |id: &str| sessions.iter().find(|s| s.id == id).unwrap();
            assert_eq!(by("burn-live").status, SessionStatus::Stopped);
            assert_eq!(
                by("burn-queued").status,
                SessionStatus::Stopped,
                "queued colonies leave the queue"
            );
            assert_eq!(
                by("manual").status,
                SessionStatus::Running,
                "a colony the operator started is untouched"
            );
        }
        let (modules, _) = ModulesConfig::load(&app.modules_file()).unwrap();
        assert_eq!(
            modules.get("burn_down").map(|c| c.enabled),
            Some(false),
            "stop persists the module off"
        );

        // Idempotent: stopping again is a no-op that still answers 204.
        let response = stop(State(app.clone())).await;
        assert_eq!(response.into_response().status(), StatusCode::NO_CONTENT);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_stop_is_safe_when_burn_down_was_never_configured() {
        let root = std::env::temp_dir().join(format!("colonizer-burn-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let response = stop(State(app.clone())).await;
        assert_eq!(response.into_response().status(), StatusCode::NO_CONTENT);
        // The record is created, already off, so a later save from the UI finds it.
        let (modules, _) = ModulesConfig::load(&app.modules_file()).unwrap();
        assert_eq!(modules.get("burn_down").map(|c| c.enabled), Some(false));
        let _ = std::fs::remove_dir_all(root);
    }
}
