//! Provenance ledger (issue #1324): an append-only JSONL record of every external resource
//! (worktrees, branches, pull requests) that colonies create. Each entry records the kind, id,
//! scope, creation time, colony id, issue/PR, tool, reason, dependencies, and verification status.
//!
//! The ledger lives at `<data_dir>/provenance.jsonl` and is tolerant of malformed lines.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

const LEDGER_FILE: &str = "provenance.jsonl";

/// The resource kind that was created.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Branch,
    PullRequest,
    Worktree,
}

/// The verification status of a recorded entry.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Verified,
    Unverified,
    Failed { note: String },
}

/// One entry in the provenance ledger: a resource created by the harness.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    pub kind: Kind,
    pub id: String,
    pub scope: String,
    pub created_at: String,
    pub colony: String,
    pub issue: Option<u64>,
    pub tool: String,
    pub reason: String,
    pub depends_on: Vec<String>,
    pub status: Status,
}

/// Record a new entry in the provenance ledger. Best-effort: failures are logged
/// and never fail the caller.
pub async fn record(app: &crate::App, entry: Entry) -> Result<()> {
    let path = app.cfg.data_dir.join(LEDGER_FILE);
    let line = serde_json::to_string(&entry)?;
    if let Err(e) = crate::util::append_line(&path, &line).await {
        warn!(path = %path.display(), error = %e, "failed to record provenance entry");
    }
    Ok(())
}

/// Helper to build a worktree entry. Verification status depends on path existence.
pub fn worktree_entry(path: &std::path::Path, scope: String, colony: String, issue: Option<u64>, issue_title: String) -> Entry {
    let status = if path.exists() && path.is_dir() {
        Status::Verified
    } else {
        Status::Failed {
            note: "worktree path does not exist after creation".to_string(),
        }
    };
    Entry {
        kind: Kind::Worktree,
        id: path.display().to_string(),
        scope,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        colony,
        issue,
        tool: "boot::create_worktree".to_string(),
        reason: issue_title,
        depends_on: vec![],
        status,
    }
}

/// Helper to build a branch entry.
pub fn branch_entry(branch: String, scope: String, colony: String, issue: Option<u64>, issue_title: String) -> Entry {
    Entry {
        kind: Kind::Branch,
        id: branch,
        scope,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        colony,
        issue,
        tool: "github::publish".to_string(),
        reason: issue_title,
        depends_on: vec![],
        status: Status::Verified,
    }
}

/// Helper to build a pull request entry.
pub fn pull_request_entry(
    url: String,
    scope: String,
    colony: String,
    issue: Option<u64>,
    issue_title: String,
    branch_id: String,
) -> Entry {
    Entry {
        kind: Kind::PullRequest,
        id: url,
        scope,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        colony,
        issue,
        tool: "github::publish".to_string(),
        reason: issue_title,
        depends_on: vec![branch_id],
        status: Status::Verified,
    }
}

/// Load all entries from the provenance ledger, skipping malformed lines.
pub async fn load(app: &crate::App) -> Vec<Entry> {
    let path = app.cfg.data_dir.join(LEDGER_FILE);
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => content
            .lines()
            .filter_map(|line| {
                if line.is_empty() {
                    return None;
                }
                match serde_json::from_str::<Entry>(line) {
                    Ok(entry) => Some(entry),
                    Err(e) => {
                        warn!(line = %line, error = %e, "skipped malformed provenance entry");
                        None
                    }
                }
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "failed to read provenance ledger");
            Vec::new()
        }
    }
}

/// Entries that depend on the given id: the removal impact of deleting `id`.
pub fn removal_impact<'a>(entries: &'a [Entry], id: &str) -> Vec<&'a Entry> {
    entries.iter().filter(|e| e.depends_on.contains(&id.to_string())).collect()
}

/// The result of reconciling the ledger against actual worktree files on disk.
#[derive(Clone, Debug, Default, Serialize)]
pub struct WorktreeReconciliation {
    pub missing: Vec<String>,
    pub orphans: Vec<String>,
}

/// Reconcile worktree entries in the ledger against the actual worktrees on disk.
pub fn reconcile_worktrees(entries: &[Entry], existing_paths: &[PathBuf]) -> WorktreeReconciliation {
    let mut result = WorktreeReconciliation::default();
    let entry_paths: std::collections::HashSet<String> = entries
        .iter()
        .filter(|e| e.kind == Kind::Worktree)
        .map(|e| e.id.clone())
        .collect();

    // Find missing entries
    for path_str in &entry_paths {
        if !existing_paths.iter().any(|p| p.to_string_lossy() == *path_str) {
            result.missing.push(path_str.clone());
        }
    }

    // Find orphans
    for path in existing_paths {
        let path_str = path.to_string_lossy().to_string();
        if !entry_paths.contains(path_str.as_str()) {
            result.orphans.push(path_str);
        }
    }

    result
}

/// List all worktree directories under `<data_dir>/worktrees`, identified by
/// the presence of a `.git` file (created by git worktree). This avoids
/// false positives from branch names containing slashes.
pub fn list_worktree_paths(data_dir: &Path) -> Result<Vec<PathBuf>> {
    let root = data_dir.join("worktrees");
    let mut paths = Vec::new();

    if !root.exists() {
        return Ok(paths);
    }

    fn find_worktrees(dir: &Path, results: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // If this directory has a .git file, it's a worktree
                if path.join(".git").exists() {
                    results.push(path);
                } else {
                    // Otherwise, recurse into it
                    find_worktrees(&path, results);
                }
            }
        }
    }

    find_worktrees(&root, &mut paths);
    Ok(paths)
}

use axum::Json;
use serde_json::json;

/// The routes for the provenance ledger feature.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "provenance_ledger",
    routes,
    token_scope: None,
    activity: &[],
    kinds: &[],
    start_tasks: None,
};

fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/provenance", routing::get(get_provenance))
}

/// `GET /api/provenance?impact=<id>` — the provenance ledger, worktree reconciliation, and optional removal impact.
/// If `impact` query param is given, also returns entries that depend on the given id.
pub async fn get_provenance(
    axum::extract::State(app): axum::extract::State<crate::Shared>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let entries = load(app.as_ref()).await;
    let worktrees = match list_worktree_paths(&app.cfg.data_dir) {
        Ok(paths) => reconcile_worktrees(&entries, &paths),
        Err(e) => {
            warn!(error = %e, "failed to list worktrees for reconciliation");
            WorktreeReconciliation::default()
        }
    };

    let mut response = json!({
        "entries": entries,
        "worktrees": worktrees
    });

    // Include impact list if impact query param is provided
    if let Some(impact_id) = params.get("impact") {
        let impact = removal_impact(&entries, impact_id);
        response["impact"] = json!(impact.into_iter().cloned().collect::<Vec<_>>());
    }

    Json(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_impact_finds_dependencies() {
        let branch_entry = Entry {
            kind: Kind::Branch,
            id: "branch-1".to_string(),
            scope: "owner/repo".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            colony: "col-1".to_string(),
            issue: Some(123),
            tool: "github::publish".to_string(),
            reason: "test".to_string(),
            depends_on: vec![],
            status: Status::Verified,
        };

        let pr_entry = Entry {
            kind: Kind::PullRequest,
            id: "https://github.com/o/r/pull/1".to_string(),
            scope: "owner/repo".to_string(),
            created_at: "2026-01-01T00:00:01Z".to_string(),
            colony: "col-1".to_string(),
            issue: Some(123),
            tool: "github::publish".to_string(),
            reason: "test".to_string(),
            depends_on: vec!["branch-1".to_string()],
            status: Status::Verified,
        };

        let entries = vec![branch_entry, pr_entry];
        let impact = removal_impact(&entries, "branch-1");

        assert_eq!(impact.len(), 1);
        assert_eq!(impact[0].kind, Kind::PullRequest);
    }

    #[test]
    fn reconcile_worktrees_finds_missing_and_orphans() {
        let entry = Entry {
            kind: Kind::Worktree,
            id: "/path/to/wt1".to_string(),
            scope: "localhost".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            colony: "col-1".to_string(),
            issue: None,
            tool: "boot::create_worktree".to_string(),
            reason: "test".to_string(),
            depends_on: vec![],
            status: Status::Verified,
        };

        let entries = vec![entry];
        let existing = vec![PathBuf::from("/path/to/wt1"), PathBuf::from("/path/to/wt2")];

        let reconciled = reconcile_worktrees(&entries, &existing);
        assert_eq!(reconciled.missing.len(), 0);
        assert_eq!(reconciled.orphans.len(), 1);
        assert_eq!(reconciled.orphans[0], "/path/to/wt2");
    }

    #[test]
    fn list_worktree_paths_empty_when_no_worktrees() {
        // Verify that the function returns empty when worktrees dir doesn't exist
        let nonexistent = PathBuf::from("/nonexistent/path");
        let paths = list_worktree_paths(&nonexistent).unwrap();
        assert!(paths.is_empty());
    }
}
