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
    client_error, github,
    orgs, providers,
    sandbox::{self},
    util::{
        dir_size,
        faults::{self, Op},
        format_disk_size,
    },
    ApiResult, App, Shared,
};
use axum::{
    extract::{
        Path, State,
    },
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    time::Duration,
};

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
    let sessions = app.sessions.read().await.clone();
    for s in sessions {
        if s.status == SessionStatus::Publishing {
            // The kill may have landed between persisting `publishing` and the teardown inside it, so a
            // microVM can still be running — and after a restart nothing would reap it: the runtimes map
            // is empty, so no later publish removes it, and `watch_sandboxes` skips non-live statuses.
            teardown_vm(app, &s).await;
            app.update_session(&s.id, |x| {
                x.status = SessionStatus::Failed;
                x.error = Some(PUBLISH_LOST_TO_RESTART.into());
            })
            .await;
            continue;
        }
        if !s.status.is_live() {
            continue;
        }
        let reachable = s.mesh.as_ref().is_some_and(|m| m.ip.is_some()) || s.local_port.is_some();
        if running.contains(&s.sandbox) && reachable && s.status != SessionStatus::Starting {
            if s.mesh.is_some() {
                match app.mesh().await {
                    Ok(mesh) => {
                        if let Err(e) = mesh.ensure_started().await {
                            app.session_log(&s.id, "error", format!("mesh failed to start: {e:#}")).await;
                        }
                    }
                    Err(e) => app.session_log(&s.id, "error", format!("{e:#}")).await,
                }
            }
            app.session_log(&s.id, "info", "harness restarted: reconnecting to the running microVM".into()).await;
            start_link(app, &s.id).await;
        } else {
            teardown_vm(app, &s).await;
            app.update_session(&s.id, |x| {
                x.status = SessionStatus::Stopped;
                x.error = Some(VM_GONE_AFTER_RESTART.into());
            })
            .await;
        }
    }
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
        let Ok(running) = sandbox::running(&app.cfg.msb).await else { continue };
        // Bound to a local first: a read guard in the `for` expression would live for the whole loop and
        // deadlock against update_session's write lock.
        let sessions = app.sessions.read().await.clone();
        for s in sessions {
            if !s.status.is_live() || s.status == SessionStatus::Starting || running.contains(&s.sandbox) {
                continue;
            }
            app.session_log(&s.id, "error", "the microVM stopped; the worktree is kept, so this colony can be resumed".into()).await;
            teardown_vm(&app, &s).await;
            app.update_session(&s.id, |x| {
                x.status = SessionStatus::Stopped;
                x.error = Some(VM_STOPPED_EARLY.into());
            })
            .await;
        }
    }
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
pub(crate) async fn stop_colony(app: &Shared, s: &Session, due: impl FnOnce(&Session) -> bool, error: String, warn: String) -> bool {
    let claimed = app
        .update_session(&s.id, |x| {
            let due = x.status.is_live() && due(x);
            if due {
                x.status = SessionStatus::Stopped;
                x.error = Some(error);
            }
            due
        })
        .await
        .is_some_and(|(_, due)| due);
    if claimed {
        app.session_log(&s.id, "warn", warn).await;
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
    let modules = app.modules.read().await;
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
    app.update_session(colony, |x| {
        x.routed_cost_usd = Some(x.routed_cost_usd.unwrap_or_default() + cost);
    })
    .await;
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
    tokio::task::spawn_blocking(move || dir_size(&worktree) + dir_size(&session_dir)).await.unwrap_or(0)
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
        for s in sessions.into_iter().filter(|s| s.status.is_live() && s.status != SessionStatus::Starting) {
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

pub async fn resume(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    let s = app.session(&id).await.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    if !can_resume(s.status, s.cleaned_up, s.git_admin_dir.is_some()) {
        return Err(client_error(StatusCode::CONFLICT, RESUME_CONFLICT));
    }
    let previous_status = s.status;
    // Past the limit a colony waits its turn rather than being refused, as in `create`; `run_queue`
    // resumes it later.
    let modules = app.modules.read().await.clone();
    let max_parallel = orgs::global_max_parallel(&modules) as usize;
    // Resolved before the admission lock: `org_settings` reads the orgs file with blocking IO.
    let org_limit = orgs::org_max_parallel(&app.org_settings(&s.org));
    // The claim comes first, exactly as the publish claim does: `failed` is both resumable and
    // publishable, so a resume landing just after a publish claimed the colony must be refused
    // rather than overwrite `publishing`. Nothing outside the list is touched until it succeeds, so
    // a refused resume leaves the agent link and the event log as it found them.
    // The closure distinguishes its two non-outcomes: `Err` is the not-resumable re-check, answered with
    // a 409 below, while `Ok(None)` is a colony that has vanished, which stays a 404.
    let claimed = with_slot(&app.sessions, &s.org, max_parallel, org_limit, |sessions, room| {
        // Counted before the flip: the colony itself is Stopped or Failed here, so it isn't counted.
        let waiting = sessions.iter().filter(|other| other.status == SessionStatus::Queued).count();
        let Some(x) = sessions.iter_mut().find(|x| x.id == id) else {
            return Ok(None);
        };
        // Re-checked under the lock: the colony must still be resumable when the slot is claimed.
        if !can_resume(x.status, x.cleaned_up, x.git_admin_dir.is_some()) {
            return Err(RESUME_CONFLICT); // another resume won the race between the handler and the lock
        }
        x.status = if room { SessionStatus::Starting } else { SessionStatus::Queued };
        x.error = None;
        x.attention = None;
        x.mesh = None;
        x.local_port = None;
        x.updated_at = Utc::now();
        Ok(Some((x.clone(), room, waiting)))
    })
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
        app.session_log(&id, "info", "resuming: booting a fresh microVM on the kept worktree".into()).await;
        tokio::spawn(boot(app.clone(), id, true));
    } else {
        let ahead = if waiting == 0 { String::new() } else { format!(", behind {waiting} already waiting") };
        app.session_log(
            &id,
            "info",
            format!("queued: the parallel limit is {max_parallel}{ahead}; the colony resumes when a slot frees up"),
        )
        .await;
    }
    Ok(Json(s))
}

pub async fn stop(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    let Some((s, was)) = app
        .update_session(&id, |x| {
            let was = x.status;
            if was.is_live() || was == SessionStatus::Queued {
                x.status = SessionStatus::Stopped;
            }
            was
        })
        .await
    else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such session"));
    };
    // A queued colony never started, so there is no microVM to remove.
    if was == SessionStatus::Queued {
        app.session_log(&id, "info", "left the queue before it started".into()).await;
        return Ok(Json(app.session(&id).await.unwrap_or(s)));
    }
    if !was.is_live() {
        return Err(client_error(StatusCode::CONFLICT, "session is not running"));
    }
    app.session_log(&id, "info", "stopping: removing the microVM (the worktree is kept)".into()).await;
    teardown_vm(&app, &s).await;
    Ok(Json(app.session(&id).await.unwrap_or(s)))
}

/// Whether a colony in this state can be cleaned up — its worktree and local branch freed. A live or
/// publishing colony has a microVM or a push in flight, and a queued colony is still waiting to start:
/// cleaning one up would leave it queued with nothing left to start on, and the next queue tick would
/// start it anyway, cleanup undone. Stop first, which takes a queued colony out of the queue.
fn cleanable(status: SessionStatus) -> bool {
    !status.is_live() && status != SessionStatus::Publishing && status != SessionStatus::Queued
}

pub async fn cleanup(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    let s = app.session(&id).await.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    if !cleanable(s.status) {
        return Err(client_error(StatusCode::CONFLICT, "stop the session first"));
    }
    {
        let lock = app.repo_lock(&s.repo).await;
        let _guard = lock.lock().await;
        github::remove_worktree(&app, &s).await?;
    }
    let (s, ()) = app
        .update_session(&id, |x| x.cleaned_up = true)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    Ok(Json(s))
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
        let at = sessions.iter().position(|s| s.id == id).ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
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
            return Err(e.context("could not remove the colony's worktree; nothing was deleted").into());
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
    app.runtimes.lock().await.remove(&id);
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
    use crate::util::short_id;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use crate::sessions::tests::{admit_resume, app_with_colony, stopped_colony_with_worktree};

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
        assert_eq!(std::fs::read_to_string(dir.join("events.jsonl")).unwrap(), "{\"seq\":1}\n", "the log is untouched");
        rotate_events(&dir).unwrap();
        assert!(!dir.join("events.jsonl").exists());
        assert_eq!(std::fs::read_to_string(dir.join("events-1.jsonl")).unwrap(), "{\"seq\":1}\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_failed_event_log_rotation_refuses_the_resume_and_leaves_the_colony_stopped() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.git_admin_dir = Some("/tmp/wt".into())).await.unwrap();
        std::fs::write(app.session_dir("abc").join("events.jsonl"), "{\"seq\":7}\n").unwrap();
        let _guard = faults::inject("events.jsonl", Op::Rename, || std::io::Error::from_raw_os_error(5));
        let err = resume(State(app.clone()), Path("abc".to_string())).await.unwrap_err();
        let body = err.1.to_string();
        assert!(body.contains("the colony was not resumed"), "{body}");
        assert!(body.contains("aside yourself and try again"), "{body}");
        assert!(app.storage_alert.read().await.is_some(), "the failure is recorded, not swallowed");
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
        assert!(message.contains(&dir.display().to_string()), "the error names the directory that filled up: {message}");
        assert_eq!(
            std::fs::read_to_string(dir.join("events.jsonl")).unwrap(),
            "{\"seq\":1}\n",
            "the log is left in place, so the resume stays refused instead of dropping events"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_resume_does_not_rotate_while_another_task_holds_the_runtime_s_file_lock() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.git_admin_dir = Some("/tmp/wt".into())).await.unwrap();
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
        assert_eq!(std::fs::read_to_string(app.session_dir("abc").join("events-1.jsonl")).unwrap(), "{\"seq\":7}\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_failed_save_keeps_the_colony_listed_and_the_delete_reports_failure() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.cleaned_up = true).await.unwrap();
        let _guard = faults::inject("sessions.json", Op::Write, || std::io::Error::from(std::io::ErrorKind::StorageFull));
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
            (0..resumable).map(|i| stopped_colony_with_worktree("acme", format!("resume-{i}"))).collect::<Vec<_>>(),
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
}
