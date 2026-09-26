//! Stale-branch tracking (issue #173): a colony branch goes stale silently when its base moves.
//! These two handlers surface that — how far behind a colony is, and a merge of the base into the
//! colony's worktree — so the UI (and the pull request note in `github::compose_pr_body`) can say so.

use crate::{
    ApiResult, App, Shared, client_error,
    github::{count_behind, viewer},
    sessions::SessionStatus,
    util::{exec, exec_status, truncate},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};

/// Whether `r` names something in the bare repo: `rev-parse --verify --quiet` answers in its exit
/// status, so a missing ref is `false`, not an error.
async fn ref_exists(app: &App, bare: &FsPath, r: &str) -> bool {
    exec_status(app.git(bare).args(["rev-parse", "--verify", "--quiet", r]))
        .await
        .unwrap_or(false)
}

/// How far behind `origin/<base>` a colony's branch is, after a best-effort fetch. A colony with no
/// base (an open session) answers `behind_by: null` rather than erroring: there is nothing to be
/// behind.
pub async fn behind(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let s = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let Some(base) = s.base.clone() else {
        return Ok(Json(
            json!({"behind_by": Value::Null, "base": Value::Null, "branch": s.branch}),
        ));
    };
    let bare = app.bare_repo(&s.repo);
    let lock = app.repo_lock(&s.repo).await;
    let _guard = lock.lock().await;
    // Best effort: without a fresh fetch the count may lag, and a failed fetch leaves the
    // last-known answer rather than failing the poll.
    let _ = exec(app.git(&bare).args(["fetch", "--quiet", "--prune", "origin"])).await;
    let behind_by = count_behind(&app, &bare, &s.branch, &base).await;
    Ok(Json(json!({"behind_by": behind_by, "base": base, "branch": s.branch})))
}

/// Merges the colony's base into its worktree, so a stale colony can catch up without leaving the
/// UI. A clean merge answers `merged: true`; a conflicting one leaves the conflicted worktree for
/// the colony to resolve and answers `merged: false` with the conflicted paths.
pub async fn catch_up(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let s = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let base = s
        .base
        .clone()
        .ok_or_else(|| client_error(StatusCode::CONFLICT, "this colony has no base branch to catch up with"))?;
    let admin = s
        .git_admin_dir
        .clone()
        .ok_or_else(|| client_error(StatusCode::CONFLICT, "this colony has no worktree yet"))?;
    if s.cleaned_up {
        return Err(client_error(StatusCode::CONFLICT, "this colony's worktree was cleaned up"));
    }
    let wt = PathBuf::from(&s.worktree);
    if !wt.exists() {
        return Err(client_error(StatusCode::CONFLICT, "this colony's worktree is gone"));
    }
    if matches!(
        s.status,
        SessionStatus::Queued | SessionStatus::Starting | SessionStatus::Running | SessionStatus::Publishing
    ) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "stop the colony first, then catch up: its agent may still be writing",
        ));
    }
    if matches!(s.status, SessionStatus::Merged | SessionStatus::Closed) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "this colony's pull request already left review; there is nothing to catch up",
        ));
    }
    let wt_git = || {
        let mut c = app.git(FsPath::new(&admin));
        c.arg("--work-tree").arg(&wt);
        c
    };
    let bare = app.bare_repo(&s.repo);
    let lock = app.repo_lock(&s.repo).await;
    let _guard = lock.lock().await;
    // Merging a stale base would mislead — the colony would look caught up while still behind — so
    // a failed fetch refuses the merge instead of merging blind.
    if let Err(e) = exec(app.git(&bare).args(["fetch", "--quiet", "--prune", "origin"])).await {
        let message = format!("could not fetch origin; merging a stale base would mislead ({e:#})");
        return Err(client_error(StatusCode::BAD_GATEWAY, &message));
    }
    // A stacked colony's base is another colony's branch, which has no `origin/` ref until it is
    // pushed; the local branch is the merge source then.
    let origin_ref = format!("origin/{base}");
    let merge_ref = if ref_exists(&app, &bare, &origin_ref).await {
        origin_ref
    } else if ref_exists(&app, &bare, &base).await {
        base.clone()
    } else {
        let message = format!("neither origin/{base} nor {base} is here; the base was likely deleted");
        return Err(client_error(StatusCode::CONFLICT, &message));
    };
    // Only non-`??` porcelain lines refuse the merge: untracked files cannot conflict with it.
    let status = exec(wt_git().args(["status", "--porcelain"])).await?;
    if status.lines().any(|line| !line.starts_with("??")) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "the worktree has uncommitted changes; commit or discard them before catching up",
        ));
    }
    // The same identity the publish commit uses, so the merge commit carries the colony's author.
    let v = viewer(&app).await?;
    let login = v["login"].as_str().unwrap_or("colonizer");
    let name = v["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(login);
    let email = format!("{}+{login}@users.noreply.github.com", v["id"]);
    let merge = exec(
        wt_git()
            .arg("-c")
            .arg(format!("user.name={name}"))
            .arg("-c")
            .arg(format!("user.email={email}"))
            .args(["merge", "--no-edit", &merge_ref]),
    )
    .await;
    let behind_now = || count_behind(&app, &bare, &s.branch, &base);
    let fresh = || async {
        app.session(&id)
            .await
            .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))
    };
    match merge {
        Ok(_) => Ok(Json(json!({
            "session": fresh().await?,
            "merged": true,
            "conflicts": Vec::<String>::new(),
            "behind_by": behind_now().await,
        }))),
        Err(e) => {
            let conflicts = exec(wt_git().args(["diff", "--name-only", "--diff-filter=U"]))
                .await
                .unwrap_or_default();
            let conflicts: Vec<String> = conflicts
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(String::from)
                .collect();
            if conflicts.is_empty() {
                let detail = truncate(&format!("{e:#}"), 2000);
                let message = format!("could not merge {merge_ref} into {}: {detail}", s.branch);
                return Err(client_error(StatusCode::CONFLICT, &message));
            }
            Ok(Json(json!({
                "session": fresh().await?,
                "merged": false,
                "conflicts": conflicts,
                "behind_by": behind_now().await,
            })))
        }
    }
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/sessions/{id}/behind", routing::get(behind))
        .route("/api/sessions/{id}/catch-up", routing::post(catch_up))
}
