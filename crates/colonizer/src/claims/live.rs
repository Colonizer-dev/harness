//! The host label and the live claim comment (issue #919).
//!
//! A claimed issue carries `colonizer:claimed` plus `colonizer:host:<host>`, so the owner can filter
//! issues by the mothership working them, and exactly one claim comment, which Colonizer edits as
//! the colony moves (queued, running, waiting for an answer, pull request opened, merged or
//! released) instead of posting a new one. A retry on the same issue edits the same comment and
//! names the colony before it. The comment's hidden `<!-- colonizer:claim … -->` marker is how the
//! comment is found again after a restart.
//!
//! Rate safety: status edits come from one background tick, at most one edit per issue every two
//! minutes (a terminal state is never held back), from the comment id kept in memory, so a tick
//! that has nothing new to say makes no request at all. Comment listings go through
//! `github::gh_get`, a conditional request. A 429, a rate-limit or abuse answer, or a 403 on a
//! write pauses every status edit, doubling from 15 minutes to four hours, like the decisions
//! inbox. A 403 on a label is a missing permission instead: the claim degrades to comment-only
//! with a warning.
//!
//! On by default; an org turns it off with its `claim_updates` setting (no host label, no status
//! edits). The kill switch (`COLONIZER_NO_EXTERNAL_EFFECTS`) stops every write here.

use super::{CLAIM_LABEL, RemoteClaim, ours_or_stale, parse_claim};
use crate::{
    App, Shared,
    sessions::{Session, SessionStatus},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;

pub const HOST_LABEL_PREFIX: &str = "colonizer:host:";
pub const HOST_LABEL_DESCRIPTION: &str = "The Colonizer mothership holding this issue's claim";
/// The least time between two status edits of one issue's comment.
const EDIT_EVERY_SECS: i64 = 120;
const TICK_EVERY: std::time::Duration = std::time::Duration::from_secs(30);
const GH_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);
/// One colour per host, picked by the host's slug so it never changes between boots.
const HOST_COLORS: &[&str] = &["1D76DB", "0E8A16", "B60205", "FBCA04", "5319E7", "D93F0B", "006B75", "C5DEF5"];

/// The host label's slug: the hostname lowercased, anything but ASCII letters and digits as a
/// single dash, at most 30 characters. An empty name falls back to `host`.
pub fn host_slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug: String = slug.chars().take(30).collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() { "host".into() } else { slug.into() }
}

pub fn host_color(slug: &str) -> &'static str {
    let hash = slug
        .bytes()
        .fold(2_166_136_261u32, |h, b| (h ^ b as u32).wrapping_mul(16_777_619));
    HOST_COLORS[hash as usize % HOST_COLORS.len()]
}

/// Who holds a claim: the marker's `"hostname (id)"` and the host label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostTag {
    pub marker: String,
    pub label: String,
}

impl HostTag {
    pub fn new(hostname: Option<&str>, host_id: &str) -> Self {
        let name = hostname.map(str::trim).filter(|n| !n.is_empty());
        let marker = match name {
            Some(name) => format!("{name} ({host_id})"),
            None => host_id.to_string(),
        };
        let short: String = host_id.chars().take(8).collect();
        HostTag {
            marker,
            label: format!("{HOST_LABEL_PREFIX}{}", host_slug(name.unwrap_or(&short))),
        }
    }
}

async fn host_tag(app: &App) -> HostTag {
    let id = crate::runtime::host_id(app);
    HostTag::new(crate::runtime::probe_hostname().await.as_deref(), &id)
}

/// What the comment says about the colony. Never a prompt, a cost or an error: only the status and,
/// for a release, a short fixed reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub issue: u64,
    pub colony: String,
    pub previous: Option<String>,
    pub branch: String,
    pub status: String,
    pub pr_url: Option<String>,
    pub terminal: bool,
}

fn pr_number(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

/// The status line for a session, and whether it is final.
pub fn status_of(status: SessionStatus, pr_url: Option<&str>) -> (String, bool) {
    use SessionStatus::*;
    let pr = || {
        pr_url
            .and_then(pr_number)
            .map(|n| format!("pull request #{n}"))
            .unwrap_or("pull request".into())
    };
    match status {
        Queued => ("queued".into(), false),
        Starting | Running | Idle => ("running".into(), false),
        WaitingForAnswer => ("waiting for an answer".into(), false),
        Publishing => ("opening a pull request".into(), false),
        PrOpened => (format!("{} opened", pr()), false),
        Parked => ("parked".into(), false),
        Merged => (format!("{} merged", pr()), true),
        Closed => (release_reason(status), true),
        Stopped | Failed | NoChanges if pr_url.is_some() => (format!("{} open; colony {}", pr(), status.as_str()), false),
        Stopped | Failed | NoChanges => (release_reason(status), true),
    }
}

/// The released status for a colony that let its issue go.
pub fn release_reason(status: SessionStatus) -> String {
    let why = match status {
        SessionStatus::Closed => "pull request closed unmerged",
        SessionStatus::NoChanges => "finished with no changes",
        SessionStatus::Failed => "colony failed",
        SessionStatus::Stopped => "colony stopped",
        _ => "colony ended",
    };
    format!("released ({why})")
}

pub fn view_of(s: &Session, previous: Option<String>) -> View {
    let (status, terminal) = status_of(s.status, s.pr_url.as_deref());
    View {
        issue: s.issue.unwrap_or_default(),
        colony: s.id.clone(),
        previous,
        branch: s.branch.clone(),
        status,
        pr_url: s.pr_url.clone(),
        terminal,
    }
}

/// Everything in the comment but the update time: what a tick compares to decide an edit is due.
fn summary(view: &View, host: &HostTag) -> String {
    let mut lines = vec![
        format!(
            "<!-- colonizer:claim host=\"{}\" colony=\"{}\" issue=\"{}\" -->",
            super::escape_marker_value(&host.marker),
            super::escape_marker_value(&view.colony),
            view.issue
        ),
        format!("**Colonizer claim** on #{}", view.issue),
        String::new(),
        format!("- Colony: `{}`", view.colony),
        format!("- Host: `{}`", host.marker),
        format!("- Status: {}", view.status),
    ];
    if !view.branch.is_empty() {
        lines.push(format!("- Branch: `{}`", view.branch));
    }
    if let Some(url) = &view.pr_url {
        lines.push(format!("- Pull request: {url}"));
    }
    if let Some(previous) = &view.previous {
        lines.push(format!("- Previous colony: `{previous}`"));
    }
    lines.join("\n")
}

pub fn render(view: &View, host: &HostTag, now: DateTime<Utc>) -> String {
    format!("{}\n- Updated: {}", summary(view, host), now.format("%Y-%m-%d %H:%M UTC"))
}

/// The newest comment carrying a claim marker: its id, the claim it names, and the colony before
/// it, which the comment lists, so a restart keeps that line.
pub fn find_claim_comment(comments: &[(u64, String)]) -> Option<(u64, RemoteClaim, Option<String>)> {
    comments
        .iter()
        .rev()
        .filter(|(_, body)| body.contains("colonizer:claim"))
        .find_map(|(id, body)| parse_claim(body).map(|c| (*id, c, previous_in(body))))
}

fn previous_in(body: &str) -> Option<String> {
    let rest = body.lines().find_map(|l| l.strip_prefix("- Previous colony: `"))?;
    rest.strip_suffix('`').filter(|p| !p.is_empty()).map(str::to_string)
}

/// GitHub pushing back on a request: 429, rate limit, abuse. A bare 403 counts only on a comment
/// write (`merge_loop::is_throttle`); on a label it is a missing permission.
fn rate_limited(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    ["429", "rate limit", "abuse"].iter().any(|p| e.contains(p))
}

/// What this module asks of GitHub, so the tests stand in for it.
pub(crate) trait ClaimGh {
    async fn comments(&self, repo: &str, issue: u64) -> Result<Vec<(u64, String)>, String>;
    async fn create_comment(&self, repo: &str, issue: u64, body: &str) -> Result<u64, String>;
    async fn edit_comment(&self, repo: &str, id: u64, body: &str) -> Result<(), String>;
    async fn create_label(&self, repo: &str, name: &str, color: &str, description: &str) -> Result<(), String>;
    async fn add_labels(&self, repo: &str, issue: u64, labels: &[&str]) -> Result<(), String>;
    async fn remove_label(&self, repo: &str, issue: u64, label: &str) -> Result<(), String>;
}

/// The comment and the edit pace of one claimed issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tracked {
    pub comment_id: u64,
    pub colony: String,
    pub previous: Option<String>,
    pub last: String,
    pub edited_at: DateTime<Utc>,
}

/// Per mothership: the tracked issues and the push-back pause.
#[derive(Debug, Default)]
pub(crate) struct Live {
    pub issues: HashMap<(String, u64), Tracked>,
    /// Issues of live colonies looked up after a restart, found or not: one lookup each.
    pub adopted: HashSet<(String, u64, String)>,
    pub strikes: u32,
    pub paused_until: Option<DateTime<Utc>>,
}

impl Live {
    pub fn paused(&self, now: DateTime<Utc>) -> bool {
        self.paused_until.is_some_and(|p| p > now)
    }

    fn throttled(&mut self, now: DateTime<Utc>) {
        self.strikes += 1;
        self.paused_until = Some(now + crate::decisions::doubled(crate::decisions::BACKOFF_FIRST_MINUTES, self.strikes - 1));
    }

    /// Records a write's outcome: a push-back pauses, anything else clears the strikes.
    fn note(&mut self, result: &Result<(), String>, comment_write: bool, now: DateTime<Utc>) {
        match result {
            Err(e) if rate_limited(e) || (comment_write && crate::merge_loop::is_throttle(e)) => self.throttled(now),
            Ok(()) => self.strikes = 0,
            Err(_) => {}
        }
    }
}

/// Whether a tracked comment's edit is due: something changed, and a terminal state or two minutes
/// since the last edit.
pub(crate) fn edit_due(tracked: &Tracked, summary: &str, terminal: bool, now: DateTime<Utc>) -> bool {
    tracked.last != summary && (terminal || now - tracked.edited_at >= Duration::seconds(EDIT_EVERY_SECS))
}

/// Claims `issue`: edits the issue's existing claim comment (noting the colony before) or posts
/// the first one, then the claim and host labels. Returns what to track; `None` when no comment
/// could be written. Best effort, never fails the launch.
pub(crate) async fn publish_with<G: ClaimGh>(
    gh: &G,
    live: &mut Live,
    repo: &str,
    mut view: View,
    host: &HostTag,
    host_label: bool,
    now: DateTime<Utc>,
) -> Option<Tracked> {
    let existing = match gh.comments(repo, view.issue).await {
        Ok(comments) => find_claim_comment(&comments),
        Err(e) => {
            live.note(&Err(e.clone()), false, now);
            eprintln!("claims: could not read the comments on {repo}#{}: {e}", view.issue);
            None
        }
    };
    if let Some((_, claim, previous)) = &existing {
        view.previous = if claim.colony == view.colony {
            previous.clone()
        } else {
            Some(claim.colony.clone())
        };
    }
    let body = render(&view, host, now);
    let mut id = None;
    if let Some((existing_id, ..)) = existing {
        let edited = gh.edit_comment(repo, existing_id, &body).await;
        live.note(&edited, false, now);
        match edited {
            Ok(()) => id = Some(existing_id),
            // Not editable (another account's comment): a fresh one below.
            Err(e) => eprintln!("claims: could not edit the claim comment on {repo}#{}: {e}", view.issue),
        }
    }
    if id.is_none() {
        match gh.create_comment(repo, view.issue, &body).await {
            Ok(new) => {
                live.note(&Ok(()), true, now);
                id = Some(new);
            }
            Err(e) => {
                live.note(&Err(e.clone()), true, now);
                eprintln!("claims: could not leave the claim comment on {repo}#{}: {e}", view.issue);
            }
        }
    }
    let _ = gh
        .create_label(repo, CLAIM_LABEL, super::CLAIM_LABEL_COLOR, super::CLAIM_LABEL_DESCRIPTION)
        .await;
    let mut labels = vec![CLAIM_LABEL];
    if host_label {
        let slug = host.label.trim_start_matches(HOST_LABEL_PREFIX);
        let _ = gh
            .create_label(repo, &host.label, host_color(slug), HOST_LABEL_DESCRIPTION)
            .await;
        labels.push(host.label.as_str());
    }
    if let Err(e) = gh.add_labels(repo, view.issue, &labels).await {
        live.note(&Err(e.clone()), false, now);
        if host_label && gh.add_labels(repo, view.issue, &[CLAIM_LABEL]).await.is_ok() {
            eprintln!(
                "claims: could not add {} to {repo}#{} ({e}); the claim is comment and claim label only",
                host.label, view.issue
            );
        } else {
            eprintln!(
                "claims: could not label {repo}#{} as claimed ({e}); the claim is comment-only",
                view.issue
            );
        }
    }
    let tracked = Tracked {
        comment_id: id?,
        colony: view.colony.clone(),
        previous: view.previous.clone(),
        last: summary(&view, host),
        edited_at: now,
    };
    live.issues.insert((repo.to_string(), view.issue), tracked.clone());
    Some(tracked)
}

/// One release: which claim, and the short reason its comment ends with.
pub(crate) struct Release<'a> {
    pub repo: &'a str,
    pub issue: u64,
    pub colony: &'a str,
    pub reason: &'a str,
}

/// Releases `colony`'s claim: both labels off, then the comment's final state. Never touches a
/// different colony's claim; a successor that claimed meanwhile gets its claim label back.
pub(crate) async fn release_with<G: ClaimGh>(gh: &G, live: &mut Live, release: Release<'_>, host: &HostTag, now: DateTime<Utc>) {
    let Release {
        repo,
        issue,
        colony,
        reason,
    } = release;
    let key = (repo.to_string(), issue);
    let comments = match gh.comments(repo, issue).await {
        Ok(c) => c,
        Err(e) => {
            live.note(&Err(e.clone()), false, now);
            eprintln!("claims: could not read the comments on {repo}#{issue} to release colony {colony}'s claim: {e}");
            return;
        }
    };
    let latest = find_claim_comment(&comments);
    if !ours_or_stale(latest.as_ref().map(|(_, c, _)| c), colony) {
        eprintln!("claims: leaving the claim on {repo}#{issue} alone: it now names another colony");
        live.issues.remove(&key);
        return;
    }
    // Best effort: either label may already be gone.
    let _ = gh.remove_label(repo, issue, CLAIM_LABEL).await;
    let _ = gh.remove_label(repo, issue, &host.label).await;
    // A successor may have claimed while this ran (issue #321); its claim edits the same comment,
    // so a re-read names it, and the claim label goes back.
    if let Ok(again) = gh.comments(repo, issue).await
        && let Some((_, successor, _)) = find_claim_comment(&again)
        && !ours_or_stale(Some(&successor), colony)
    {
        let _ = gh.add_labels(repo, issue, &[CLAIM_LABEL]).await;
        live.issues.remove(&key);
        return;
    }
    if let Some((id, _, previous)) = latest {
        let branch = super::branch_prefix(issue) + colony;
        let view = View {
            issue,
            colony: colony.to_string(),
            previous,
            branch,
            status: reason.to_string(),
            pr_url: None,
            terminal: true,
        };
        let edited = gh.edit_comment(repo, id, &render(&view, host, now)).await;
        live.note(&edited, true, now);
    }
    live.issues.remove(&key);
}

/// One pass of status edits over `sessions`: a due edit per tracked issue, and at most one lookup
/// of an untracked live colony's comment after a restart. Stops at the first push-back.
pub(crate) async fn tick_with<G: ClaimGh>(
    gh: &G,
    live: &mut Live,
    sessions: &[Session],
    enabled: impl Fn(&str) -> bool,
    host: &HostTag,
    now: DateTime<Utc>,
) {
    if live.paused(now) {
        return;
    }
    let mut looked_up = false;
    for s in sessions {
        let Some(issue) = s.issue else { continue };
        // A colony that lets its issue go is finished by the release, which owns that final edit.
        if !enabled(&s.repo) || super::should_release(&s.status, s.pr_url.as_deref()) {
            continue;
        }
        let key = (s.repo.clone(), issue);
        if !live.issues.contains_key(&key) {
            let adopt = (s.repo.clone(), issue, s.id.clone());
            if looked_up || !crate::duplicates::holds_issue(s) || !live.adopted.insert(adopt) {
                continue;
            }
            looked_up = true;
            match gh.comments(&s.repo, issue).await {
                Ok(comments) => {
                    if let Some((id, claim, previous)) = find_claim_comment(&comments)
                        && claim.colony == s.id
                    {
                        live.issues.insert(
                            key.clone(),
                            Tracked {
                                comment_id: id,
                                colony: s.id.clone(),
                                previous,
                                last: String::new(),
                                edited_at: now - Duration::seconds(EDIT_EVERY_SECS),
                            },
                        );
                    }
                }
                Err(e) => live.note(&Err(e), false, now),
            }
            if live.paused(now) {
                return;
            }
        }
        let Some(tracked) = live.issues.get(&key).cloned() else {
            continue;
        };
        if tracked.colony != s.id {
            continue;
        }
        let view = view_of(s, tracked.previous.clone());
        let text = summary(&view, host);
        if !edit_due(&tracked, &text, view.terminal, now) {
            continue;
        }
        let result = gh.edit_comment(&s.repo, tracked.comment_id, &render(&view, host, now)).await;
        live.note(&result, true, now);
        match result {
            Ok(()) if view.terminal => {
                live.issues.remove(&key);
            }
            outcome => {
                if let Some(t) = live.issues.get_mut(&key) {
                    if outcome.is_ok() {
                        t.last = text;
                    }
                    // A failed edit waits its two minutes too, so a broken comment is not hammered.
                    t.edited_at = now;
                }
            }
        }
        if live.paused(now) {
            return;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The real GitHub, and the mothership's entry points.
// ---------------------------------------------------------------------------------------------

static LIVE: LazyLock<tokio::sync::Mutex<HashMap<PathBuf, Live>>> = LazyLock::new(Default::default);

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

struct RealGh<'a> {
    app: &'a App,
}

impl RealGh<'_> {
    async fn gh(&self, args: Vec<String>) -> Result<String, String> {
        crate::util::exec_within(GH_LIMIT, &mut self.app.gh(args))
            .await
            .map_err(|e| format!("{e:#}"))
    }
}

impl ClaimGh for RealGh<'_> {
    async fn comments(&self, repo: &str, issue: u64) -> Result<Vec<(u64, String)>, String> {
        let mut out = Vec::new();
        // Conditional pages of 100: an unchanged page answers 304, which costs no rate limit.
        for page in 1..=10 {
            let path = format!("repos/{repo}/issues/{issue}/comments?per_page=100&page={page}");
            let (status, body) = crate::github::gh_get(self.app, &path, None)
                .await
                .map_err(|e| format!("{e:#}"))?;
            if status >= 400 {
                return Err(format!("HTTP {status}: {}", crate::util::truncate(body.trim(), 200)));
            }
            let rows: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let rows = rows.as_array().cloned().unwrap_or_default();
            out.extend(
                rows.iter()
                    .filter_map(|c| Some((c["id"].as_u64()?, c["body"].as_str()?.to_string()))),
            );
            if rows.len() < 100 {
                break;
            }
        }
        Ok(out)
    }

    async fn create_comment(&self, repo: &str, issue: u64, body: &str) -> Result<u64, String> {
        let out = self
            .gh(vec![
                "api".into(),
                "-X".into(),
                "POST".into(),
                format!("repos/{repo}/issues/{issue}/comments"),
                "-f".into(),
                format!("body={body}"),
                "--jq".into(),
                ".id".into(),
            ])
            .await?;
        out.trim().parse().map_err(|_| format!("unexpected comment id {out:?}"))
    }

    async fn edit_comment(&self, repo: &str, id: u64, body: &str) -> Result<(), String> {
        self.gh(vec![
            "api".into(),
            "-X".into(),
            "PATCH".into(),
            format!("repos/{repo}/issues/comments/{id}"),
            "-f".into(),
            format!("body={body}"),
            "--silent".into(),
        ])
        .await
        .map(|_| ())
    }

    async fn create_label(&self, repo: &str, name: &str, color: &str, description: &str) -> Result<(), String> {
        match self
            .gh(vec![
                "api".into(),
                "-X".into(),
                "POST".into(),
                format!("repos/{repo}/labels"),
                "-f".into(),
                format!("name={name}"),
                "-f".into(),
                format!("color={color}"),
                "-f".into(),
                format!("description={description}"),
                "--silent".into(),
            ])
            .await
        {
            // Already there is what we wanted.
            Err(e) if e.contains("already_exists") || e.contains("422") => Ok(()),
            other => other.map(|_| ()),
        }
    }

    async fn add_labels(&self, repo: &str, issue: u64, labels: &[&str]) -> Result<(), String> {
        let mut args = vec![
            "api".into(),
            "-X".into(),
            "POST".into(),
            format!("repos/{repo}/issues/{issue}/labels"),
            "--silent".into(),
        ];
        for label in labels {
            args.extend(["-f".into(), format!("labels[]={label}")]);
        }
        self.gh(args).await.map(|_| ())
    }

    async fn remove_label(&self, repo: &str, issue: u64, label: &str) -> Result<(), String> {
        match self
            .gh(vec![
                "api".into(),
                "-X".into(),
                "DELETE".into(),
                format!("repos/{repo}/issues/{issue}/labels/{}", encode(label)),
                "--silent".into(),
            ])
            .await
        {
            Err(e) if e.contains("404") || e.contains("Label does not exist") => Ok(()),
            other => other.map(|_| ()),
        }
    }
}

/// Whether claims in `repo` get the host label and status edits: on unless its org's
/// `claim_updates` is `false`.
pub fn updates_enabled(app: &App, repo: &str) -> bool {
    let org = repo.split('/').next().unwrap_or_default();
    app.org_settings(org).claim_updates != Some(false)
}

/// The live publish behind `claims::publish_claim`.
pub async fn publish(app: &App, repo: &str, issue: u64, colony: &str) -> bool {
    let host = host_tag(app).await;
    let session = app.sessions.read().await.iter().find(|s| s.id == colony).cloned();
    let view = match session {
        Some(s) => view_of(&s, None),
        None => View {
            issue,
            colony: colony.to_string(),
            previous: None,
            branch: super::branch_prefix(issue) + colony,
            status: "queued".into(),
            pr_url: None,
            terminal: false,
        },
    };
    let view = View { issue, ..view };
    let enabled = updates_enabled(app, repo);
    let mut all = LIVE.lock().await;
    let live = all.entry(app.cfg.config_dir.clone()).or_default();
    publish_with(&RealGh { app }, live, repo, view, &host, enabled, Utc::now())
        .await
        .is_some()
}

/// The live release behind `claims::release_claim`.
pub async fn release(app: &App, repo: &str, issue: u64, colony: &str, reason: &str) {
    let host = host_tag(app).await;
    let mut all = LIVE.lock().await;
    let live = all.entry(app.cfg.config_dir.clone()).or_default();
    release_with(
        &RealGh { app },
        live,
        Release {
            repo,
            issue,
            colony,
            reason,
        },
        &host,
        Utc::now(),
    )
    .await;
}

/// The status-edit loop, started with the lifecycle's background work.
pub fn start(app: Shared) {
    tokio::spawn(async move {
        let host = host_tag(&app).await;
        let mut every = tokio::time::interval(TICK_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            if crate::authority::external_writes_blocked() {
                continue;
            }
            let sessions = app.sessions.read().await.clone();
            let mut all = LIVE.lock().await;
            let live = all.entry(app.cfg.config_dir.clone()).or_default();
            tick_with(
                &RealGh { app: &app },
                live,
                &sessions,
                |repo| updates_enabled(&app, repo),
                &host,
                Utc::now(),
            )
            .await;
        }
    });
}

#[cfg(test)]
mod tests;
