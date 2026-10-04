//! Issue #968: a conflicted (DIRTY) colony pull request is resolved by a short colony run instead
//! of sitting until a person merges main in. The host merges the base into the colony's own kept
//! worktree — a merge commit, never a rebase, so nothing is rewritten and nothing force-pushed —
//! and a clean merge is pushed as it is. A conflicted one resumes the colony on that worktree with
//! a one-shot brief ([`brief`]): resolve keeping both sides' intent, rerun the generators the
//! repository documents, run its checks, and ask with choices when a conflict needs a decision.
//! Its publish then commits the merge and pushes it to the same pull request. Files the repository
//! marks never to auto-resolve (`.colonizer/merge.toml` `[resolve] never`) go to a person instead.

use super::local_checks::{MERGE_TOML, MergeToml};
use crate::{
    Shared,
    sessions::SessionStatus,
    util::{exec, exec_within, truncate},
};
use axum::extract::{Path as AxumPath, State};
use serde::Deserialize;
use std::{path::Path, time::Duration};

/// The label a pull request gets when a person has to decide.
pub(crate) const NEEDS_HUMAN: &str = "needs-human";
const GIT_LIMIT: Duration = Duration::from_secs(120);

/// `.colonizer/merge.toml`'s `[resolve]` table.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ResolveToml {
    /// Globs of files never resolved automatically: a conflict in one goes to a person.
    pub never: Vec<String>,
    /// Generators to rerun when a matching file conflicted or changed, instead of hand-merging it.
    pub generators: Vec<Generator>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Generator {
    pub files: String,
    pub run: String,
}

/// The conflicted files a repository says never to auto-resolve.
pub(crate) fn never_resolved<'a>(conflicts: &'a [String], cfg: &ResolveToml) -> Vec<&'a str> {
    conflicts
        .iter()
        .filter(|f| cfg.never.iter().any(|g| crate::docs_loop::glob_match(g, f)))
        .map(String::as_str)
        .collect()
}

/// The one-shot brief the resumed colony gets (the session's `resume_note`).
pub(crate) fn brief(pr_url: &str, base: &str, conflicts: &[String], cfg: &ResolveToml) -> String {
    let mut out = format!(
        "Your pull request {pr_url} conflicts with `{base}`. The harness has started `git merge origin/{base}` in your \
         worktree — a merge commit: history is never rewritten and nothing is force-pushed. These files still conflict:\n"
    );
    for f in conflicts.iter().take(40) {
        out.push_str(&format!("- `{f}`\n"));
    }
    if conflicts.len() > 40 {
        out.push_str(&format!(
            "- …and {} more (`git diff --name-only --diff-filter=U`)\n",
            conflicts.len() - 40
        ));
    }
    out.push_str(
        "\n1. Resolve every conflict keeping both sides' intent: your change and what landed on the base since. Remove \
         every conflict marker.\n\
         2. Do not hand-merge generated files: rerun the generator the repository documents (route snapshots, \
         lockfiles, compatibility or error docs, translation catalogs) and take its output.\n",
    );
    let generators: Vec<&Generator> = cfg
        .generators
        .iter()
        .filter(|g| conflicts.iter().any(|f| crate::docs_loop::glob_match(&g.files, f)))
        .collect();
    for g in generators {
        out.push_str(&format!("   - `{}` changed: run `{}`\n", g.files, g.run));
    }
    out.push_str(
        "3. Run the repository's checks and fix what the merge broke.\n\
         4. If a conflict needs a product or security decision — two behaviours that cannot both be kept, a permission, \
         validation or limit one side loosened — do not guess: ask with choices. The pull request is labelled \
         needs-human while you wait.\n\
         5. Do not run `git commit`, `git push` or `git rebase`: when you finish, rewrite pr.md — keep its description and \
         add a line saying the base was merged in and how the conflicts were resolved — and the harness commits the \
         merge and pushes it to the same pull request.\n",
    );
    out
}

/// What starting a resolve did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Started {
    /// The colony was resumed on its worktree with these files conflicting.
    Resuming(Vec<String>),
    /// The base merged in without a conflict and was pushed.
    Clean,
    /// A person has to: why.
    NeedsHuman(String),
    Failed(String),
}

fn worktree_git(app: &Shared, admin: &str, wt: &str) -> tokio::process::Command {
    let mut c = app.git(Path::new(admin));
    c.arg("--work-tree").arg(wt);
    c
}

/// The `[resolve]` table on the base branch, from the bare clone the fetch just refreshed.
async fn config(app: &Shared, bare: &Path, base: &str) -> Result<ResolveToml, String> {
    let mut show = app.git(bare);
    show.args(["show", &format!("refs/remotes/origin/{base}:{MERGE_TOML}")]);
    let Ok(text) = exec_within(GIT_LIMIT, &mut show).await else {
        return Ok(ResolveToml::default());
    };
    let file: MergeToml = toml::from_str(&text).map_err(|e| format!("{MERGE_TOML} does not parse ({})", e.message()))?;
    Ok(file.resolve.unwrap_or_default())
}

/// Merges `origin/<base>` into the colony's kept worktree and either pushes the clean merge or
/// resumes the colony to resolve it. Every way out that is not `Resuming` leaves the worktree as it
/// was found.
pub(super) async fn start(app: &Shared, id: &str, base: &str) -> Started {
    let Some(s) = app.session(id).await else {
        return Started::Failed("the colony is gone".into());
    };
    let (Some(admin), false) = (s.git_admin_dir.clone(), s.cleaned_up) else {
        return Started::NeedsHuman("the colony's worktree was cleaned up, so no colony can resolve it here".into());
    };
    if !Path::new(&s.worktree).is_dir() {
        return Started::NeedsHuman("the colony's worktree is gone, so no colony can resolve it here".into());
    }
    let bare = app.bare_repo(&s.repo);
    let lock = app.repo_lock(&s.repo).await;
    let _guard = lock.lock().await;
    if let Err(e) = exec_within(
        GIT_LIMIT,
        app.git_authed(&bare).args(["fetch", "--quiet", "--prune", "origin"]),
    )
    .await
    {
        return Started::Failed(format!("could not fetch origin ({})", truncate(&format!("{e:#}"), 200)));
    }
    let cfg = match config(app, &bare, base).await {
        Ok(cfg) => cfg,
        Err(e) => return Started::NeedsHuman(e),
    };
    let git = || worktree_git(app, &admin, &s.worktree);
    match exec(git().args(["status", "--porcelain"])).await {
        Ok(status) if status.lines().all(|l| l.starts_with("??")) => {}
        Ok(_) => return Started::Failed("the colony's worktree has uncommitted changes".into()),
        Err(e) => return Started::Failed(format!("{e:#}")),
    }
    // The publish commit's identity, so the merge commit is the colony's like the rest of it.
    let v = crate::github::viewer(app).await.unwrap_or_default();
    let login = v["login"].as_str().unwrap_or("colonizer");
    let email = format!("{}+{login}@users.noreply.github.com", v["id"]);
    let merge = exec_within(
        GIT_LIMIT,
        git()
            .args(["-c", &format!("user.name={login}"), "-c", &format!("user.email={email}")])
            .args(["merge", "--no-edit", &format!("origin/{base}")]),
    )
    .await;
    let abort = || async { exec(git().args(["merge", "--abort"])).await.map(|_| ()) };
    if merge.is_ok() {
        let refspec = format!("refs/heads/{0}:refs/heads/{0}", s.branch);
        // A plain push: the merge only adds to the branch, so anything but a fast-forward is refused.
        return match exec_within(GIT_LIMIT, app.git_authed(&bare).args(["push", "--quiet", "origin", &refspec])).await {
            Ok(_) => Started::Clean,
            Err(e) => Started::Failed(format!(
                "the clean merge could not be pushed ({})",
                truncate(&format!("{e:#}"), 200)
            )),
        };
    }
    let unmerged = exec(git().args(["diff", "--name-only", "--diff-filter=U"]))
        .await
        .unwrap_or_default();
    let conflicts: Vec<String> = unmerged.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
    if conflicts.is_empty() {
        let _ = abort().await;
        return Started::Failed(format!(
            "the merge failed: {}",
            truncate(&format!("{:#}", merge.unwrap_err()), 200)
        ));
    }
    let never = never_resolved(&conflicts, &cfg);
    if !never.is_empty() {
        let _ = abort().await;
        return Started::NeedsHuman(format!(
            "it conflicts in {}, which {MERGE_TOML} says never to auto-resolve",
            never.join(", ")
        ));
    }
    let url = s.pr_url.clone().unwrap_or_default();
    let note = brief(&url, base, &conflicts, &cfg);
    // Resumable is `stopped`: the run publishes the merge to the same pull request and is
    // `pr_opened` again; the brief rides the resume as its one-shot note.
    let flipped = app
        .update_session(id, |x| {
            if x.status != SessionStatus::PrOpened {
                return false;
            }
            x.status = SessionStatus::Stopped;
            x.autopilot = true;
            x.resume_note = Some(note.clone());
            true
        })
        .await
        .is_some_and(|(_, ok)| ok);
    if !flipped {
        let _ = abort().await;
        return Started::Failed("the colony moved on before it could be resumed".into());
    }
    if let Err(e) = crate::lifecycle::resume(State(app.clone()), AxumPath(id.to_string()), None).await {
        let _ = abort().await;
        restore(app, id).await;
        return Started::Failed(format!("the colony could not be resumed ({})", e.message()));
    }
    Started::Resuming(conflicts)
}

/// Puts a colony whose resolve ended without publishing back to `pr_opened` — so the watcher and
/// the train see its pull request again — and aborts the merge it left in its worktree.
pub(super) async fn reset(app: &Shared, id: &str) -> Result<(), String> {
    let s = app.session(id).await.ok_or("the colony is gone")?;
    if let Some(admin) = s.git_admin_dir.as_deref() {
        let lock = app.repo_lock(&s.repo).await;
        let _guard = lock.lock().await;
        let mut c = worktree_git(app, admin, &s.worktree);
        // Without a merge in progress this fails harmlessly; a resolve that already concluded it
        // left nothing to abort.
        let _ = exec(c.args(["merge", "--abort"])).await;
    }
    restore(app, id).await;
    Ok(())
}

async fn restore(app: &Shared, id: &str) {
    app.update_session(id, |x| {
        if matches!(x.status, SessionStatus::Stopped | SessionStatus::Failed) && x.pr_url.is_some() {
            x.status = SessionStatus::PrOpened;
            x.resume_note = None;
        }
    })
    .await;
}

/// Adds [`NEEDS_HUMAN`] to a pull request (the REST call creates the label when it is missing).
pub(super) async fn label(app: &Shared, repo: &str, pr: u64) -> Result<(), String> {
    let mut gh = app.gh([
        "api".to_string(),
        "-X".into(),
        "POST".into(),
        format!("repos/{repo}/issues/{pr}/labels"),
        "-f".into(),
        format!("labels[]={NEEDS_HUMAN}"),
    ]);
    exec_within(Duration::from_secs(60), &mut gh)
        .await
        .map(|_| ())
        .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ResolveToml {
        toml::from_str::<MergeToml>(
            r#"
            [resolve]
            never = ["migrations/**", "SECURITY.md"]
            [[resolve.generators]]
            files = "crates/colonizer/routes.snap"
            run = "UPDATE_ROUTE_SNAPSHOT=1 cargo test -p colonizer-harness route_table"
            "#,
        )
        .unwrap()
        .resolve
        .unwrap()
    }

    #[test]
    fn never_resolved_files_and_generators_come_from_the_repository_file() {
        let conflicts = vec!["src/a.rs".to_string(), "migrations/0042_add.sql".to_string()];
        assert_eq!(never_resolved(&conflicts, &cfg()), vec!["migrations/0042_add.sql"]);
        assert!(never_resolved(&conflicts[..1], &cfg()).is_empty());
        assert!(never_resolved(&conflicts, &ResolveToml::default()).is_empty());
        let snap = vec!["crates/colonizer/routes.snap".to_string()];
        let text = brief("https://github.com/acme/web/pull/7", "main", &snap, &cfg());
        assert!(text.contains("UPDATE_ROUTE_SNAPSHOT=1"), "{text}");
        assert!(!brief("u", "main", &conflicts, &cfg()).contains("UPDATE_ROUTE_SNAPSHOT"));
    }

    #[test]
    fn the_brief_merges_never_rewrites_and_asks_rather_than_guesses() {
        let text = brief(
            "https://github.com/acme/web/pull/7",
            "main",
            &["src/a.rs".to_string()],
            &ResolveToml::default(),
        );
        for needle in [
            "git merge origin/main",
            "nothing is force-pushed",
            "`src/a.rs`",
            "ask with choices",
            "rewrite pr.md",
        ] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
        assert!(text.contains("Do not run `git commit`, `git push` or `git rebase`"));
    }
}
