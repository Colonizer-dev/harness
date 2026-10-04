//! Which files merged pull requests touch most often in a window — where parallel colonies
//! collide, and a hint at what to split (issue #831). The activity events carry no file lists,
//! so the data is the repository's own history: a squash-merged pull request is a commit whose
//! subject ends in `(#NNN)` (sometimes two — the last is the pull request), an older one a true
//! merge commit `Merge pull request #NNN from …`. Both carry their file list under
//! `git log --first-parent -m --name-only`, so one `git log` over the main line says which files
//! the most distinct pull requests touched. Everything but [`read`] and [`command`] is pure.

use crate::Settings;
use anyhow::{Context as _, Result, bail};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The window `colonizer hotspots` looks back over when `--days` says nothing.
pub const DEFAULT_DAYS: u64 = 30;
/// How many files it lists when `--top` says nothing.
pub const DEFAULT_TOP: usize = 15;

/// The record separator the git log format puts before each commit's subject (`%x1e`).
const RECORD: char = '\x1e';

/// One file and how many distinct pull requests in the window touched it.
#[derive(Debug, PartialEq, Eq)]
pub struct Hotspot {
    pub path: String,
    pub prs: usize,
}

/// The whole answer: how many pull requests the window held, and the ranked files.
#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub prs: usize,
    pub files: Vec<Hotspot>,
}

/// The pull request a commit subject names: a squash-merge subject ends in `(#NNN)` (the last one
/// wins when the title carries two), an older true merge starts `Merge pull request #NNN`.
/// `None` when it names neither — an ordinary or unmerged commit, which is not merged work.
pub fn pr_number(subject: &str) -> Option<u64> {
    let subject = subject.trim();
    if let Some(rest) = subject.strip_prefix("Merge pull request #") {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        return digits.parse().ok();
    }
    subject.strip_suffix(')')?.rsplit_once("(#")?.1.parse().ok()
}

/// The always-touched files that say nothing about collisions: the changelog every pull request
/// edits, the generated lockfile, `changelog.d/` fragments and the route snapshots.
fn is_noise(path: &str) -> bool {
    path == "CHANGELOG.md" || path == "Cargo.lock" || path.starts_with("changelog.d/") || path.ends_with("routes.snap")
}

/// Parses `git log --first-parent -m --name-only --format=%x1e%s` output into a [`Report`]:
/// commits without a pull request number contribute nothing, two commits of the same pull
/// request count once per file, and the noise files ([`is_noise`]) are dropped. Ranked by count
/// descending, then path ascending, and cut to `top`.
pub fn parse(log: &str, top: usize) -> Report {
    let mut by_file: HashMap<&str, HashSet<u64>> = HashMap::new();
    let mut prs: HashSet<u64> = HashSet::new();
    for chunk in log.split(RECORD) {
        let mut lines = chunk.lines();
        let Some(pr) = lines.next().and_then(pr_number) else {
            continue;
        };
        prs.insert(pr);
        for path in lines.map(str::trim).filter(|l| !l.is_empty()) {
            if !is_noise(path) {
                by_file.entry(path).or_default().insert(pr);
            }
        }
    }
    let mut files: Vec<Hotspot> = by_file
        .into_iter()
        .map(|(path, prs)| Hotspot {
            path: path.to_string(),
            prs: prs.len(),
        })
        .collect();
    files.sort_by(|a, b| b.prs.cmp(&a.prs).then_with(|| a.path.cmp(&b.path)));
    files.truncate(top);
    Report { prs: prs.len(), files }
}

/// The git directory to read: `--git-dir` as given, else `--repo`'s bare mirror under the data
/// dir (the same rule as `App::bare_repo`), else `None` — the repository in the current directory.
fn resolve_dir(repo: Option<&str>, git_dir: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(dir) = git_dir {
        return Ok(Some(dir.to_path_buf()));
    }
    let Some(repo) = repo else {
        return Ok(None);
    };
    if !crate::util::valid_repo(repo) {
        bail!("--repo is owner/repo (got {repo:?})");
    }
    let cfg = Settings::from_env().context("reading settings to find the repository mirror")?;
    Ok(Some(cfg.data_dir.join("repos").join(format!("{repo}.git"))))
}

/// Runs the git log over `dir` (a bare mirror, a worktree's `.git`, or `None` for the current
/// directory) and parses it.
fn read(dir: Option<&Path>, days: u64, top: usize) -> Result<Report> {
    let mut cmd = crate::github::host_git_offline();
    if let Some(dir) = dir {
        cmd.arg("--git-dir").arg(dir);
    }
    let since = format!("--since={days} days ago");
    let out = cmd
        .args(["log", "--first-parent", "-m", "--name-only", &since, "--format=%x1e%s"])
        .stdin(std::process::Stdio::null())
        .output()
        .context("running git log")?;
    if !out.status.success() {
        let where_ = dir.map(|d| d.display().to_string()).unwrap_or_else(|| ".".into());
        bail!("git log failed in {where_}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(parse(&String::from_utf8_lossy(&out.stdout), top))
}

/// `colonizer hotspots`: the files merged pull requests touched most often in a window. Local: it
/// reads a repository's git history on this machine, no mothership.
pub fn command(repo: Option<&str>, git_dir: Option<&Path>, days: u64, top: usize, json: bool) -> Result<()> {
    let dir = resolve_dir(repo, git_dir)?;
    let report = read(dir.as_deref(), days, top)?;
    if json {
        let files: Vec<_> = report.files.iter().map(|h| json!({ "path": h.path, "prs": h.prs })).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "days": days,
                "pull_requests": report.prs,
                "files": files,
            }))?
        );
        return Ok(());
    }
    println!("hotspots over the last {days} days: {} merged pull requests", report.prs);
    for h in &report.files {
        println!("{}  {}", h.prs, h.path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two squash merges sharing a file, a double-numbered subject, a plain commit, and a true
    /// merge — with every noise file on the last one.
    const LOG: &str = concat!(
        "\x1eAdd the thing (#10)\n\nsrc/a.rs\ndocs/x.md\n",
        "\x1eFix the other (#11)\n\nsrc/a.rs\nweb/types.ts\n",
        "\x1eDouble (#589) (#837)\n\nsrc/b.rs\n",
        "\x1eNo pull request here\n\nsrc/c.rs\n",
        "\x1eMerge pull request #20 from colonizer/issue-1\n\nsrc/a.rs\n",
        "CHANGELOG.md\nchangelog.d/12.added.md\nCargo.lock\ncrates/colonizer/routes.snap\n",
    );

    fn ranked(report: &Report) -> Vec<(&str, usize)> {
        report.files.iter().map(|h| (h.path.as_str(), h.prs)).collect()
    }

    #[test]
    fn a_subject_names_its_pull_request() {
        assert_eq!(pr_number("Add the thing (#10)"), Some(10));
        // Two numbers on one title: the last is the squash's own.
        assert_eq!(pr_number("Double (#589) (#837)"), Some(837));
        assert_eq!(pr_number("Merge pull request #20 from colonizer/issue-1"), Some(20));
        assert_eq!(pr_number("No pull request here"), None);
        // A number that is not a trailing `(#N)` does not count.
        assert_eq!(pr_number("Reference #7 in passing"), None);
    }

    #[test]
    fn counts_distinct_pull_requests_per_file_and_drops_noise() {
        let report = parse(LOG, 15);
        assert_eq!(report.prs, 4, "only the commits with a pull request number count");
        // Ranked by count, then path; the pull-request-less commit's file and every noise file
        // are absent.
        assert_eq!(
            ranked(&report),
            vec![("src/a.rs", 3), ("docs/x.md", 1), ("src/b.rs", 1), ("web/types.ts", 1)]
        );
    }

    #[test]
    fn the_top_cut_keeps_the_highest_counts() {
        let report = parse(LOG, 2);
        assert_eq!(report.prs, 4, "the window's pull requests are counted whole");
        assert_eq!(ranked(&report), vec![("src/a.rs", 3), ("docs/x.md", 1)]);
    }

    #[test]
    fn the_repo_flag_must_be_owner_slash_repo() {
        // Anything but owner/repo is refused before any path is built, so `../../x` cannot escape
        // the data dir's repos/ and `/etc/passwd` cannot name an absolute one.
        for bad in ["../../x", "/etc/passwd", "acme", "a/b/c", "acme/..", ""] {
            assert!(resolve_dir(Some(bad), None).is_err(), "{bad:?} must be refused");
        }
    }
}
