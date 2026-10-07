//! What stands between a finished colony and a landed push (issue #1206): a branch someone else
//! moved on GitHub, and a secret-shaped literal GitHub's push protection refuses.
//!
//! Neither is the colony's failure, so neither fails it. A non-fast-forward rejection fetches the
//! remote branch and folds the colony's new commits onto it ([`integrate`]: rebase, then merge) and
//! pushes again; a conflict, and a secret-shaped literal found in the diff before the push
//! ([`scan_diff`]) or named by a `GH013` rejection ([`parse_gh013`]), come back as a
//! [`PublishHold`], which `publish_session` turns into one resume of the colony with a note saying
//! what to do. A note names `path:line` and the kind of secret — never the value.

use crate::{exec_bits::GitRun, github::HOST_GIT_IDENTITY};
use anyhow::Result;
use std::future::Future;

/// How many times one publish fetches, folds in the remote branch and pushes again.
pub(crate) const MAX_SYNCS: u32 = 2;

/// One secret-shaped literal: where it is and what it looks like, never its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretSpot {
    pub path: String,
    pub line: usize,
    pub kind: String,
}

/// A publish that stopped for something the colony can fix. Carried as the error of the publish and
/// recognised by `downcast_ref`, so nothing between the push and `publish_session` has to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PublishHold {
    /// Secret-shaped literals in the diff, or a push protection rejection. May name no spot at all
    /// when GitHub refused without saying where.
    Secrets(Vec<SecretSpot>),
    /// The branch moved on GitHub and the colony's commits do not fold onto it.
    Conflict { files: Vec<String> },
}

impl std::fmt::Display for PublishHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Secrets(spots) if spots.is_empty() => {
                write!(
                    f,
                    "GitHub push protection refused the push: the branch carries a secret-shaped literal"
                )
            }
            Self::Secrets(spots) => write!(
                f,
                "the branch carries secret-shaped literals ({}); nothing was pushed",
                spots.iter().map(spot_label).collect::<Vec<_>>().join(", ")
            ),
            Self::Conflict { files } if files.is_empty() => {
                write!(f, "the branch moved on GitHub and the colony's commits do not fold onto it")
            }
            Self::Conflict { files } => write!(
                f,
                "the branch moved on GitHub and the colony's commits conflict with it in {}",
                files.join(", ")
            ),
        }
    }
}

impl std::error::Error for PublishHold {}

fn spot_label(spot: &SecretSpot) -> String {
    format!("{}:{} {}", spot.path, spot.line, kind_label(&spot.kind))
}

fn kind_label(kind: &str) -> String {
    kind.replace('_', " ")
}

/// Whether a failed push says the remote branch has commits the push does not carry.
pub(crate) fn is_non_fast_forward(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("non-fast-forward") || lower.contains("fetch first")
}

/// Lockfiles are all hashes; scanning them only finds noise.
fn is_lockfile(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.ends_with(".lock")
        || name.ends_with(".lockb")
        || matches!(
            name,
            "package-lock.json" | "npm-shrinkwrap.json" | "pnpm-lock.yaml" | "go.sum"
        )
}

/// The secret-shaped literals on the lines a unified diff adds, in order: `colonizer_redact` over
/// each added line, so it knows every kind the logs redact. Reports `path:line` in the new file and
/// the kind; the line itself goes nowhere.
pub(crate) fn scan_diff(diff: &str) -> Vec<SecretSpot> {
    let mut out = Vec::new();
    let mut path: Option<String> = None;
    let mut next_line = 0usize;
    let mut in_hunk = false;
    for raw in diff.lines() {
        if raw.starts_with("diff --git ") {
            path = None;
            in_hunk = false;
            continue;
        }
        if !in_hunk {
            if let Some(rest) = raw.strip_prefix("+++ ") {
                path = rest
                    .strip_prefix("b/")
                    .map(|p| p.trim_matches('"').to_string())
                    .filter(|p| !is_lockfile(p));
                continue;
            }
            if let Some(rest) = raw.strip_prefix("@@ ") {
                next_line = hunk_new_start(rest);
                in_hunk = true;
            }
            continue;
        }
        if let Some(rest) = raw.strip_prefix("@@ ") {
            next_line = hunk_new_start(rest);
            continue;
        }
        match raw.as_bytes().first() {
            Some(b'+') => {
                if let Some(path) = &path {
                    for (_, kind) in colonizer_redact::findings(&raw[1..]) {
                        out.push(SecretSpot {
                            path: path.clone(),
                            line: next_line,
                            kind,
                        });
                    }
                }
                next_line += 1;
            }
            Some(b'-') | Some(b'\\') => {}
            _ => next_line += 1,
        }
    }
    out
}

/// The new file's first line of a hunk header's tail (`-1,3 +7,4 @@ ...` → 7).
fn hunk_new_start(header: &str) -> usize {
    header
        .split_whitespace()
        .find_map(|part| part.strip_prefix('+'))
        .and_then(|range| range.split(',').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
}

/// Whether a push failure is GitHub's secret scanning refusing it (`GH013`).
pub(crate) fn is_secret_rejection(text: &str) -> bool {
    text.contains("GH013") || text.contains("Push cannot contain secrets")
}

/// The spots a `GH013` rejection names. GitHub prints each secret type as `—— <Kind> ——` and each
/// place as `path: <file>:<line>` under it; a path with no line is reported at line 1. Empty when the
/// text is no secret rejection, or when GitHub named no place.
pub(crate) fn parse_gh013(text: &str) -> Vec<SecretSpot> {
    if !is_secret_rejection(text) {
        return Vec::new();
    }
    let mut kind = String::from("secret");
    let mut out: Vec<SecretSpot> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_start_matches("remote:").trim();
        if let Some(rest) = line.strip_prefix("——").or_else(|| line.strip_prefix("--")) {
            let name = rest
                .split("——")
                .next()
                .unwrap_or(rest)
                .trim_matches(|c: char| c == '-' || c == '—' || c.is_whitespace());
            if !name.is_empty() {
                kind = name.to_lowercase().replace(' ', "_");
            }
            continue;
        }
        let Some(location) = line.strip_prefix("path:").map(str::trim) else {
            continue;
        };
        let (path, line_no) = match location.rsplit_once(':') {
            Some((path, n)) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => (path, n.parse().unwrap_or(1)),
            _ => (location, 1),
        };
        let spot = SecretSpot {
            path: path.to_string(),
            line: line_no,
            kind: kind.clone(),
        };
        if !path.is_empty() && !out.contains(&spot) {
            out.push(spot);
        }
    }
    out
}

/// The note a colony is resumed with for secret-shaped literals: where, what kind, and how to
/// write the value without ever having it in the tree. Never carries a value.
pub(crate) fn secrets_note(spots: &[SecretSpot], base: &str) -> String {
    let mut note = String::from(
        "GitHub rejects pushes that contain secret-shaped strings, and your branch has some. \
         Nothing was pushed.\n",
    );
    if spots.is_empty() {
        note.push_str("GitHub did not say where; look at the files you added, test fixtures first.\n");
    }
    for spot in spots {
        note.push_str(&format!(
            "- `{}:{}` contains a {}-shaped literal\n",
            spot.path,
            spot.line,
            kind_label(&spot.kind)
        ));
    }
    note.push_str(&format!(
        "Do not write such a value literally anywhere. In test fixtures build it at runtime from fragments \
         (`concat!` in Rust, string concatenation or a join elsewhere) or use a value with a clearly fake prefix, and \
         never copy the old value into a message or a file. The literal is also in your earlier commits, which GitHub \
         scans too: after fixing the files, rewrite the branch so no commit contains it (for example `git reset --soft \
         origin/{base}` and commit again, or an amend), re-run this repo's gates and finish so the work is published again."
    ));
    note
}

/// The note a colony is resumed with when its branch moved on GitHub and its commits conflict.
pub(crate) fn conflict_note(branch: &str, files: &[String]) -> String {
    let files = if files.is_empty() {
        String::new()
    } else {
        format!(" in {}", files.join(", "))
    };
    format!(
        "Your branch `{branch}` was updated on GitHub after you started (a merge of the base branch, a rebase or \
         update-branch), and your new commits conflict with it{files}. Fetch `origin/{branch}`, rebase your commits \
         onto it (or merge it), resolve the conflicts (changelog fragments and generated files are mechanical; take \
         both sides), re-run this repo's gates, and finish so the work is published again."
    )
}

/// Runs `push` until it lands, folding the remote branch in with `sync` after each non-fast-forward
/// rejection, at most `max_syncs` times. Any other failure, and a rejection past the budget, is the
/// push's own error. Returns how many syncs it took.
pub(crate) async fn push_syncing<P, PF, S, SF>(mut push: P, mut sync: S, max_syncs: u32) -> Result<u32>
where
    P: FnMut() -> PF,
    PF: Future<Output = Result<()>>,
    S: FnMut() -> SF,
    SF: Future<Output = Result<()>>,
{
    let mut syncs = 0;
    loop {
        match push().await {
            Ok(()) => return Ok(syncs),
            Err(e) if syncs < max_syncs && is_non_fast_forward(&format!("{e:#}")) => {
                syncs += 1;
                sync().await?;
            }
            Err(e) => return Err(e),
        }
    }
}

/// How the colony's commits were folded onto the remote branch.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Integrated {
    Rebased,
    Merged,
}

fn with_identity(args: &[&str]) -> Vec<String> {
    HOST_GIT_IDENTITY
        .iter()
        .map(|s| s.to_string())
        .chain(args.iter().map(|s| s.to_string()))
        .collect()
}

async fn unmerged(git: &mut impl GitRun) -> Vec<String> {
    git.run_git(vec!["diff".into(), "--name-only".into(), "--diff-filter=U".into()])
        .await
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Folds the commits on HEAD that `remote_ref` lacks onto it: a rebase first (commits already on the
/// remote by patch id drop out, so a branch someone rebased or merged main into replays only the
/// colony's new work), a merge when the rebase cannot. A failure aborts what it started, so the
/// worktree is never left mid-operation, and comes back as [`PublishHold::Conflict`].
pub(crate) async fn integrate(git: &mut impl GitRun, remote_ref: &str) -> Result<Integrated, PublishHold> {
    if git.run_git(with_identity(&["rebase", remote_ref])).await.is_ok() {
        return Ok(Integrated::Rebased);
    }
    let _ = git.run_git(vec!["rebase".into(), "--abort".into()]).await;
    if git.run_git(with_identity(&["merge", "--no-edit", remote_ref])).await.is_ok() {
        return Ok(Integrated::Merged);
    }
    let files = unmerged(git).await;
    let _ = git.run_git(vec!["merge".into(), "--abort".into()]).await;
    Err(PublishHold::Conflict { files })
}

#[cfg(test)]
mod tests;
