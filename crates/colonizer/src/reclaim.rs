//! Automatic reclamation of finished colonies: a 5-minute tick reclaims the
//! worktrees of colonies whose work is safely on the remote, plus worktree and
//! microVM orphans no session owns. `keep_worktree` exempts one colony;
//! `COLONIZER_RECLAIM` switches the whole tick off. Manual cleanup works either way.

use crate::{
    ApiResult, Shared, client_error, lifecycle, runtime, sandbox,
    sessions::{Session, SessionStatus},
    util::{dir_size, env_nonempty, exec, parse_disk_size},
};
use axum::{
    Json,
    extract::{Path as RoutePath, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

pub struct ReclaimConfig {
    pub enabled: bool,
    pub retention_secs: u64,
    pub min_free_bytes: u64,
}

pub fn default_retention_secs() -> u64 {
    12 * 3600
}

pub fn default_min_free_bytes() -> u64 {
    parse_disk_size("5G").unwrap_or(5 * 1024 * 1024 * 1024)
}

/// Anything but an explicit opt-out keeps the tick on.
pub fn parse_enabled(text: &str) -> bool {
    !matches!(text.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no")
}

/// Hours from the environment, answered in seconds; garbage means the default.
pub fn parse_retention(text: &str) -> u64 {
    text.trim()
        .parse::<u64>()
        .map(|h| h.saturating_mul(3600).min(i64::MAX as u64))
        .unwrap_or(default_retention_secs())
}

pub fn parse_min_free(text: &str) -> u64 {
    parse_disk_size(text.trim()).unwrap_or_else(default_min_free_bytes)
}

impl ReclaimConfig {
    pub fn from_env() -> Self {
        Self {
            enabled: env_nonempty("COLONIZER_RECLAIM").map(|v| parse_enabled(&v)).unwrap_or(true),
            retention_secs: env_nonempty("COLONIZER_RECLAIM_RETENTION_HOURS")
                .map(|v| parse_retention(&v))
                .unwrap_or_else(default_retention_secs),
            min_free_bytes: env_nonempty("COLONIZER_RECLAIM_MIN_FREE")
                .map(|v| parse_min_free(&v))
                .unwrap_or_else(default_min_free_bytes),
        }
    }
}

/// A colony whose work is safely on the remote: a pushed terminal state, or
/// `NoChanges`, where publish found nothing to push so no unique work exists.
fn pushed_terminal(s: &Session) -> bool {
    matches!(
        s.status,
        SessionStatus::PrOpened | SessionStatus::Merged | SessionStatus::Closed
    ) && s.pr_url.is_some()
        || s.status == SessionStatus::NoChanges
}

pub fn reclaim_due(s: &Session, now: DateTime<Utc>, retention_secs: u64) -> bool {
    if s.cleaned_up || s.keep_worktree {
        return false;
    }
    // Stopped/Failed are resumable via `can_resume`, so they are never
    // auto-reclaimed even with a pull request; live statuses are excluded too.
    if !pushed_terminal(s) {
        return false;
    }
    now.signed_duration_since(s.updated_at).num_seconds() >= retention_secs as i64
}

/// A colony that may hold its only copy of the work: never auto-reclaimed.
pub fn unpushed_work(s: &Session) -> bool {
    !s.cleaned_up && s.pr_url.is_none() && s.git_admin_dir.is_some()
}

/// Free bytes on the data disk via the status probe's `df` parse; `None` on failure ("no signal", never full).
pub async fn free_bytes(data_dir: &Path) -> Option<u64> {
    runtime::probe_df(data_dir).await.2
}

pub struct SweepReport {
    pub reclaimed: Vec<String>,
    pub failed: Vec<String>,
    pub orphans_removed: Vec<String>,
    pub orphans_held: Vec<String>,
}

impl SweepReport {
    pub fn is_empty(&self) -> bool {
        self.reclaimed.is_empty() && self.failed.is_empty() && self.orphans_removed.is_empty() && self.orphans_held.is_empty()
    }
}

/// Reclaim candidates oldest-pushed-first, so a low-disk sweep takes the stalest colonies first.
fn sweep_candidates(sessions: &[Session], now: DateTime<Utc>, retention_secs: u64) -> Vec<Session> {
    let mut out: Vec<Session> = sessions
        .iter()
        .filter(|s| reclaim_due(s, now, retention_secs))
        .cloned()
        .collect();
    out.sort_by_key(|s| s.updated_at);
    out
}

pub async fn sweep_once(app: &Shared, cfg: &ReclaimConfig) -> SweepReport {
    let mut report = SweepReport {
        reclaimed: Vec::new(),
        failed: Vec::new(),
        orphans_removed: Vec::new(),
        orphans_held: Vec::new(),
    };
    if !cfg.enabled {
        return report;
    }
    let sessions = app.sessions.read().await.clone();
    let low_disk = matches!(free_bytes(&app.cfg.data_dir).await, Some(f) if f < cfg.min_free_bytes);
    let retention = if low_disk { 0 } else { cfg.retention_secs }; // low disk reclaims regardless of the window
    for s in sweep_candidates(&sessions, Utc::now(), retention) {
        let id = s.id.clone();
        match lifecycle::cleanup_one(app, &id).await {
            Ok(s) => {
                // Manual cleanup leaves the VM to the operator, but the automatic path must not leak one.
                sandbox::remove(&app.cfg.msb, &s.sandbox).await;
                app.session_log(&id, "info", "automatically reclaimed: its pull request holds the work, so the worktree was removed and the colony is now unresumable".into()).await;
                report.reclaimed.push(id);
            }
            Err(e) => report.failed.push(format!("{id}: {e:#}")),
        }
    }
    let (removed, held) = sweep_orphan_worktrees(app, retention).await;
    report.orphans_removed = removed;
    report.orphans_held = held;
    report.orphans_removed.extend(sweep_orphan_vms(app).await);
    report
}

enum OrphanVerdict {
    Skip, // young enough to belong to someone: left alone, reported nowhere
    Held(String),
    Reclaimable,
}

async fn classify_orphan(wt: &Path, retention_secs: u64) -> OrphanVerdict {
    // An unreadable mtime is conservative: held, never removed.
    let age_secs = match std::fs::metadata(wt).and_then(|m| m.modified()) {
        Ok(mtime) => std::time::SystemTime::now()
            .duration_since(mtime)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        Err(_) => return OrphanVerdict::Held("mtime-unreadable".into()),
    };
    if age_secs < retention_secs {
        return OrphanVerdict::Skip;
    }
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(wt).args(["status", "--porcelain"]);
    match exec(&mut cmd).await {
        Ok(out) if out.trim().is_empty() => {}
        Ok(_) => return OrphanVerdict::Held("dirty".into()),
        Err(_) => return OrphanVerdict::Held("unreadable-git".into()),
    }
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(wt)
        .args(["rev-list", "--count", "HEAD", "--not", "--remotes"]);
    match exec(&mut cmd).await {
        Ok(out) if out.trim().parse::<u64>().unwrap_or(1) == 0 => {}
        Ok(_) => return OrphanVerdict::Held("unpushed-commits".into()),
        Err(_) => return OrphanVerdict::Held("unreadable-git".into()),
    }
    OrphanVerdict::Reclaimable
}

/// Label for `GET /api/storage` orphans: young `Skip` orphans are `"pending"`
/// (nothing will reclaim them yet), actually-due ones `"reclaimable"`, held
/// ones their reason.
fn orphan_action(verdict: OrphanVerdict) -> String {
    match verdict {
        OrphanVerdict::Skip => "pending".to_string(),
        OrphanVerdict::Reclaimable => "reclaimable".to_string(),
        OrphanVerdict::Held(reason) => reason,
    }
}
/// Worktree directories exactly 3 levels under `<data_dir>/worktrees`
/// (`owner/name/slug`) that no session's `worktree` names.
fn list_orphan_worktrees(data_dir: &Path, live: &HashSet<String>) -> Vec<(String, String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(owners) = std::fs::read_dir(data_dir.join("worktrees")) else {
        return out;
    };
    for owner in owners.flatten() {
        let owner_name = owner.file_name().to_string_lossy().into_owned();
        let Ok(repos) = std::fs::read_dir(owner.path()) else {
            continue;
        };
        for repo in repos.flatten() {
            let repo_name = repo.file_name().to_string_lossy().into_owned();
            let Ok(slugs) = std::fs::read_dir(repo.path()) else { continue };
            for slug in slugs.flatten() {
                if !live.contains(&slug.path().display().to_string()) {
                    out.push((owner_name.clone(), repo_name.clone(), slug.path()));
                }
            }
        }
    }
    out
}

async fn sweep_orphan_worktrees(app: &Shared, retention_secs: u64) -> (Vec<String>, Vec<String>) {
    let mut removed = Vec::new();
    let mut held = Vec::new();
    let live: HashSet<String> = app.sessions.read().await.iter().map(|s| s.worktree.clone()).collect();
    let data_dir = app.cfg.data_dir.clone();
    let orphans = tokio::task::spawn_blocking(move || list_orphan_worktrees(&data_dir, &live))
        .await
        .unwrap_or_default();
    for (owner, name, wt) in orphans {
        let path = wt.display().to_string();
        match classify_orphan(&wt, retention_secs).await {
            OrphanVerdict::Skip => continue,
            OrphanVerdict::Held(reason) => {
                held.push(format!("{path}: {reason}"));
                continue;
            }
            OrphanVerdict::Reclaimable => {}
        }
        let bare = app.cfg.data_dir.join("repos").join(format!("{owner}/{name}.git"));
        if bare.exists() {
            let _ = exec(app.git(&bare).args(["worktree", "remove", "--force"]).arg(&wt)).await;
        }
        if wt.exists() && tokio::fs::remove_dir_all(&wt).await.is_err() && wt.exists() {
            held.push(format!("{path}: remove-failed"));
            continue;
        }
        if bare.exists() {
            let _ = exec(app.git(&bare).args(["worktree", "prune"])).await;
            if let Some(slug) = wt.file_name().and_then(|n| n.to_str()) {
                let _ = exec(app.git(&bare).args(["branch", "-D"]).arg(format!("colonizer/{slug}"))).await;
            }
        }
        removed.push(path);
    }
    (removed, held)
}

/// MicroVMs no session owns: a `colonizer-<id>` name whose id matches no
/// session and which is not running. A VM with a matching session is never
/// touched here, even if stopped — the watchdogs own those.
async fn sweep_orphan_vms(app: &Shared) -> Vec<String> {
    let mut removed = Vec::new();
    let Ok(all) = sandbox::all(&app.cfg.msb).await else {
        return removed;
    };
    let running = sandbox::running(&app.cfg.msb).await.unwrap_or_default();
    let ids: HashSet<String> = app.sessions.read().await.iter().map(|s| s.id.clone()).collect();
    for name in all {
        let Some(id) = name.strip_prefix("colonizer-") else { continue };
        if ids.contains(id) || running.contains(&name) {
            continue;
        }
        sandbox::remove(&app.cfg.msb, &name).await;
        removed.push(name);
    }
    removed
}

/// The 5-minute auto-reclaim tick; missed ticks are skipped, never piled up.
pub async fn run(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(300));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let report = sweep_once(&app, &ReclaimConfig::from_env()).await;
        if !report.is_empty() {
            eprintln!(
                "reclaim: reclaimed {}, failed {}, orphans removed {}, held {}",
                report.reclaimed.len(),
                report.failed.len(),
                report.orphans_removed.len(),
                report.orphans_held.len()
            );
        }
    }
}

pub async fn admission_paused(app: &Shared) -> bool {
    let cfg = ReclaimConfig::from_env();
    match free_bytes(&app.cfg.data_dir).await {
        Some(free) => free < cfg.min_free_bytes,
        None => false,
    }
}

/// `GET /api/storage`: on-demand, read-only disk accounting — totals, what a
/// sweep would reclaim, and what it would hold back, without deleting anything.
pub async fn storage(State(app): State<Shared>) -> Json<Value> {
    let cfg = ReclaimConfig::from_env();
    let now = Utc::now();
    let sessions = app.sessions.read().await.clone();
    let data_dir = app.cfg.data_dir.clone();
    let paths: Vec<(PathBuf, PathBuf)> = sessions
        .iter()
        .map(|s| (PathBuf::from(&s.worktree), app.session_dir(&s.id)))
        .collect();
    let (wt_total, repos_total, sess_total, sizes) = tokio::task::spawn_blocking(move || {
        let sizes: Vec<u64> = paths.iter().map(|(wt, dir)| dir_size(wt) + dir_size(dir)).collect();
        (
            dir_size(&data_dir.join("worktrees")),
            dir_size(&data_dir.join("repos")),
            dir_size(&data_dir.join("sessions")),
            sizes,
        )
    })
    .await
    .unwrap_or((0, 0, 0, Vec::new()));
    let mut reclaimable = Vec::new();
    let mut unpushed = Vec::new();
    for (s, bytes) in sessions.iter().zip(sizes.into_iter().chain(std::iter::repeat(0))) {
        if pushed_terminal(s) && !s.cleaned_up {
            reclaimable.push(json!({"id": s.id, "status": s.status.as_str(), "pr_url": s.pr_url,
                "bytes": bytes, "updated_at": s.updated_at, "due": reclaim_due(s, now, cfg.retention_secs)}));
        }
        if unpushed_work(s) {
            unpushed.push(json!({"id": s.id, "status": s.status.as_str(), "bytes": bytes, "updated_at": s.updated_at}));
        }
    }
    let live: HashSet<String> = sessions.iter().map(|s| s.worktree.clone()).collect();
    let data_dir = app.cfg.data_dir.clone();
    let orphan_paths = tokio::task::spawn_blocking(move || list_orphan_worktrees(&data_dir, &live))
        .await
        .unwrap_or_default();
    let mut orphans = Vec::new();
    for (_, _, wt) in orphan_paths {
        let path = wt.display().to_string();
        let bytes = tokio::task::spawn_blocking({
            let wt = wt.clone();
            move || dir_size(&wt)
        })
        .await
        .unwrap_or(0);
        let action = orphan_action(classify_orphan(&wt, cfg.retention_secs).await);
        orphans.push(json!({"path": path, "bytes": bytes, "action": action}));
    }
    Json(json!({
        "enabled": cfg.enabled, "retention_secs": cfg.retention_secs, "min_free_bytes": cfg.min_free_bytes,
        "free_bytes": free_bytes(&app.cfg.data_dir).await,
        "totals": {"worktrees_bytes": wt_total, "repos_bytes": repos_total, "sessions_bytes": sess_total},
        "reclaimable": reclaimable, "unpushed": unpushed, "orphans": orphans,
    }))
}

/// `POST /api/sessions/{id}/retain`: opt a colony out of (or back into)
/// automatic reclamation. `{"keep": bool}`, defaulting to true.
pub async fn retain(State(app): State<Shared>, RoutePath(id): RoutePath<String>, Json(body): Json<Value>) -> ApiResult<Session> {
    let keep = body.get("keep").and_then(Value::as_bool).unwrap_or(true);
    let Some((s, ())) = app.update_session(&id, |x| x.keep_worktree = keep).await else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such session"));
    };
    let message = if keep {
        "keeping the worktree: this colony is exempt from automatic reclamation"
    } else {
        "the worktree exemption is lifted: this colony is eligible for automatic reclamation again"
    };
    app.session_log(&id, "info", message.to_string()).await;
    Ok(Json(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::{app_with_colony, colony};

    fn sess(status: SessionStatus, pr: bool, hours_old: i64) -> Session {
        let mut s = colony("acme", status);
        s.pr_url = pr.then(|| "https://github.com/acme/repo/pull/1".into());
        s.updated_at = Utc::now() - chrono::Duration::hours(hours_old);
        s
    }

    #[test]
    fn reclaim_due_needs_a_pushed_terminal_colony_past_retention() {
        let (now, retention) = (Utc::now(), 12 * 3600);
        assert!(reclaim_due(&sess(SessionStatus::PrOpened, true, 13), now, retention));
        assert!(reclaim_due(&sess(SessionStatus::Merged, true, 13), now, retention));
        assert!(reclaim_due(&sess(SessionStatus::Closed, true, 13), now, retention));
        assert!(
            reclaim_due(&sess(SessionStatus::NoChanges, false, 13), now, retention),
            "nothing pushed: nothing to lose"
        );
        assert!(
            !reclaim_due(&sess(SessionStatus::PrOpened, false, 13), now, retention),
            "no pull request"
        );
        assert!(
            !reclaim_due(&sess(SessionStatus::PrOpened, true, 1), now, retention),
            "retention not met"
        );
        assert!(
            !reclaim_due(&sess(SessionStatus::Stopped, true, 13), now, retention),
            "stopped colonies resume"
        );
        assert!(!reclaim_due(&sess(SessionStatus::Failed, true, 13), now, retention));
        assert!(!reclaim_due(&sess(SessionStatus::Running, false, 13), now, retention));
        let mut kept = sess(SessionStatus::PrOpened, true, 13);
        kept.keep_worktree = true;
        assert!(!reclaim_due(&kept, now, retention), "operator opt-out");
        let mut cleaned = sess(SessionStatus::PrOpened, true, 13);
        cleaned.cleaned_up = true;
        assert!(!reclaim_due(&cleaned, now, retention), "already cleaned up");
    }

    #[test]
    fn unpushed_work_is_a_colony_with_no_pr_and_a_worktree() {
        let mut s = colony("acme", SessionStatus::Stopped);
        s.git_admin_dir = Some("/tmp/wt".into());
        assert!(unpushed_work(&s));
        s.pr_url = Some("https://github.com/acme/repo/pull/1".into());
        assert!(!unpushed_work(&s), "a pull request holds the work");
        s.pr_url = None;
        s.cleaned_up = true;
        assert!(!unpushed_work(&s), "cleaned up");
    }

    #[test]
    fn sweep_candidates_come_out_oldest_pushed_first() {
        let now = Utc::now();
        let (mut oldest, mut middle, mut newest) = (
            sess(SessionStatus::PrOpened, true, 20),
            sess(SessionStatus::PrOpened, true, 14),
            sess(SessionStatus::PrOpened, true, 13),
        );
        (oldest.id, middle.id, newest.id) = ("oldest".into(), "middle".into(), "newest".into());
        let ids: Vec<String> = sweep_candidates(&[newest, middle, oldest], now, 3600)
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, ["oldest", "middle", "newest"]);
    }

    #[test]
    fn the_pure_parsers_answer_without_touching_the_environment() {
        assert!(parse_enabled("1") && parse_enabled("yes") && parse_enabled(""));
        for off in ["0", "false", "off", "no", " FALSE ", "Off"] {
            assert!(!parse_enabled(off), "{off:?} disables");
        }
        assert_eq!(parse_retention("12"), 12 * 3600);
        assert_eq!(
            parse_retention("garbage"),
            default_retention_secs(),
            "garbage means the default"
        );
        assert_eq!(parse_retention(""), default_retention_secs());
        assert_eq!(
            parse_retention(&u64::MAX.to_string()),
            i64::MAX as u64,
            "absurd input clamps to i64::MAX"
        );
        assert!(
            !reclaim_due(&sess(SessionStatus::PrOpened, true, 1), Utc::now(), i64::MAX as u64),
            "clamped retention never wraps due for a recently-updated session"
        );
        assert_eq!(parse_min_free("5G"), 5 * 1024 * 1024 * 1024);
        assert_eq!(parse_min_free("garbage"), default_min_free_bytes());
    }

    #[test]
    fn young_orphans_are_pending_not_reclaimable() {
        assert_eq!(orphan_action(OrphanVerdict::Skip), "pending");
        assert_eq!(orphan_action(OrphanVerdict::Reclaimable), "reclaimable");
        assert_eq!(orphan_action(OrphanVerdict::Held("dirty".into())), "dirty");
    }

    #[tokio::test]
    async fn a_sweep_reclaims_an_old_pushed_colony_without_external_binaries() {
        let (app, root) = app_with_colony("old1", SessionStatus::PrOpened).await;
        app.update_session("old1", |s| {
            s.pr_url = Some("https://github.com/acme/repo/pull/1".into());
            s.sandbox = "colonizer-old1".into();
        })
        .await
        .unwrap();
        // `update_session` stamps `updated_at` with now, so backdate directly.
        app.sessions
            .write()
            .await
            .iter_mut()
            .find(|s| s.id == "old1")
            .unwrap()
            .updated_at = Utc::now() - chrono::Duration::hours(13);
        // A fake worktree path and a missing bare repo: `remove_worktree`
        // no-ops on both, and `sandbox::remove` swallows its errors.
        let cfg = ReclaimConfig {
            enabled: true,
            retention_secs: 3600,
            min_free_bytes: 0,
        };
        let report = sweep_once(&app, &cfg).await;
        assert_eq!(report.reclaimed, ["old1"]);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(app.session("old1").await.unwrap().cleaned_up);
        let off = ReclaimConfig {
            enabled: false,
            retention_secs: 0,
            min_free_bytes: 0,
        };
        assert!(sweep_once(&app, &off).await.is_empty(), "disabled means nothing happens");
        let _ = std::fs::remove_dir_all(root);
    }
}
