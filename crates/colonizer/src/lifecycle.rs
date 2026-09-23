//! Everything that ends or restarts a colony's microVM: stopping and deleting, resuming on the kept
//! worktree, cleaning up, recovering after a harness restart, and the minute-tick that notices a
//! microVM the host or its max session length has stopped.
//!
//! The worktree and branch outlive the microVM, so every path here either preserves them for a
//! resume or removes them only once the colony is done for good.
//!
//! The host's own limits are here as well — a spend budget and a host-disk quota — because
//! passing one ends a colony exactly the way the max session length does.

use crate::{
    ApiResult, App, Shared, client_error, github, orgs, providers,
    sandbox::{self},
    spend,
    util::{
        dir_size,
        faults::{self, Op},
        format_disk_size,
    },
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::Utc;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};

#[allow(unused_imports)]
use crate::{events::*, publish::*, queue::*, sessions::*};

/// What a refused resume says: the conditions `can_resume` checks, phrased for the user.
pub(crate) const RESUME_CONFLICT: &str = "this colony can't be resumed: it has to be stopped and still have its worktree";

/// Stops the agent link, asks agentd to shut the runner down, removes the VM and its mesh node.
pub(crate) async fn teardown_vm(app: &Shared, s: &Session) {
    if let Some(rt) = app.runtimes.lock().await.get(&s.id).cloned() {
        rt.stop.send_replace(true);
    }
    if s.mesh.as_ref().is_some_and(|m| m.ip.is_some()) || s.local_port.is_some() {
        let _ = tokio::time::timeout(Duration::from_secs(15), agentd_http(app, s, "POST", "/v1/shutdown")).await;
    }
    sandbox::remove(&app.cfg.msb, &s.sandbox).await;
    if s.mesh.is_some()
        && let Ok(mesh) = app.mesh().await
    {
        let _ = mesh.delete_nodes_named(&s.sandbox).await;
    }
}

/// Reconnects to microVMs that kept running while the harness was down.
pub async fn recover(app: &Shared) {
    let running = sandbox::running(&app.cfg.msb).await.unwrap_or_default();
    // Both this list and the running set above are a snapshot, and the list moves under it while
    // this loop works down the colonies — the HTTP API keeps serving through a restart. Each
    // colony is handled the way `watch_sandboxes` handles its own: under the colony's lifecycle
    // lock, re-read fresh, and only then decide. The `running` check stays on the snapshot — it
    // is the detection, and a microVM does not come back — but every status flip below is a
    // compare-and-set, so a publish, resume or stop that claimed the colony in between is not
    // clobbered.
    let sessions = app.sessions.read().await.clone();
    for s in sessions {
        // The snapshot above moves under this loop: the HTTP API admits resumes, stops and
        // publishes while it works down the colonies. Take the colony's lifecycle lock and
        // re-read it, and branch on that fresh record — a colony already claimed, stopped or
        // deleted in the meantime is not this pass's to touch. The `running` check stays on
        // the snapshot — it is the detection, and a microVM does not come back — but every
        // status flip below is a compare-and-set, so a publish, resume or stop that claimed
        // the colony in between is not clobbered. A resume admitted by a flip below waits on
        // this same lock and finds the teardown finished rather than booting a microVM that
        // the in-flight `msb rm --force` then removes.
        let lifecycle = app.session_lock(&s.id).await;
        let _lifecycle = lifecycle.lock().await;
        let Some(fresh) = app.session(&s.id).await else {
            continue;
        };
        if claimed_boot_after_snapshot(&s, &fresh) {
            // A resume or the queue claimed the colony after the snapshot; tearing it down
            // would `msb rm --force` the microVM the boot is creating. A snapshot already
            // `Starting` is an orphaned boot instead, and falls through to the teardown below.
            // Unless the boot died before its first durable artifact: no worktree was laid
            // down, so leaving it stopped would abandon it where no resume can reach it. The
            // retry clock (`boot_attempt_started_at`) is stamped when a boot actually starts,
            // so a fresh claim (no clock yet) is left alone while a dead boot re-queues under
            // admission on the same budget.
            if fresh.git_admin_dir.is_none() && fresh.boot_attempt_started_at.is_some() {
                app.update_session(&fresh.id, |x| {
                    x.status = SessionStatus::Queued;
                    x.error = None;
                })
                .await;
                app.session_log(
                    &fresh.id,
                    "info",
                    "harness restarted during boot, before the worktree existed: queued to start again".into(),
                )
                .await;
            }
            continue;
        }
        if s.status == SessionStatus::Publishing {
            // The kill may have landed between persisting `publishing` and the teardown inside it, so a
            // microVM can still be running — and after a restart nothing would reap it: the runtimes map
            // is empty, so no later publish removes it, and `watch_sandboxes` skips non-live statuses.
            // Only an orphaned push from before the restart is this pass's to reap: a publish that
            // claimed the colony after the snapshot owns the worktree now and is left alone.
            if !orphaned_publish(&s, &fresh) {
                continue;
            }
            //
            // Fencing (issue #98): the colony is left `Failed`, and anything preserved here resumes
            // only under fresh authorization — `authority::requires_reauth_after_restart` is true by
            // construction, so no grant survives the restart and resume re-authenticates from scratch.
            debug_assert!(crate::authority::requires_reauth_after_restart());
            teardown_vm(app, &fresh).await;
            let mut attention = None;
            app.update_session(&fresh.id, |x| {
                if !mark_failed_after_restart(x) {
                    return false;
                }
                attention = x.clear_attention();
                true
            })
            .await;
            app.note_cleared_attention(&fresh.id, attention).await;
            continue;
        }
        if !s.status.is_live() {
            continue;
        }
        if !fresh.status.is_live() {
            continue;
        }
        if claimed_boot_after_snapshot(&s, &fresh) {
            // A resume or the queue claimed the colony after the snapshot; tearing it down would
            // `msb rm --force` the microVM the boot is creating.
            continue;
        }
        let reachable = fresh.mesh.as_ref().is_some_and(|m| m.ip.is_some()) || fresh.local_port.is_some();
        if fresh.status != SessionStatus::Starting && running.contains(&fresh.sandbox) && reachable {
            if fresh.mesh.is_some() {
                match app.mesh().await {
                    Ok(mesh) => {
                        if let Err(e) = mesh.ensure_started().await {
                            app.session_log(&fresh.id, "error", format!("mesh failed to start: {e:#}"))
                                .await;
                        }
                    }
                    Err(e) => app.session_log(&fresh.id, "error", format!("{e:#}")).await,
                }
            }
            app.session_log(
                &fresh.id,
                "info",
                "harness restarted: reconnecting to the running microVM".into(),
            )
            .await;
            start_link(app, &fresh.id).await;
        } else {
            // A snapshot already `Starting` is an orphaned boot — its owner died with the restart
            // and nothing else reaps `Starting` — so it falls through here instead of stranding
            // the colony forever.
            teardown_vm(app, &fresh).await;
            let at = fresh.status;
            let mut attention = None;
            app.update_session(&fresh.id, |x| {
                if !mark_stopped_after_restart(x, at) {
                    return false;
                }
                attention = x.clear_attention();
                true
            })
            .await;
            app.note_cleared_attention(&fresh.id, attention).await;
        }
    }
}

/// A push this restart orphaned: the snapshot already had the colony `Publishing`, so the publish
/// died with the restart and nothing will reap its microVM. A colony the snapshot saw live that a
/// publish claimed in the meantime is not orphaned — it owns the worktree now.
fn orphaned_publish(snap: &Session, fresh: &Session) -> bool {
    snap.status == SessionStatus::Publishing && fresh.status == SessionStatus::Publishing
}

/// A boot this pass must not touch: the snapshot did not have the colony `Starting`, so a resume
/// or the queue claimed it after the snapshot and is creating its microVM now. A snapshot already
/// `Starting` is an orphaned boot instead, and falls through to the teardown above.
fn claimed_boot_after_snapshot(snap: &Session, fresh: &Session) -> bool {
    fresh.status == SessionStatus::Starting && snap.status != SessionStatus::Starting
}

/// This pass's flip of an orphaned push it has just torn down: a compare-and-set against
/// `Publishing`, so a publish that claimed the colony while the teardown was in flight — it
/// holds no lifecycle lock — keeps its claim. Returns whether the flip landed.
fn mark_failed_after_restart(x: &mut Session) -> bool {
    if x.status != SessionStatus::Publishing {
        return false;
    }
    x.status = SessionStatus::Failed;
    x.error = Some(PUBLISH_LOST_TO_RESTART.into());
    true
}

/// This pass's flip of a colony whose microVM it has just removed: a compare-and-set against the
/// status the re-read saw, so a publish that claimed the colony while the teardown was in flight
/// keeps its claim. Returns whether the flip landed.
fn mark_stopped_after_restart(x: &mut Session, at_teardown: SessionStatus) -> bool {
    if x.status != at_teardown {
        return false;
    }
    x.status = SessionStatus::Stopped;
    x.error = Some(VM_GONE_AFTER_RESTART.into());
    true
}

/// microsandbox stops a colony's microVM on its own when the sandbox's max session length runs out, and the
/// host can stop one too. Without this the colony keeps whatever status it last had — usually `idle` — and
/// looks alive in the UI while nothing can reach it. Checking once a minute turns that into a `stopped`
/// colony the maintainer can resume, since the worktree outlives the microVM.
pub async fn watch_sandboxes(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        // A failed `msb ls` says nothing about the colonies, so leave them alone until it answers again.
        let Ok(running) = sandbox::running(&app.cfg.msb).await else {
            continue;
        };
        // Bound to a local first: a read guard in the `for` expression would live for the whole loop and
        // deadlock against update_session's write lock.
        let sessions = app.sessions.read().await.clone();
        for s in sessions {
            if !watch_candidate(&s) || running.contains(&s.sandbox) {
                continue;
            }
            // Both the session list and the running set above are a snapshot, and the list moves under
            // it while this loop works down the colonies. Take the colony's lifecycle lock and re-read
            // it, so the teardown and the `stopped` flip it earns happen together: a resume admitted by
            // that flip waits on the same lock and finds the teardown finished rather than booting a
            // microVM that the in-flight `msb rm --force` then removes, and a stop already tearing the
            // colony down holds the lock until its own teardown is done, keeping this flip out. The
            // `running` check stays on the snapshot — it is the detection, and a microVM does not come
            // back — but the flip below is a compare-and-set, so a publish that claimed the colony in
            // between (it holds no lifecycle lock) is not clobbered.
            let lifecycle = app.session_lock(&s.id).await;
            let _lifecycle = lifecycle.lock().await;
            let Some(s) = the_colony_to_stop(&app, &s.id).await else {
                continue;
            };
            app.session_log(
                &s.id,
                "error",
                "the microVM stopped; the worktree is kept, so this colony can be resumed".into(),
            )
            .await;
            teardown_vm(&app, &s).await;
            // The snapshot's attention flag, logged only when the flip below lands: a publish that
            // claimed the colony in the meantime keeps both its status and its flag.
            let had_attention = s.attention.clone();
            let landed = app
                .update_session(&s.id, |x| mark_stopped_after_teardown(x, s.status))
                .await
                .is_some_and(|(_, landed)| landed);
            if landed {
                app.note_cleared_attention(&s.id, had_attention).await;
            }
        }
    }
}

/// Whether the sandbox watchdog has business with a colony: a live one that is no longer booting. The
/// snapshot pass adds one more condition on top — the microVM must actually be gone from `msb ls` —
/// but the re-check under the lifecycle lock cannot re-ask that (`msb ls` was the tick's one look,
/// and a removed microVM does not come back), so both passes share this and only this.
fn watch_candidate(s: &Session) -> bool {
    s.status.is_live() && s.status != SessionStatus::Starting
}

/// The colony this tick is stopping, re-read under the lifecycle lock its caller holds: the snapshot
/// the tick worked from moves under it, so by teardown time both the record and its status may be
/// stale. A colony deleted since the snapshot is gone from the list, and one a resume or a publish
/// claimed in the meantime no longer has the live non-booting status the snapshot saw; neither is
/// this tick's to stop, and `None` says so.
async fn the_colony_to_stop(app: &Shared, id: &str) -> Option<Session> {
    let s = app.session(id).await?;
    watch_candidate(&s).then_some(s)
}

/// The watchdog's flip of a colony whose microVM it has just removed: a compare-and-set against the
/// status the snapshot read, so a publish that claimed the colony while the teardown was in flight —
/// it holds no lifecycle lock — keeps the `publishing` this flip must not clobber. Returns whether
/// the flip landed.
fn mark_stopped_after_teardown(x: &mut Session, at_teardown: SessionStatus) -> bool {
    if x.status != at_teardown {
        return false;
    }
    x.status = SessionStatus::Stopped;
    x.error = Some(VM_STOPPED_EARLY.into());
    // Only on the landed flip: a publish that claimed the colony mid-teardown keeps its claim,
    // and its attention flag with it.
    x.attention = None;
    true
}

/// Whether `total_usd` is past a colony's budget. A budget of `0` means no budget at all: there is no
/// dollar figure the harness can pick for someone else's deployment, so colonies are unlimited until the
/// operator names a number.
fn over_budget(total_usd: f64, budget_usd: f64) -> bool {
    budget_usd > 0.0 && total_usd > budget_usd
}

/// The one way the host stops a colony on its own decision — a past spend budget, a past host-disk quota.
/// The stop is claimed under `update_session`'s write lock, so of all the observers that see "over" only
/// the first tears the microVM down and an already stopped colony is never torn down again; `due` is the
/// final re-check under that lock. The status goes to `stopped` with a human-readable `error`, the
/// harness log says why, and the worktree is kept so the colony can be resumed. Returns whether this call
/// did the stopping.
pub(crate) async fn stop_colony(
    app: &Shared,
    s: &Session,
    due: impl FnOnce(&Session) -> bool,
    error: String,
    warn: String,
) -> bool {
    // The same discipline as the stop handler: the claim and the teardown under the colony's lifecycle
    // lock, so a resume that finds the `stopped` claim waits for the teardown to finish instead of
    // booting a microVM that this in-flight removal then takes with it.
    let lifecycle = app.session_lock(&s.id).await;
    let _lifecycle = lifecycle.lock().await;
    let mut attention = None;
    let claimed = app
        .update_session(&s.id, |x| {
            let due = x.status.is_live() && due(x);
            if due {
                x.status = SessionStatus::Stopped;
                x.error = Some(error);
                attention = x.clear_attention();
            }
            due
        })
        .await
        .is_some_and(|(_, due)| due);
    if claimed {
        app.session_log(&s.id, "warn", warn).await;
        app.note_cleared_attention(&s.id, attention).await;
        teardown_vm(app, s).await;
    }
    claimed
}

/// The budget check and its consequence, in one place, called wherever a colony's spend can change: after
/// the gateway records routed usage, when Claude's own cost arrives at turn end, and before the gateway
/// serves a request. A colony past its budget — the org's own if it set one, else the sandbox module's
/// default — is refused *and* stopped like the max-duration path stops one: microVM removed, status
/// `stopped`, a clear error, the worktree kept so it can be resumed once the budget is raised. Returns
/// whether the colony is over its budget.
pub async fn enforce_budget(app: &Shared, id: &str) -> bool {
    let Some(s) = app.session(id).await else { return false };
    let org = app.org_settings(&s.org);
    // Cloned, not held: the guard must not live across `stop_colony`, whose teardown takes the
    // colony's lifecycle lock and, through `app.mesh()`, the modules lock again — a read guard held
    // here would order modules before the colony lock against teardown's colony lock before modules,
    // and a modules writer arriving in between turns that inversion into a deadlock.
    let modules = app.modules.read().await.clone();
    let budget = orgs::budget_usd(&modules, &org);
    if !over_budget(s.total_cost_usd(), budget) {
        return false;
    }
    let (spent, source) = (s.total_cost_usd(), orgs::budget_source(&org));
    stop_colony(
        app,
        &s,
        |x| over_budget(x.total_cost_usd(), budget),
        format!("passed its spend budget of ${budget:.2} ({source}) at ${spent:.2} of model spend; the worktree is kept, so raise the budget and press Resume to continue"),
        format!("passed its spend budget of ${budget:.2} ({source}) at ${spent:.2}; stopping the colony, which can be resumed once the budget is raised"),
    )
    .await;
    true
}

/// Adds one gateway response's spend to the colony and re-checks its budget. A response with nothing
/// priced in it (a provider without pricing) changes nothing: its tokens still reach the session through
/// the runner's per-model usage.
pub async fn record_routed_usage(app: &Shared, colony: &str, provider: &providers::Provider, usage: providers::Usage) {
    let cost = provider.cost_usd(usage);
    if cost <= 0.0 {
        return;
    }
    let Some((session, _)) = app
        .update_session(colony, |x| {
            x.routed_cost_usd = Some(x.routed_cost_usd.unwrap_or_default() + cost);
        })
        .await
    else {
        return;
    };
    // Told to the append-only journal now, while the colony still exists to name its org: the
    // routed dollar has to survive the cleanup or delete that will forget it.
    spend::record_routed(app, &session.org, cost).await;
    enforce_budget(app, colony).await;
}

/// Whether `bytes` on the host is past a colony's host-disk quota. A quota of `0` means no quota at all:
/// how much disk a colony deserves is a decision about someone else's deployment, so colonies are
/// unlimited until the operator names a size — the same opt-in the spend budget uses.
fn over_host_disk(bytes: u64, quota_bytes: u64) -> bool {
    quota_bytes > 0 && bytes > quota_bytes
}

/// What a colony leaves on the host: its worktree (bind-mounted rw at `/workspace` inside the microVM,
/// where everything the colony builds lands) plus its session directory (`out/`, `vm/`, and the
/// append-only logs). The microVM's root disk is a separate limit, microsandbox's `--root-disk`. Walked
/// on the blocking pool: it is plain IO over trees that can be gigabytes.
async fn host_footprint_bytes(app: &App, s: &Session) -> u64 {
    let (worktree, session_dir) = (PathBuf::from(&s.worktree), app.session_dir(&s.id));
    // A walk that never finishes (a shutdown) measures 0, which can only under-report — never a reason to
    // stop a colony.
    tokio::task::spawn_blocking(move || dir_size(&worktree) + dir_size(&session_dir))
        .await
        .unwrap_or(0)
}

/// The host-disk check and its consequence, in one place, called from [`watch_host_disks`] — the only
/// observer, so the measurement is fresh whenever it matters. With no quota for the colony — neither the
/// org's own nor the sandbox module's default — it does nothing at all: the measurement is a full walk of
/// trees a `cargo build` can make gigabytes deep, and without a quota it would run every five minutes
/// purely to fill in a UI number, so `host_disk_bytes` stays `None` until the operator names a size.
/// Under a quota it records what the colony leaves on the host, whatever the verdict, and stops a colony
/// past it through the same [`stop_colony`] the budget uses: microVM removed, status `stopped`, a clear
/// error naming the quota and the measured size, the worktree kept, because deleting a colony's work is
/// the operator's call.
async fn enforce_host_disk(app: &Shared, s: &Session) {
    let org = app.org_settings(&s.org);
    let modules = app.modules.read().await.clone();
    let quota = orgs::host_disk(&modules, &org);
    // A quota of 0 is no quota, so there is no verdict to reach and the walk would be real IO for
    // nothing; skipping it keeps stock deployments from re-reading every colony's tree every tick.
    if quota == 0 {
        return;
    }
    let measured = host_footprint_bytes(app, s).await;
    if s.host_disk_bytes != Some(measured) {
        app.update_session(&s.id, |x| x.host_disk_bytes = Some(measured)).await;
    }
    if !over_host_disk(measured, quota) {
        return;
    }
    let source = orgs::host_disk_source(&org);
    let (quota, measured) = (format_disk_size(quota), format_disk_size(measured));
    // There is nothing colony-held to re-check under the lock that the measurement has not already seen,
    // and the loop is the only caller, so `due` has nothing left to ask beyond `stop_colony`'s own
    // still-live check.
    stop_colony(
        app,
        s,
        |_| true,
        format!(
            "passed its host-disk quota of {quota} ({source}) at {measured} on the host, worktree and session files together; the worktree is kept, so clean up or raise the quota and press Resume to continue"
        ),
        format!(
            "passed its host-disk quota of {quota} ({source}) at {measured} on the host; stopping the colony, which can be resumed once the quota is raised or the worktree cleaned up"
        ),
    )
    .await;
}

/// The host-disk check runs every five minutes — far slower than the 60-second loops on purpose: unlike
/// them it walks the worktree and session directory of every live colony under a quota, real IO over
/// trees a `cargo build` can make gigabytes deep, and growth past a quota is a matter of minutes and
/// hours, not seconds. A colony still booting is skipped, like the other loops skip it. Missed ticks are
/// skipped, so a busy machine never piles up walks.
pub async fn watch_host_disks(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(300));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        // Bound to a local first: a read guard in the `for` expression would live for the whole loop and
        // deadlock against update_session's write lock.
        let sessions = app.sessions.read().await.clone();
        for s in sessions
            .into_iter()
            .filter(|s| s.status.is_live() && s.status != SessionStatus::Starting)
        {
            enforce_host_disk(&app, &s).await;
        }
    }
}

/// A colony can be resumed while its worktree is still on disk and no microVM is running for it.
pub(crate) fn can_resume(status: SessionStatus, cleaned_up: bool, has_worktree: bool) -> bool {
    matches!(status, SessionStatus::Stopped | SessionStatus::Failed) && !cleaned_up && has_worktree
}

/// Moves a finished microVM's event log aside, so a resumed colony's `seq` numbering starts from 1
/// again. A failure here must stop the resume, not degrade it: agentd keeps its event store inside
/// the microVM, so a resumed colony numbers from 1 regardless, and with the stale log still in
/// place `Runtime::load` picks up the previous life's maximum and drops every new event until the
/// colony has out-produced it.
pub(crate) fn rotate_events(dir: &std::path::Path) -> std::io::Result<()> {
    let events = dir.join("events.jsonl");
    if !events.exists() {
        return Ok(());
    }
    for n in 1..1000 {
        let target = dir.join(format!("events-{n}.jsonl"));
        if !target.exists() {
            // The error goes back to the caller, which refuses the resume rather than carry on; a
            // rotation that silently failed would drop events instead of just replaying old ones.
            faults::check(&events, Op::Rename)?;
            return std::fs::rename(&events, &target);
        }
    }
    // Unreachable in practice — getting here means a colony has been resumed a thousand times
    // without one rotation being reported — but falling out silently would be a no-op that the
    // caller reads as success, and a stale log left in place drops the resumed colony's events.
    // So this is an error like any other failed rotation, and names the directory that filled up.
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "every archive slot in {} is taken (events-1.jsonl through events-999.jsonl), so {} has nowhere to go",
            dir.display(),
            events.display()
        ),
    ))
}

/// The current run epoch of a colony's event log: 1 for a fresh colony, one past the highest
/// archived `events-N.jsonl` after that. Each successful resume rotates `events.jsonl` aside into
/// the next archive slot (see `rotate_events`), so the archive count is the resume count, and the
/// epoch survives harness restarts with no migration. Read straight from the session directory so
/// an events socket can learn it before any Runtime exists (runtimes are created lazily).
pub(crate) fn run_epoch_for_dir(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 1;
    };
    let mut max = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name
            .to_str()
            .and_then(|n| n.strip_prefix("events-"))
            .and_then(|n| n.strip_suffix(".jsonl"))
        else {
            continue;
        };
        if let Ok(n) = rest.parse::<u64>() {
            max = max.max(n);
        }
    }
    max + 1
}

/// Which `since` cursor an events socket replays from, given the epoch the client last saw. A
/// client reconnecting after a resume names a stale epoch — and its `since` is a rank in the
/// retired run's per-run numbering, meaningless in the new run — so it replays from 0 instead of
/// dropping the new run's first events. Absent (legacy clients) or 0 ("unknown") keeps `since`.
pub(crate) fn effective_since(client_epoch: Option<u64>, current_epoch: u64, since: u64) -> u64 {
    match client_epoch {
        Some(e) if e != 0 && e != current_epoch => 0,
        _ => since,
    }
}

pub async fn resume(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    let s = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    if !can_resume(s.status, s.cleaned_up, s.git_admin_dir.is_some()) {
        return Err(client_error(StatusCode::CONFLICT, RESUME_CONFLICT));
    }
    let previous_status = s.status;
    // Past the limit a colony waits its turn rather than being refused, as in `create`; `run_queue`
    // resumes it later.
    let modules = app.modules.read().await.clone();
    let max_parallel = orgs::global_max_parallel(&modules) as usize;
    // Resolved before the admission lock: `org_settings` reads the orgs file with blocking IO.
    let org_settings = app.org_settings(&s.org);
    let org_limit = orgs::org_max_parallel(&org_settings);
    let repo_limit = crate::queue::repo_limit(&modules, &org_settings);
    // The colony's lifecycle lock, held from here to the end of the handler. The claim below is the
    // moment this colony gains a microVM (or a queue slot), and everything between it and the boot
    // spawn — dropping the old agent link, rotating the event log, the revert if the rotation fails —
    // assumes the colony stays claimed. Holding the guard through the `tokio::spawn` keeps that claim
    // and the handoff to `boot` in one critical section; it is belt-and-braces, not the part that
    // closes a race, because a stop landing before the spawn only flips the status out of `starting`
    // and the boot's first `ensure_starting` then bails before it creates anything. The race this lock
    // exists for is on the stopping side — a resume admitted between a stop's status flip and its
    // teardown — and the stop holds this same lock across both, so this resume cannot be admitted into
    // that window. Once the spawn has happened the boot's `ensure_starting` checkpoints take over.
    // Every early return below (404, 409) simply drops it.
    let lifecycle = app.session_lock(&id).await;
    let _lifecycle = lifecycle.lock().await;
    // The claim comes first, exactly as the publish claim does: `failed` is both resumable and
    // publishable, so a resume landing just after a publish claimed the colony must be refused
    // rather than overwrite `publishing`. Nothing outside the list is touched until it succeeds, so
    // a refused resume leaves the agent link and the event log as it found them.
    // The closure distinguishes its two non-outcomes: `Err` is the not-resumable re-check, answered with
    // a 409 below, while `Ok(None)` is a colony that has vanished, which stays a 404.
    let claimed = with_slot(
        &app.sessions,
        &s.org,
        &s.repo,
        max_parallel,
        org_limit,
        repo_limit,
        |sessions, room| {
            // Counted before the flip: the colony itself is Stopped or Failed here, so it isn't counted.
            let waiting = sessions.iter().filter(|other| other.status == SessionStatus::Queued).count();
            let Some(x) = sessions.iter_mut().find(|x| x.id == id) else {
                return Ok(None);
            };
            // Re-checked under the lock: the colony must still be resumable when the slot is claimed.
            if !can_resume(x.status, x.cleaned_up, x.git_admin_dir.is_some()) {
                return Err(RESUME_CONFLICT); // another resume won the race between the handler and the lock
            }
            x.status = if room {
                SessionStatus::Starting
            } else {
                SessionStatus::Queued
            };
            x.error = None;
            x.attention = None;
            x.mesh = None;
            x.local_port = None;
            // The last boot's phases would read as this one's under `starting` or `queued`.
            x.boot_timing = None;
            x.updated_at = Utc::now();
            Ok(Some((x.clone(), room, waiting)))
        },
    )
    .await;
    let (s, admitted, waiting) = match claimed {
        Ok(Some(claimed)) => claimed,
        Ok(None) => return Err(client_error(StatusCode::NOT_FOUND, "no such session")),
        Err(message) => return Err(client_error(StatusCode::CONFLICT, message)),
    };
    app.persist_and_broadcast(&s).await;
    // The colony is ours: only now is the old agent link dropped and the event log rotated.
    let runtime = app.runtimes.lock().await.remove(&id);
    if let Some(rt) = &runtime {
        rt.stop.send_replace(true);
        // Retire pre-existing event sockets: they hold this Runtime and read from its broadcast
        // channels, so they can never see the new run's events. `stop` cannot do this —
        // `teardown_vm` sets it on a plain stop too, where sockets stay open on purpose — so this
        // dedicated signal, sent only on the resume-retire path, closes them, and the clients
        // reconnect into the new epoch.
        rt.retired.send_replace(true);
    }
    // Rotation must not fail silently (see `rotate_events`): with the stale log still in place the
    // resumed colony's events are dropped as already seen. A colony reads as Stopped before
    // `teardown_vm`'s shutdown POST has finished, so its link task can still be draining agentd's
    // last events and appending under the runtime's `file_lock`; hold that lock across the rename so
    // an in-flight append cannot straddle it and resurrect an `events.jsonl` holding the old life's
    // seq. Nothing under the guard may itself take `file_lock` (`session_log` does), so the failure
    // reporting stays outside it.
    let dir = app.session_dir(&id);
    let rotated = {
        let _file_lock = match runtime.as_ref() {
            Some(rt) => Some(rt.file_lock.lock().await),
            None => None,
        };
        rotate_events(&dir)
    };
    if let Err(e) = rotated {
        // The claim already moved this colony off its old status — and may have taken a parallel
        // slot with it — so put it back, or the colony is left mid-resume and `can_resume` refuses
        // the retry this error asks for.
        if let Some((s, ())) = app.update_session(&id, |x| x.status = previous_status).await {
            app.persist_and_broadcast(&s).await;
        }
        let e = anyhow::Error::from(e);
        let message = format!(
            "could not move the old event log aside ({e}); the colony was not resumed — move {} aside yourself and try again",
            dir.join("events.jsonl").display()
        );
        app.storage_failed("rotate the old event log", &e).await;
        app.session_log(&id, "error", message.clone()).await;
        return Err(client_error(StatusCode::INTERNAL_SERVER_ERROR, &message));
    }
    if admitted {
        app.session_log(&id, "info", "resuming: booting a fresh microVM on the kept worktree".into())
            .await;
        tokio::spawn(boot(app.clone(), id, true));
    } else {
        let ahead = if waiting == 0 {
            String::new()
        } else {
            format!(", behind {waiting} already waiting")
        };
        let limits = crate::queue::limits_message(max_parallel, org_limit, repo_limit);
        app.session_log(
            &id,
            "info",
            format!("queued: {limits}{ahead}; the colony resumes when a slot frees up"),
        )
        .await;
    }
    Ok(Json(s))
}

/// What a stop did. A colony that is already over is the outcome a stop asks for, so a script's retry,
/// a double click or a stop racing the colony's own finish is told `already_stopped` rather than
/// handed an error it has to special-case.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopResult {
    Stopped,
    AlreadyStopped,
}

/// The stop's answer: the result beside the colony it leaves, flattened so callers that read the
/// reply as a `Session` keep working.
#[derive(Debug, serde::Serialize)]
pub struct StopReply {
    pub result: StopResult,
    #[serde(flatten)]
    pub session: Session,
}

pub async fn stop(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<StopReply> {
    // Checked before the lifecycle lock so a stop of an unknown id does not leave a lock slot behind
    // for a colony that does not exist; the claim below stays the authority.
    if app.session(&id).await.is_none() {
        return Err(client_error(StatusCode::NOT_FOUND, "no such session"));
    }
    // The lifecycle lock across the claim and the teardown: `stop` flips the colony to `stopped` — a
    // status `can_resume` accepts — before `teardown_vm` removes the microVM, so a resume must wait
    // for this lock instead of booting a microVM that the in-flight `msb rm --force` then removes
    // under the same deterministic name. The early returns drop it, and safely so: they claim nothing
    // and run no teardown, so there is no state change of ours here for a boot to race — the colony
    // keeps whatever microVM it may still have (a teardown swallows its errors, so `stopped` does not
    // prove the VM is gone) for the next stop or a publish to collect.
    let lifecycle = app.session_lock(&id).await;
    let _lifecycle = lifecycle.lock().await;
    let mut attention = None;
    let Some((s, was)) = app
        .update_session(&id, |x| {
            let was = x.status;
            if was.is_live() || was == SessionStatus::Queued {
                x.status = SessionStatus::Stopped;
                attention = x.clear_attention();
            }
            was
        })
        .await
    else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such session"));
    };
    app.note_cleared_attention(&id, attention).await;
    let stopped = |session| {
        Json(StopReply {
            result: StopResult::Stopped,
            session,
        })
    };
    // A queued colony never started, so there is no microVM to remove.
    if was == SessionStatus::Queued {
        app.session_log(&id, "info", "left the queue before it started".into()).await;
        return Ok(stopped(app.session(&id).await.unwrap_or(s)));
    }
    // `was` is read under the same write lock that would have claimed the colony, so the status
    // reported here is the one this stop found, not a later one.
    if was.is_terminal() {
        return Ok(Json(StopReply {
            result: StopResult::AlreadyStopped,
            session: s,
        }));
    }
    // Only `publishing` is left: neither live nor over, its push in flight and settling on its own.
    if !was.is_live() {
        return Err(client_error(StatusCode::CONFLICT, "session is not running"));
    }
    app.session_log(&id, "info", "stopping: removing the microVM (the worktree is kept)".into())
        .await;
    teardown_vm(&app, &s).await;
    Ok(stopped(app.session(&id).await.unwrap_or(s)))
}

/// Whether a colony in this state can be cleaned up — its worktree and local branch freed. A live or
/// publishing colony has a microVM or a push in flight, and a queued colony is still waiting to start:
/// cleaning one up would leave it queued with nothing left to start on, and the next queue tick would
/// start it anyway, cleanup undone. Stop first, which takes a queued colony out of the queue.
fn cleanable(status: SessionStatus) -> bool {
    !status.is_live() && status != SessionStatus::Publishing && status != SessionStatus::Queued
}

/// The work of the `cleanup` handler, shared with the auto-reclaim tick: claim the colony as cleaned
/// up under its lifecycle lock, remove its worktree under its repo lock, and re-read the record.
/// Errors named exactly `no such session` mean the colony is gone (NOT_FOUND at the HTTP layer) and
/// `stop the session first` means it is not cleanable (CONFLICT); anything else is the worktree
/// removal failing and surfaces as a 500. The wrapper below maps these; keep the messages in sync.
pub(crate) async fn cleanup_one(app: &Shared, id: &str) -> anyhow::Result<Session> {
    let s = app.session(id).await.ok_or_else(|| anyhow::anyhow!("no such session"))?;
    if !cleanable(s.status) {
        return Err(anyhow::anyhow!("stop the session first"));
    }
    // The lifecycle lock first, and the claim before the removal: `cleaned_up` is exactly what
    // `can_resume` checks, so setting it while holding this lock means a resume waiting on it is
    // refused outright rather than admitted onto the worktree this handler is about to delete — a
    // boot bind-mounts that worktree rw at /workspace. Removing first, as this used to, let a resume
    // claim `starting` on a worktree that was mid-deletion and then landed `cleaned_up` on a live
    // colony, which `can_resume` refuses forever. An already-cleaned-up colony still cleans up
    // cleanly: the claim does not look at `cleaned_up`, the worktree is already gone, and
    // `remove_worktree` no-ops.
    let lifecycle = app.session_lock(id).await;
    let _lifecycle = lifecycle.lock().await;
    // Compare-and-set under the write lock: the pre-check above read a snapshot, and a resume or a
    // stop may have claimed the colony in the meantime. The previous `cleaned_up` comes back so a
    // failed removal can put it back.
    let (s, (claimed, was_cleaned)) = app
        .update_session(id, |x| {
            let allowed = cleanable(x.status);
            let was_cleaned = x.cleaned_up;
            if allowed {
                x.cleaned_up = true;
            }
            (allowed, was_cleaned)
        })
        .await
        .ok_or_else(|| anyhow::anyhow!("no such session"))?;
    if !claimed {
        return Err(anyhow::anyhow!("stop the session first"));
    }
    let removed = {
        let lock = app.repo_lock(&s.repo).await;
        let _guard = lock.lock().await;
        github::remove_worktree(app, &s).await
    };
    if let Err(e) = removed {
        // The colony is marked cleaned up but its worktree may still be there, and `can_resume` reads
        // `cleaned_up` — leaving the flag set would strand a colony that is really resumable. Put the
        // old value back, as `delete` re-inserts the record when its worktree removal fails. A colony
        // deleted while its worktree was being removed makes that a silent no-op: the record is gone,
        // and there is nothing left to restore.
        app.update_session(id, |x| x.cleaned_up = was_cleaned).await;
        return Err(
            e.context("could not remove the colony's worktree; the colony is left cleanable so the cleanup can be retried")
        );
    }
    // The answer is read after the removal, not taken from the claim's clone: `delete` takes no
    // lifecycle lock, so a colony deleted while its worktree was being removed must come back as the
    // 404 the old removal-first order got from its post-removal update, not as a 200 naming a colony
    // that no longer exists.
    let s = app.session(id).await.ok_or_else(|| anyhow::anyhow!("no such session"))?;
    Ok(s)
}

pub async fn cleanup(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    match cleanup_one(&app, &id).await {
        Ok(s) => Ok(Json(s)),
        // The anyhow error only carries the message, so the HTTP status is recovered from the
        // sentinel messages `cleanup_one` documents above; anything else is a failed removal.
        Err(e) => Err(match e.to_string().as_str() {
            "no such session" => client_error(StatusCode::NOT_FOUND, "no such session"),
            "stop the session first" => client_error(StatusCode::CONFLICT, "stop the session first"),
            _ => e.into(),
        }),
    }
}

/// Whether a colony in this state can be deleted. A live or publishing colony has a microVM or a push in flight.
fn deletable(status: SessionStatus) -> bool {
    !status.is_live() && status != SessionStatus::Publishing
}

/// Forgets a colony: its worktree and local branch (unless already cleaned up), its chat and harness logs, and its
/// record. A pull request it opened, and any branch it pushed, stay on GitHub. Live and publishing colonies must be
/// stopped first.
pub async fn delete(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    // Out of the list before any file is touched, so neither the queue nor Resume can start it meanwhile.
    let (at, s) = {
        let mut sessions = app.sessions.write().await;
        let at = sessions
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
        if !deletable(sessions[at].status) {
            return Err(client_error(StatusCode::CONFLICT, "stop the colony first"));
        }
        (at, sessions.remove(at))
    };
    if !s.cleaned_up {
        let removed = {
            let lock = app.repo_lock(&s.repo).await;
            let _guard = lock.lock().await;
            github::remove_worktree(&app, &s).await
        };
        if let Err(e) = removed {
            // Nothing is lost yet: put the record back where it was, so the colony can be cleaned up or retried.
            let mut sessions = app.sessions.write().await;
            let at = at.min(sessions.len());
            sessions.insert(at, s);
            return Err(e
                .context("could not remove the colony's worktree; nothing was deleted")
                .into());
        }
    }
    if let Err(e) = app.persist_sessions().await {
        // The worktree may already be gone — it is removed before the list is saved — but the colony
        // goes back in the list either way: nothing reports a colony forgotten while the save that
        // forgets it did not happen, and the deletion can be retried once storage works again.
        let cleaned = s.cleaned_up;
        {
            let mut sessions = app.sessions.write().await;
            let at = at.min(sessions.len());
            sessions.insert(at, s);
        }
        let e = e.context(if cleaned {
            "could not save the session list; the colony is kept"
        } else {
            "could not save the session list; the colony is kept, but its worktree was already removed"
        });
        app.storage_failed("save the session list", &e).await;
        return Err(e.into());
    }
    if let Some(rt) = app.runtimes.lock().await.remove(&id) {
        rt.stop.send_replace(true);
    }
    // The colony was non-live (`deletable` refused live ones), so any microVM still under its
    // deterministic sandbox name is an orphan no reaper will collect — e.g. a stop that raced a
    // boot's unguarded stretch — and the id is never reused, so `--replace` never masks it. Reap
    // it now that the record is gone. `teardown_vm` swallows its own errors, so this can neither
    // fail the deletion nor trigger the re-insert paths above.
    teardown_vm(&app, &s).await;
    // The record is gone, so drop its lifecycle lock slot and the map does not grow without bound on a
    // long-running mothership. A bound, not a guarantee: `watch_sandboxes` takes this lock before its
    // existence re-check, so a reaper tick that snapshotted the colony before this delete can
    // resurrect an empty entry afterwards, and a resume, stop or cleanup that passed its pre-check in
    // that window can do the same. The cost of a raced delete is one small idle mutex and nothing
    // more. Safe even if a guard were still held somewhere: the `Arc` keeps that mutex alive for as
    // long as its holder needs it.
    app.session_locks.lock().await.remove(&id);
    let dir = app.session_dir(&id);
    let leftover = match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(format!("{}: {e}", dir.display())),
    };
    Ok(Json(json!({"deleted": id, "leftover": leftover})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::{admit_resume, app_with_colony, colony, stopped_colony_with_worktree};
    use crate::util::short_id;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[test]
    fn only_colonies_with_nothing_running_can_be_deleted() {
        use SessionStatus::*;
        for status in [Queued, Stopped, Failed, NoChanges, PrOpened, Merged, Closed] {
            assert!(deletable(status), "{status:?}");
        }
        for status in [Starting, Running, WaitingForAnswer, Idle, Publishing] {
            assert!(!deletable(status), "{status:?}");
        }
    }

    #[tokio::test]
    async fn deleting_a_stopped_colony_runs_the_vm_teardown_and_forgets_the_record() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        // Already cleaned up, so no worktree removal stands between the record and the
        // teardown: the test exercises the reap path (`msb` is absent here, and
        // `sandbox::remove` swallows that, with no mesh or local port there is no agentd
        // call either) and the deletion must still succeed.
        app.update_session("abc", |s| s.cleaned_up = true).await.unwrap();
        let out = delete(State(app.clone()), Path("abc".to_string())).await.unwrap();
        assert_eq!(out.0["deleted"], json!("abc"), "the deletion reports the colony");
        assert!(
            app.session("abc").await.is_none(),
            "the record is gone, so the id is never reused"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn only_a_stopped_colony_that_still_has_its_worktree_can_be_resumed() {
        assert!(can_resume(SessionStatus::Stopped, false, true));
        assert!(can_resume(SessionStatus::Failed, false, true));
        assert!(!can_resume(SessionStatus::Stopped, true, true), "cleaned up");
        assert!(!can_resume(SessionStatus::Stopped, false, false), "no worktree");
        for status in [
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::WaitingForAnswer,
            SessionStatus::Idle,
            SessionStatus::Publishing,
            SessionStatus::PrOpened,
            SessionStatus::Merged,
            SessionStatus::Closed,
            SessionStatus::NoChanges,
        ] {
            assert!(!can_resume(status, false, true), "{status:?}");
        }
    }

    #[test]
    fn only_a_colony_that_has_finished_can_be_cleaned_up() {
        use SessionStatus::*;
        for status in [Starting, Running, WaitingForAnswer, Idle, Publishing, Queued] {
            assert!(!cleanable(status), "{status:?}");
        }
        for status in [Stopped, Failed, NoChanges, PrOpened] {
            assert!(cleanable(status), "{status:?}");
        }
    }

    #[test]
    fn a_budget_of_zero_is_unlimited_and_only_a_positive_one_can_be_passed() {
        assert!(!over_budget(4.99, 5.0), "under the budget");
        assert!(!over_budget(5.0, 5.0), "exactly at the budget is still within it");
        assert!(over_budget(5.01, 5.0), "the first cent past the budget is over it");
        assert!(!over_budget(1_000.0, 0.0), "0 means no budget at all");
        assert!(!over_budget(1_000.0, -5.0), "a negative budget is no budget either");
    }

    #[test]
    fn a_host_disk_quota_of_zero_is_unlimited_and_only_a_positive_one_can_be_passed() {
        assert!(!over_host_disk(100, 200), "under the quota");
        assert!(!over_host_disk(200, 200), "exactly at the quota is still within it");
        assert!(over_host_disk(201, 200), "the first byte past the quota is over it");
        assert!(!over_host_disk(1 << 40, 0), "0 means no quota at all");
    }

    #[test]
    fn a_failed_event_log_rotation_is_reported_not_swallowed() {
        let dir = std::env::temp_dir().join(format!("colonizer-rotate-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), "{\"seq\":1}\n").unwrap();
        let _guard = faults::inject("events.jsonl", Op::Rename, || std::io::Error::from_raw_os_error(5));
        assert!(rotate_events(&dir).is_err());
        drop(_guard);
        assert_eq!(
            std::fs::read_to_string(dir.join("events.jsonl")).unwrap(),
            "{\"seq\":1}\n",
            "the log is untouched"
        );
        rotate_events(&dir).unwrap();
        assert!(!dir.join("events.jsonl").exists());
        assert_eq!(std::fs::read_to_string(dir.join("events-1.jsonl")).unwrap(), "{\"seq\":1}\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_failed_event_log_rotation_refuses_the_resume_and_leaves_the_colony_stopped() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.git_admin_dir = Some("/tmp/wt".into()))
            .await
            .unwrap();
        std::fs::write(app.session_dir("abc").join("events.jsonl"), "{\"seq\":7}\n").unwrap();
        let _guard = faults::inject("events.jsonl", Op::Rename, || std::io::Error::from_raw_os_error(5));
        let err = resume(State(app.clone()), Path("abc".to_string())).await.unwrap_err();
        let body = err.1.to_string();
        assert!(body.contains("the colony was not resumed"), "{body}");
        assert!(body.contains("aside yourself and try again"), "{body}");
        assert!(
            app.storage_alert.read().await.is_some(),
            "the failure is recorded, not swallowed"
        );
        assert_eq!(
            app.session("abc").await.unwrap().status,
            SessionStatus::Stopped,
            "nothing about the colony changed"
        );
        assert_eq!(
            std::fs::read_to_string(app.session_dir("abc").join("events.jsonl")).unwrap(),
            "{\"seq\":7}\n",
            "the old log is left where it was"
        );
        let log = std::fs::read_to_string(app.session_dir("abc").join("harness.jsonl")).unwrap();
        assert!(log.contains("could not move the old event log aside"), "{log}");
        drop(_guard);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_rotation_with_every_archive_slot_taken_is_an_error_and_leaves_the_log_in_place() {
        let dir = std::env::temp_dir().join(format!("colonizer-rotate-full-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), "{\"seq\":1}\n").unwrap();
        for n in 1..1000 {
            std::fs::write(dir.join(format!("events-{n}.jsonl")), "").unwrap();
        }
        let message = rotate_events(&dir).unwrap_err().to_string();
        assert!(message.contains("every archive slot"), "{message}");
        assert!(
            message.contains(&dir.display().to_string()),
            "the error names the directory that filled up: {message}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("events.jsonl")).unwrap(),
            "{\"seq\":1}\n",
            "the log is left in place, so the resume stays refused instead of dropping events"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_run_epoch_is_one_past_the_highest_archived_event_log() {
        let dir = std::env::temp_dir().join(format!("colonizer-epoch-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(run_epoch_for_dir(&dir), 1, "a fresh colony is epoch 1");
        std::fs::write(dir.join("events.jsonl"), "{\"seq\":1}\n").unwrap();
        assert_eq!(run_epoch_for_dir(&dir), 1, "the live log is the current run, not an archive");
        std::fs::write(dir.join("events-1.jsonl"), "").unwrap();
        std::fs::write(dir.join("events-2.jsonl"), "").unwrap();
        assert_eq!(run_epoch_for_dir(&dir), 3, "two resumes are epoch 3");
        std::fs::write(dir.join("events-9.jsonl"), "").unwrap();
        assert_eq!(run_epoch_for_dir(&dir), 10, "only the highest archive counts, gaps aside");
        assert_eq!(
            run_epoch_for_dir(&dir.join("missing")),
            1,
            "an unreadable directory reads as a fresh colony"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_stale_epoch_replays_from_zero_while_a_current_unknown_or_absent_epoch_keeps_since() {
        assert_eq!(effective_since(Some(3), 3, 41), 41, "a matching epoch keeps the cursor");
        assert_eq!(
            effective_since(Some(2), 3, 41),
            0,
            "a stale epoch replays from zero: the cursor is a retired run's rank"
        );
        assert_eq!(effective_since(Some(4), 3, 41), 0, "an epoch from the future is stale too");
        assert_eq!(
            effective_since(Some(0), 3, 41),
            41,
            "0 means the client does not know its epoch"
        );
        assert_eq!(effective_since(None, 3, 41), 41, "absent means a legacy client");
        assert_eq!(effective_since(Some(2), 3, 0), 0, "already at zero stays at zero");
    }

    #[tokio::test]
    async fn a_resume_bumps_the_run_epoch_and_retires_the_old_runtime() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.git_admin_dir = Some("git".into()))
            .await
            .unwrap();
        std::fs::write(app.session_dir("abc").join("events.jsonl"), "{\"seq\":7}\n").unwrap();
        assert_eq!(run_epoch_for_dir(&app.session_dir("abc")), 1);
        // A runtime in the map, as a just-stopped colony still has while its link task drains.
        let rt_before = app.runtime("abc").await;
        // No free slot, so the resume queues instead of spawning a boot whose git and `msb` work
        // would flip the colony's status under the assertions below.
        fill_the_parallel_limit(&app).await;
        let _ = resume(State(app.clone()), Path("abc".to_string())).await.unwrap();
        assert!(app.session_dir("abc").join("events-1.jsonl").exists(), "the rotation landed");
        assert_eq!(
            run_epoch_for_dir(&app.session_dir("abc")),
            2,
            "one successful resume is epoch 2"
        );
        assert!(
            *rt_before.retired.borrow(),
            "the old runtime is retired, so its event sockets close and reconnect"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_resume_does_not_rotate_while_another_task_holds_the_runtime_s_file_lock() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.git_admin_dir = Some("/tmp/wt".into()))
            .await
            .unwrap();
        std::fs::write(app.session_dir("abc").join("events.jsonl"), "{\"seq\":7}\n").unwrap();
        // A runtime in the map, as a just-stopped colony still has while its link task drains.
        let rt = app.runtime("abc").await;
        // Stand in for an in-flight append: the lock another task would hold.
        let append = rt.file_lock.lock().await;
        let resumed = tokio::spawn(resume(State(app.clone()), Path("abc".to_string())));
        // Purely cooperative, so there is no timing bet: each yield lets the resume advance to the
        // lock it cannot take. With the lock held it can neither have finished nor have renamed.
        for _ in 0..64 {
            tokio::task::yield_now().await;
            assert!(!resumed.is_finished(), "the resume waits for the runtime's file_lock");
            assert!(
                app.session_dir("abc").join("events.jsonl").exists(),
                "no rotation while an append holds the lock"
            );
        }
        drop(append);
        if let Err(e) = resumed.await.unwrap() {
            panic!("the resume failed once the lock freed up: {:#}", e.1);
        }
        assert!(
            !app.session_dir("abc").join("events.jsonl").exists(),
            "the rotation went ahead once the lock freed up"
        );
        assert_eq!(
            std::fs::read_to_string(app.session_dir("abc").join("events-1.jsonl")).unwrap(),
            "{\"seq\":7}\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_failed_save_keeps_the_colony_listed_and_the_delete_reports_failure() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.cleaned_up = true).await.unwrap();
        let _guard = faults::inject("sessions.json", Op::Write, || {
            std::io::Error::from(std::io::ErrorKind::StorageFull)
        });
        let result = delete(State(app.clone()), Path("abc".to_string())).await;
        assert!(result.unwrap_err().1.to_string().contains("the colony is kept"));
        assert!(app.session("abc").await.is_some(), "the colony goes back in the list");
        assert!(app.storage_alert.read().await.is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_resumes_cannot_overshoot_the_limit() {
        let max_parallel = 3;
        let resumable = 12;
        let sessions = Arc::new(RwLock::new(
            (0..resumable)
                .map(|i| stopped_colony_with_worktree("acme", format!("resume-{i}")))
                .collect::<Vec<_>>(),
        ));
        let barrier = Arc::new(tokio::sync::Barrier::new(resumable));
        let mut tasks = Vec::new();
        for i in 0..resumable {
            let (sessions, barrier) = (sessions.clone(), barrier.clone());
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                admit_resume(&sessions, "acme", max_parallel, None, &format!("resume-{i}")).await;
            }));
        }
        for task in tasks {
            task.await.expect("resume task joined");
        }
        let done = sessions.read().await;
        assert_eq!(
            done.iter().filter(|s| s.status.is_live()).count(),
            max_parallel,
            "the live count never passes the limit"
        );
        assert_eq!(
            done.iter().filter(|s| s.status == SessionStatus::Queued).count(),
            resumable - max_parallel,
            "the resumes that did not fit queued instead of being refused"
        );
    }

    // -- the colony lifecycle lock ---------------------------------------------------------------

    /// Pushes live colonies until the app's global parallel limit is full, so an admission that would
    /// boot queues instead: a real boot does git and `msb` work in the temp root and would move the
    /// colony's status around while the test is asserting on it. The mutual exclusion under test does
    /// not care which way the winning resume lands. Returns the limit it filled.
    async fn fill_the_parallel_limit(app: &Shared) -> usize {
        let max_parallel = orgs::global_max_parallel(&app.modules.read().await.clone()) as usize;
        let mut sessions = app.sessions.write().await;
        for i in sessions.len()..sessions.len() + max_parallel {
            let mut filler = colony("acme", SessionStatus::Idle);
            filler.id = format!("filler-{i}");
            sessions.push(filler);
        }
        max_parallel
    }

    /// The body of the lock-wait test each lifecycle handler has to pass: with the colony's lifecycle
    /// lock held elsewhere, the handler must neither finish nor make its claim, and once the lock
    /// frees up the claim must go through. Purely cooperative, so there is no timing bet: each yield
    /// lets the handler advance to the lock it cannot take.
    async fn a_handler_held_behind_the_colony_lifecycle_lock_claims_nothing_until_it_is_dropped(
        app: &Shared,
        id: &str,
        what: &str,
        request: impl std::future::Future<Output = ApiResult<Session>> + Send + 'static,
        claimed: impl Fn(&Session) -> bool,
    ) {
        let lifecycle = app.session_lock(id).await;
        let held = lifecycle.lock().await;
        let handled = tokio::spawn(request);
        for _ in 0..64 {
            tokio::task::yield_now().await;
            assert!(!handled.is_finished(), "the {what} waits for the colony's lifecycle lock");
            assert!(
                !claimed(&app.session(id).await.unwrap()),
                "nothing is claimed while another task holds the colony's lifecycle lock"
            );
        }
        drop(held);
        if let Err(e) = handled.await.unwrap() {
            panic!("the {what} failed once the lock freed up: {:#}", e.1);
        }
        assert!(
            claimed(&app.session(id).await.unwrap()),
            "the {what} claims the colony once the lock freed up"
        );
    }

    #[tokio::test]
    async fn a_resume_waits_for_the_colony_s_lifecycle_lock_before_it_claims() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.git_admin_dir = Some("git".into()))
            .await
            .unwrap();
        fill_the_parallel_limit(&app).await;
        a_handler_held_behind_the_colony_lifecycle_lock_claims_nothing_until_it_is_dropped(
            &app,
            "abc",
            "resume",
            resume(State(app.clone()), Path("abc".to_string())),
            |s| s.status == SessionStatus::Queued,
        )
        .await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stopping_a_colony_clears_its_attention_flag_and_keeps_the_reason_in_its_log() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        app.update_session("abc", |s| {
            s.attention = Some(json!({"reason": "autopilot_held", "since": Utc::now(), "nudges": 0}));
        })
        .await
        .unwrap();
        let stopped = stop(State(app.clone()), Path("abc".to_string())).await.unwrap();
        assert_eq!(stopped.result, StopResult::Stopped);
        assert_eq!(
            stopped.session.status,
            SessionStatus::Stopped,
            "the stop answers with the stopped colony"
        );
        let s = app.session("abc").await.unwrap();
        assert_eq!(s.status, SessionStatus::Stopped);
        assert!(
            s.attention.is_none(),
            "a stopped colony carries no attention flag into sessions.json"
        );
        let log = std::fs::read_to_string(app.session_dir("abc").join("harness.jsonl")).unwrap();
        assert!(
            log.contains("autopilot_held"),
            "the cleared flag's reason stays in the colony's log: {log}"
        );
        // The cleared flag stays cleared across a persist and reload of the session list.
        app.persist_sessions().await.unwrap();
        let reloaded: Vec<Session> = serde_json::from_slice(&std::fs::read(app.sessions_file()).unwrap()).unwrap();
        let reloaded = reloaded.iter().find(|s| s.id == "abc").unwrap();
        assert_eq!(reloaded.status, SessionStatus::Stopped);
        assert!(
            reloaded.attention.is_none(),
            "a reloaded stopped colony still carries no attention flag"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_stop_waits_for_the_colony_s_lifecycle_lock_before_it_claims() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        a_handler_held_behind_the_colony_lifecycle_lock_claims_nothing_until_it_is_dropped(
            &app,
            "abc",
            "stop",
            {
                let app = app.clone();
                async move { stop(State(app), Path("abc".to_string())).await.map(|Json(r)| Json(r.session)) }
            },
            |s| s.status == SessionStatus::Stopped,
        )
        .await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_second_stop_of_a_stopped_colony_answers_already_stopped_instead_of_an_error() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let first = stop(State(app.clone()), Path("abc".to_string())).await.unwrap();
        assert_eq!(first.result, StopResult::Stopped, "the live colony is stopped");
        assert_eq!(first.session.status, SessionStatus::Stopped);
        let second = stop(State(app.clone()), Path("abc".to_string())).await.unwrap();
        assert_eq!(second.result, StopResult::AlreadyStopped, "a retried stop is a success");
        assert_eq!(second.session.status, SessionStatus::Stopped);
        let body = serde_json::to_value(&second.0).unwrap();
        assert_eq!(body["result"], json!("already_stopped"), "{body}");
        assert_eq!(
            body["status"],
            json!("stopped"),
            "the reply still reads as the colony: {body}"
        );
        assert_eq!(body["id"], json!("abc"), "{body}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stopping_a_colony_that_is_already_over_answers_already_stopped_and_changes_nothing() {
        use SessionStatus::*;
        for status in [PrOpened, NoChanges, Merged, Closed, Failed] {
            let (app, root) = app_with_colony("abc", status).await;
            let before = app.session("abc").await.unwrap();
            let out = stop(State(app.clone()), Path("abc".to_string())).await.unwrap();
            assert_eq!(out.result, StopResult::AlreadyStopped, "{status:?}");
            assert_eq!(out.session.status, status, "the reply carries the status it found");
            let after = app.session("abc").await.unwrap();
            assert_eq!(after.status, status, "{status:?} is not rewritten to stopped");
            assert_eq!(after.updated_at, before.updated_at, "{status:?}: nothing was claimed");
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[tokio::test]
    async fn a_stop_takes_a_queued_colony_out_of_the_queue_and_refuses_a_publishing_one() {
        let (app, root) = app_with_colony("abc", SessionStatus::Queued).await;
        let out = stop(State(app.clone()), Path("abc".to_string())).await.unwrap();
        assert_eq!(out.result, StopResult::Stopped);
        assert_eq!(out.session.status, SessionStatus::Stopped);
        let _ = std::fs::remove_dir_all(root);

        let (app, root) = app_with_colony("abc", SessionStatus::Publishing).await;
        let err = stop(State(app.clone()), Path("abc".to_string())).await.unwrap_err();
        assert_eq!(
            err.status(),
            StatusCode::CONFLICT,
            "a push in flight is neither live nor over"
        );
        assert_eq!(app.session("abc").await.unwrap().status, SessionStatus::Publishing);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stopping_an_unknown_colony_is_still_not_found() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        let err = stop(State(app.clone()), Path("nope".to_string())).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_cleanup_waits_for_the_colony_s_lifecycle_lock_before_it_claims() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        a_handler_held_behind_the_colony_lifecycle_lock_claims_nothing_until_it_is_dropped(
            &app,
            "abc",
            "cleanup",
            cleanup(State(app.clone()), Path("abc".to_string())),
            |s| s.cleaned_up,
        )
        .await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn two_concurrent_resumes_of_one_colony_admit_exactly_one_and_refuse_the_other() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.git_admin_dir = Some("git".into()))
            .await
            .unwrap();
        // No free slot, so the winning resume queues (`Queued`) instead of spawning a boot whose git
        // and `msb` work would flip the colony's status under the assertions below.
        fill_the_parallel_limit(&app).await;
        // Park both resumes behind the colony's lifecycle lock before letting either claim: both are
        // then past their snapshot pre-checks, so the race is settled by the re-check under the
        // admission lock rather than by whichever handler happened to read the list first.
        let lifecycle = app.session_lock("abc").await;
        let held = lifecycle.lock().await;
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let app = app.clone();
            tasks.push(tokio::spawn(resume(State(app), Path("abc".to_string()))));
        }
        for _ in 0..64 {
            tokio::task::yield_now().await;
            for task in &tasks {
                assert!(!task.is_finished(), "each resume waits for the colony's lifecycle lock");
            }
        }
        drop(held);
        let mut outcomes = Vec::new();
        for task in tasks {
            outcomes.push(task.await.expect("resume task joined"));
        }
        assert_eq!(outcomes.len(), 2, "both resumes answered");
        assert_eq!(
            outcomes.iter().filter(|r| r.is_ok()).count(),
            1,
            "exactly one of the two resumes claims the colony"
        );
        for e in outcomes.iter().filter_map(|r| r.as_ref().err()) {
            assert_eq!(e.0, StatusCode::CONFLICT, "the loser is refused, not failed: {}", e.1);
            assert!(e.1.to_string().contains(RESUME_CONFLICT), "{}", e.1);
        }
        let s = app.session("abc").await.unwrap();
        assert_eq!(
            s.status,
            SessionStatus::Queued,
            "the winner holds the one claim — queued here, since the parallel limit is full"
        );
        assert_eq!(s.error, None, "the winning claim cleared the colony's error");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_cleanup_refuses_a_colony_that_stopped_being_cleanable_while_it_waited_and_leaves_the_worktree_alone() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        let worktree = root.join("worktrees/acme/repo/issue-1-abc");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join("work.txt"), "the colony's work").unwrap();
        app.update_session("abc", |s| s.worktree = worktree.display().to_string())
            .await
            .unwrap();
        // Hold the lifecycle lock so the cleanup is parked between its snapshot pre-check (which sees
        // `stopped` and passes) and its claim, then take the colony live in that window. The claim,
        // not the pre-check, is what keeps a colony from being cleaned out from under a boot.
        let lifecycle = app.session_lock("abc").await;
        let held = lifecycle.lock().await;
        let handled = tokio::spawn(cleanup(State(app.clone()), Path("abc".to_string())));
        for _ in 0..64 {
            tokio::task::yield_now().await;
            assert!(!handled.is_finished(), "the cleanup waits for the colony's lifecycle lock");
            assert!(
                !app.session("abc").await.unwrap().cleaned_up,
                "nothing is claimed while the colony's lifecycle lock is held"
            );
        }
        app.update_session("abc", |s| s.status = SessionStatus::Running)
            .await
            .unwrap();
        drop(held);
        let err = handled.await.unwrap().unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT, "{}", err.1);
        assert!(err.1.to_string().contains("stop the session first"), "{}", err.1);
        assert!(
            !app.session("abc").await.unwrap().cleaned_up,
            "the colony is not marked cleaned up"
        );
        assert!(worktree.join("work.txt").exists(), "the worktree is untouched");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_cleanup_whose_worktree_removal_fails_puts_cleaned_up_back_so_the_colony_stays_resumable() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        // A "worktree" that is a plain file: `remove_worktree` sees it exists, and then fails to write
        // the `.git` file inside it — the one failure it propagates that is deterministic without a
        // git binary or permission games (the tests run as root, so chmod-based denial does not fail).
        let worktree = root.join("worktrees/acme/repo/issue-1-abc");
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        std::fs::write(&worktree, "not a directory").unwrap();
        app.update_session("abc", |s| {
            s.worktree = worktree.display().to_string();
            s.git_admin_dir = Some("git".into());
        })
        .await
        .unwrap();
        let err = cleanup(State(app.clone()), Path("abc".to_string())).await.unwrap_err();
        let body = err.1.to_string();
        assert!(body.contains("could not remove the colony's worktree"), "{body}");
        let s = app.session("abc").await.unwrap();
        assert!(!s.cleaned_up, "cleaned_up is put back, or a resumable colony is stranded");
        assert!(
            can_resume(s.status, s.cleaned_up, s.git_admin_dir.is_some()),
            "the colony can still be resumed, and the cleanup retried"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_cleanup_of_a_colony_deleted_while_its_worktree_was_being_removed_answers_not_found() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        // Park the cleanup between its claim and the worktree removal, then make the colony vanish in
        // that window — as a delete does, taking no lifecycle lock. This colony has no worktree on
        // disk, so the removal itself no-ops and the answer comes straight from the re-read.
        let repo = app.session("abc").await.unwrap().repo;
        let repo_lock = app.repo_lock(&repo).await;
        let held = repo_lock.lock().await;
        let handled = tokio::spawn(cleanup(State(app.clone()), Path("abc".to_string())));
        for _ in 0..256 {
            tokio::task::yield_now().await;
            assert!(!handled.is_finished(), "the cleanup waits for the repo lock");
            if app.session("abc").await.is_some_and(|s| s.cleaned_up) {
                break;
            }
        }
        assert!(
            app.session("abc").await.is_some_and(|s| s.cleaned_up),
            "the cleanup claimed the colony before its worktree removal"
        );
        app.sessions.write().await.retain(|s| s.id != "abc");
        drop(held);
        let handled = match handled.await.unwrap() {
            Err(e) => e,
            Ok(s) => panic!("a colony deleted during the removal must not answer 200: {:?}", s.status),
        };
        assert_eq!(handled.0, StatusCode::NOT_FOUND, "{}", handled.1);
        assert!(handled.1.to_string().contains("no such session"), "{}", handled.1);
        let _ = std::fs::remove_dir_all(root);
    }

    // -- the sandbox watchdog ----------------------------------------------------------------------

    #[test]
    fn a_watch_candidate_is_a_live_colony_that_is_not_still_booting() {
        use SessionStatus::*;
        for status in [Running, WaitingForAnswer, Idle] {
            assert!(
                watch_candidate(&colony("acme", status)),
                "{status:?} is live and past its boot"
            );
        }
        for status in [
            Starting, Queued, Publishing, Stopped, Failed, PrOpened, Merged, Closed, NoChanges,
        ] {
            assert!(
                !watch_candidate(&colony("acme", status)),
                "{status:?} is not the watchdog's to stop"
            );
        }
    }

    #[test]
    fn a_watch_flip_lands_on_the_snapshot_status_and_not_on_a_publish_that_claimed_in_the_meantime() {
        let mut stopped_early = colony("acme", SessionStatus::Running);
        assert!(
            mark_stopped_after_teardown(&mut stopped_early, SessionStatus::Running),
            "the flip lands while the colony is still on the status the snapshot read"
        );
        assert_eq!(stopped_early.status, SessionStatus::Stopped, "the colony reads stopped");
        assert_eq!(stopped_early.error, Some(VM_STOPPED_EARLY.into()), "the flip says why");
        // A publish holds no lifecycle lock, so it can claim the colony while the watchdog's teardown
        // is still in flight; the flip must leave that claim standing.
        let mut claimed = colony("acme", SessionStatus::Publishing);
        assert!(
            !mark_stopped_after_teardown(&mut claimed, SessionStatus::Running),
            "the flip refuses a colony that moved off its snapshot status"
        );
        assert_eq!(claimed.status, SessionStatus::Publishing, "the publish keeps its claim");
        assert_eq!(claimed.error, None, "and the watchdog's error is not painted over it");
    }

    #[tokio::test]
    async fn a_watch_s_re_read_skips_a_colony_deleted_or_claimed_after_its_snapshot() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        let mut claimed = colony("acme", SessionStatus::Running);
        claimed.id = "claimed".into();
        app.sessions.write().await.push(claimed);
        assert!(
            the_colony_to_stop(&app, "abc").await.is_some(),
            "an idle colony the re-read still finds is the tick's to stop"
        );
        // The two ways a snapshot goes stale while the tick works down the list: a delete, which takes
        // no lifecycle lock, removes the record; a publish claims the colony out from under it.
        app.sessions.write().await.retain(|s| s.id != "abc");
        app.update_session("claimed", |x| x.status = SessionStatus::Publishing)
            .await
            .unwrap();
        assert!(
            the_colony_to_stop(&app, "abc").await.is_none(),
            "a colony deleted since the snapshot is gone from the list, so there is nothing to stop"
        );
        assert!(
            the_colony_to_stop(&app, "claimed").await.is_none(),
            "a colony claimed off its snapshot status in the meantime is not the tick's to stop"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    // -- recover after a harness restart -------------------------------------------------------

    /// Parks a `recover` pass behind a colony's lifecycle lock, flips the colony in that window —
    /// as a resume, stop or publish admitted by the HTTP API would — then lets the pass through.
    /// Purely cooperative, so there is no timing bet: each yield lets the pass advance to the
    /// lock it cannot take, and the flip always lands after the pass's snapshot.
    async fn recover_after_a_concurrent_flip(app: &Shared, id: &str, flip: impl FnOnce(&mut Session)) {
        let lifecycle = app.session_lock(id).await;
        let held = lifecycle.lock().await;
        let owned = app.clone();
        let recovered = tokio::spawn(async move { recover(&owned).await });
        for _ in 0..64 {
            tokio::task::yield_now().await;
            assert!(
                !recovered.is_finished(),
                "the recover pass waits for the colony's lifecycle lock"
            );
        }
        app.update_session(id, flip).await.unwrap();
        drop(held);
        recovered.await.expect("recover task joined");
    }

    #[tokio::test]
    async fn a_recover_does_not_clobber_a_resume_that_claimed_the_colony_after_its_snapshot() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        recover_after_a_concurrent_flip(&app, "abc", |x| x.status = SessionStatus::Starting).await;
        let s = app.session("abc").await.unwrap();
        assert_eq!(s.status, SessionStatus::Starting, "the boot in flight keeps its claim");
        assert_eq!(s.error, None, "and the pass paints no stopped error over it");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_recover_does_not_clobber_a_publish_that_claimed_the_colony_after_its_snapshot() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        recover_after_a_concurrent_flip(&app, "abc", |x| x.status = SessionStatus::Publishing).await;
        let s = app.session("abc").await.unwrap();
        assert_eq!(s.status, SessionStatus::Publishing, "the publish keeps its claim");
        assert_eq!(s.error, None, "and the pass paints no stopped error over it");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_recover_leaves_a_publishing_colony_alone_once_it_moves_off_publishing() {
        let (app, root) = app_with_colony("abc", SessionStatus::Publishing).await;
        recover_after_a_concurrent_flip(&app, "abc", |x| x.status = SessionStatus::Stopped).await;
        let s = app.session("abc").await.unwrap();
        assert_eq!(s.status, SessionStatus::Stopped, "the colony keeps the status it moved to");
        assert_eq!(s.error, None, "and the pass paints no failed error over it");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn only_a_push_orphaned_before_the_restart_is_recovers_to_reap() {
        assert!(
            orphaned_publish(
                &colony("acme", SessionStatus::Publishing),
                &colony("acme", SessionStatus::Publishing)
            ),
            "a push the snapshot already held died with the restart, so nothing else reaps it"
        );
        assert!(
            !orphaned_publish(
                &colony("acme", SessionStatus::Idle),
                &colony("acme", SessionStatus::Publishing)
            ),
            "a publish that claimed the colony after the snapshot owns the worktree now"
        );
        assert!(
            !orphaned_publish(
                &colony("acme", SessionStatus::Publishing),
                &colony("acme", SessionStatus::Idle)
            ),
            "a push the restart did not orphan finished on its own"
        );
    }

    #[test]
    fn only_a_boot_claimed_after_the_snapshot_is_skipped_not_an_orphaned_one() {
        assert!(
            claimed_boot_after_snapshot(&colony("acme", SessionStatus::Idle), &colony("acme", SessionStatus::Starting)),
            "a resume that claimed the colony after the snapshot is creating its microVM now"
        );
        assert!(
            claimed_boot_after_snapshot(
                &colony("acme", SessionStatus::Queued),
                &colony("acme", SessionStatus::Starting)
            ),
            "the queue admitting the colony after the snapshot is the same claim"
        );
        assert!(
            !claimed_boot_after_snapshot(
                &colony("acme", SessionStatus::Starting),
                &colony("acme", SessionStatus::Starting)
            ),
            "a boot the snapshot already held is orphaned, and falls through to the teardown"
        );
        assert!(
            !claimed_boot_after_snapshot(
                &colony("acme", SessionStatus::Starting),
                &colony("acme", SessionStatus::Running)
            ),
            "a colony past its boot is judged on its own status, not this rule"
        );
    }

    #[test]
    fn a_restart_failed_flip_lands_on_publishing_and_not_on_a_claim_that_moved_in_the_meantime() {
        let mut orphaned = colony("acme", SessionStatus::Publishing);
        assert!(
            mark_failed_after_restart(&mut orphaned),
            "the flip lands while the colony still holds the orphaned push"
        );
        assert_eq!(orphaned.status, SessionStatus::Failed, "the colony reads failed");
        assert_eq!(orphaned.error, Some(PUBLISH_LOST_TO_RESTART.into()), "the flip says why");
        // A publish holds no lifecycle lock, so it can reclaim the colony while this pass's
        // teardown is still in flight; the flip must leave that claim standing.
        let mut reclaimed = colony("acme", SessionStatus::Idle);
        assert!(
            !mark_failed_after_restart(&mut reclaimed),
            "the flip refuses a colony that moved off publishing"
        );
        assert_eq!(reclaimed.status, SessionStatus::Idle, "the claim keeps its status");
        assert_eq!(reclaimed.error, None, "and the restart's error is not painted over it");
    }

    #[test]
    fn a_restart_stopped_flip_lands_on_the_teardown_status_and_not_on_a_publish_that_claimed_in_the_meantime() {
        let mut gone = colony("acme", SessionStatus::Idle);
        assert!(
            mark_stopped_after_restart(&mut gone, SessionStatus::Idle),
            "the flip lands while the colony is still on the status the re-read saw"
        );
        assert_eq!(gone.status, SessionStatus::Stopped, "the colony reads stopped");
        assert_eq!(gone.error, Some(VM_GONE_AFTER_RESTART.into()), "the flip says why");
        let mut claimed = colony("acme", SessionStatus::Publishing);
        assert!(
            !mark_stopped_after_restart(&mut claimed, SessionStatus::Idle),
            "the flip refuses a colony that moved off its teardown status"
        );
        assert_eq!(claimed.status, SessionStatus::Publishing, "the publish keeps its claim");
        assert_eq!(claimed.error, None, "and the restart's error is not painted over it");
    }

    #[tokio::test]
    async fn recover_reaps_what_the_restart_orphaned_and_leaves_a_finished_colony_alone() {
        // Safe without KVM: the fixture colonies have no mesh address and no local port, so none
        // is reachable and the reconnect branch (which would spawn the agent link) never runs;
        // `msb` is absent here, so the running set is empty and the removals fail silently.
        use crate::tests::test_app;
        let root = std::env::temp_dir().join(format!("colonizer-recover-{}", short_id()));
        let app = test_app(&root);
        for (id, status) in [
            ("boot", SessionStatus::Starting),
            ("push", SessionStatus::Publishing),
            ("idle", SessionStatus::Idle),
            ("done", SessionStatus::Stopped),
        ] {
            let mut s = colony("acme", status);
            s.id = id.into();
            s.sandbox = format!("sandbox-{id}");
            app.sessions.write().await.push(s);
            tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
        }
        recover(&app).await;
        let boot = app.session("boot").await.unwrap();
        assert_eq!(
            boot.status,
            SessionStatus::Stopped,
            "an orphaned boot is not stranded forever"
        );
        assert_eq!(boot.error.as_deref(), Some(VM_GONE_AFTER_RESTART), "the reaped boot says why");
        let push = app.session("push").await.unwrap();
        assert_eq!(push.status, SessionStatus::Failed, "an orphaned push is reaped");
        assert_eq!(
            push.error.as_deref(),
            Some(PUBLISH_LOST_TO_RESTART),
            "the reaped push says why"
        );
        let idle = app.session("idle").await.unwrap();
        assert_eq!(idle.status, SessionStatus::Stopped, "a live colony with no microVM is reaped");
        assert_eq!(
            idle.error.as_deref(),
            Some(VM_GONE_AFTER_RESTART),
            "the reaped colony says why"
        );
        let done = app.session("done").await.unwrap();
        assert_eq!(done.status, SessionStatus::Stopped, "a finished colony is skipped");
        assert_eq!(done.error, None, "and the skip does not repaint its error");
        let _ = std::fs::remove_dir_all(root);
    }
}
