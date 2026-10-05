//! The decisions inbox (issue #1036): what only a person can settle, beside the colonies' own
//! questions in the cockpit's "Needs you".
//!
//! Two kinds of card:
//!
//! - **Repo decisions.** Open issues labelled `needs-decision`, or whose body or latest comment has
//!   a line starting "Open decision:" or "Decision needed:", in the orgs the operator opted in.
//!   Options come from a Markdown list under an "Options:" line after the question; without one the
//!   card offers free text. Answering posts one comment starting "Decision (maintainer):" and
//!   removes the label when the issue carries it. Nothing is posted without the operator's click.
//! - **Pull requests that need a person.** A review requested from the operator; a colony pull
//!   request the merge train marked `needs_redo`, or one that conflicts and the auto-rebase could
//!   not fix; red CI that is not a known flake the merge loop is re-running; a colony held by
//!   policy (a control-defeat flag, a secret redacted from its description, a refused publish);
//!   and a green pull request in a repository nothing merges on its own.
//!
//! Only the issue search reaches GitHub: one search per opted-in org, at most every five minutes
//! per org and at most one org per minute, as a conditional request through `github::gh_get` (a
//! 304 is free), with the latest comment of a matching issue read only when the issue changed. A
//! 403/429, abuse or secondary-rate-limit answer pauses every poll, doubling from 15 minutes to
//! four hours. Everything else — the pull-request cards — is read from what the mothership already
//! knows: its colonies, the merge-train loop's memory and its last report. The search answers are
//! cached in memory; per-org opt-in and the answered issues live in `<config_dir>/decisions.json`.
//! `COLONIZER_NO_EXTERNAL_EFFECTS` leaves the inbox read-only.

use crate::{
    ApiResult, Shared, authority, client_error, github,
    merge_loop::{self, Action, LoopState},
    merge_train,
    sessions::{self, NewSession, Session, SessionStatus},
    util::{truncate, valid_repo},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

/// The label that marks an issue as waiting on a decision.
pub(crate) const LABEL: &str = "needs-decision";
/// The line starts that ask for a decision in an issue's text, compared without case.
const MARKERS: &[&str] = &["open decision:", "decision needed:"];
/// How an answer comment starts.
pub(crate) const ANSWER_PREFIX: &str = "Decision (maintainer):";
/// The least time between two searches of one org.
pub(crate) const POLL_EVERY_MINUTES: i64 = 5;
/// The first pause after GitHub pushes back; each further push-back doubles it.
pub(crate) const BACKOFF_FIRST_MINUTES: i64 = 15;
/// The longest pause, and the longest an org's failing search waits between tries.
const BACKOFF_MAX_MINUTES: i64 = 240;
/// Latest-comment reads one poll may make; the rest wait for the next poll.
const MAX_COMMENT_READS: usize = 10;
/// How long an answered issue is remembered, so a body-marker issue does not come back.
const ANSWERED_KEEP_DAYS: i64 = 30;
/// Options parsed from one question, at most.
const MAX_OPTIONS: usize = 10;
/// Failed runs one "Re-run failed jobs" click re-runs, at most.
const MAX_RERUNS: usize = 5;
const FILE: &str = "decisions.json";
const GH_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

// ---------------------------------------------------------------------------------------------
// Reading a decision out of issue text.
// ---------------------------------------------------------------------------------------------

/// One question found in an issue's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub question: String,
    pub options: Vec<String>,
    /// Further "Open decision:" lines in the same text; the card asks the first.
    pub more: usize,
}

/// A line with its Markdown decoration (quote marks, headings, bullets, emphasis) taken off the front.
fn undecorated(line: &str) -> &str {
    line.trim()
        .trim_start_matches(['>', '#', '*', '_', '-', '+', ' ', '\t'])
        .trim_start()
}

/// The text after a decision marker, when the line is one.
fn marker_rest(line: &str) -> Option<&str> {
    let bare = undecorated(line);
    let lower = bare.to_ascii_lowercase();
    let marker = MARKERS.iter().find(|m| lower.starts_with(*m))?;
    Some(clean(&bare[marker.len()..]))
}

/// Emphasis markers and whitespace around a phrase taken off.
fn clean(text: &str) -> &str {
    text.trim().trim_matches(['*', '_', '`']).trim()
}

/// Whether the line opens the options list ("Options:", "**Options:**", "### Options").
fn is_options_line(line: &str) -> bool {
    let bare = clean(undecorated(line)).to_ascii_lowercase();
    let bare = bare.trim_end_matches(['*', '_', ':']).trim();
    bare == "options"
}

/// The text of a list item: `- a`, `* a`, `+ a`, `1. a`, `1) a`, a task box dropped.
fn bullet(line: &str) -> Option<String> {
    let t = line.trim();
    let rest = if let Some(rest) = t.strip_prefix(['-', '*', '+']) {
        rest
    } else {
        let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return None;
        }
        t[digits..].strip_prefix(['.', ')'])?
    };
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim();
    let rest = rest
        .strip_prefix("[ ]")
        .or_else(|| rest.strip_prefix("[x]"))
        .or_else(|| rest.strip_prefix("[X]"))
        .unwrap_or(rest);
    let text = clean(rest);
    (!text.is_empty()).then(|| truncate(text, 200))
}

/// The first decision the text asks for, with the options listed under it.
pub(crate) fn parse_decision(text: &str) -> Option<Parsed> {
    let lines: Vec<&str> = text.lines().collect();
    let markers: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| marker_rest(l).is_some())
        .map(|(i, _)| i)
        .collect();
    let first = *markers.first()?;
    let end = markers.get(1).copied().unwrap_or(lines.len());
    let mut question = marker_rest(lines[first]).unwrap_or_default().to_string();
    let mut i = first + 1;
    if question.is_empty() {
        // "Open decision:" alone on its line: the question is the next line of prose.
        while i < end {
            let line = lines[i].trim();
            i += 1;
            if line.is_empty() {
                continue;
            }
            if !is_options_line(line) && bullet(line).is_none() {
                question = clean(undecorated(line)).to_string();
            } else {
                i -= 1;
            }
            break;
        }
    }
    if question.is_empty() {
        return None;
    }
    let mut options: Vec<String> = Vec::new();
    if let Some(at) = (i..end).find(|&j| is_options_line(lines[j])) {
        for line in &lines[at + 1..end] {
            // A blank line before or inside the list is fine; the list ends at the first line
            // that is neither blank nor an item.
            if line.trim().is_empty() {
                continue;
            }
            match bullet(line) {
                Some(option) => {
                    if !options.contains(&option) && options.len() < MAX_OPTIONS {
                        options.push(option);
                    }
                }
                None => break,
            }
        }
    }
    Some(Parsed {
        question: truncate(&question, 300),
        options,
        more: markers.len() - 1,
    })
}

/// Whether a comment is a decision already given.
pub(crate) fn is_answer(text: &str) -> bool {
    text.trim_start()
        .get(..ANSWER_PREFIX.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(ANSWER_PREFIX))
}

/// The comment an answer posts: the prefix and the choice on one line, the note below it.
pub(crate) fn answer_body(choice: &str, note: Option<&str>) -> String {
    let choice = choice.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut body = format!("{ANSWER_PREFIX} {choice}");
    if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
        body.push_str("\n\n");
        body.push_str(note);
    }
    body
}

// ---------------------------------------------------------------------------------------------
// Cards.
// ---------------------------------------------------------------------------------------------

/// One issue waiting on a decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DecisionCard {
    /// `owner/repo#n`.
    pub id: String,
    pub org: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub question: String,
    /// Empty: the card offers free text only.
    pub options: Vec<String>,
    /// Where it was found: `label`, `body` or `comment`.
    pub source: &'static str,
    /// The issue carries `needs-decision`, which an answer removes.
    pub labelled: bool,
    pub more: usize,
    pub updated_at: Option<String>,
}

/// Why a pull request needs a person, in the order the inbox lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PrReason {
    /// Issue #1075: merged, but its branch had commits after the merged head.
    CommitsNotMerged,
    PolicyHold,
    NeedsRedo,
    Conflicted,
    RedCi,
    ReviewRequested,
    AwaitingMerge,
}

/// One pull request (or, for a policy hold before publishing, one colony) that needs a person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PrCard {
    /// The pull request's URL, or `colony:<id>` for a colony with none yet.
    pub id: String,
    pub org: String,
    pub repo: String,
    pub number: Option<u64>,
    pub title: String,
    pub url: Option<String>,
    /// The colony behind it, when one is.
    pub colony: Option<String>,
    pub reason: PrReason,
    /// The reason in a sentence.
    pub why: String,
    /// Quick actions: `rerun` (re-run failed jobs), `redo` (dispatch a redo colony), `dismiss`
    /// (forget a commits-not-merged card once a person has dealt with it).
    pub actions: Vec<&'static str>,
}

/// `owner/repo` and the number of a search item.
fn item_repo_number(item: &Value) -> Option<(String, u64)> {
    let repo = item["repository_url"].as_str()?.split("/repos/").nth(1)?.to_string();
    let number = item["number"].as_u64()?;
    valid_repo(&repo).then_some((repo, number))
}

fn owner(repo: &str) -> String {
    repo.split('/').next().unwrap_or_default().to_ascii_lowercase()
}

fn pr_number(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

/// The decision card for a search item, if it is waiting on one. A label counts — unless the
/// operator answered here and the latest comment is that answer, which is a label that would not
/// come off, and a second card would post a second answer; a marker in the latest comment counts;
/// a marker in the body counts unless the latest comment is an answer or the operator answered it
/// here already.
pub(crate) fn decision_from_item(item: &Value, latest_comment: Option<&str>, answered_here: bool) -> Option<DecisionCard> {
    let (repo, number) = item_repo_number(item)?;
    let labelled = item["labels"].as_array().is_some_and(|ls| {
        ls.iter()
            .any(|l| l["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(LABEL)))
    });
    let title = item["title"].as_str().unwrap_or_default().to_string();
    let body = parse_decision(item["body"].as_str().unwrap_or_default());
    let comment = latest_comment.and_then(parse_decision);
    let comment_answered = latest_comment.is_some_and(is_answer);
    let (parsed, source) = match (comment, body) {
        (Some(p), _) => (Some(p), "comment"),
        (None, body) if labelled && !(comment_answered && answered_here) => (body, "label"),
        (None, Some(p)) if !comment_answered && !answered_here => (Some(p), "body"),
        _ => return None,
    };
    let (question, options, more) = match parsed {
        Some(p) => (p.question, p.options, p.more),
        None => (title.clone(), Vec::new(), 0),
    };
    Some(DecisionCard {
        id: format!("{repo}#{number}"),
        org: owner(&repo),
        url: item["html_url"].as_str().unwrap_or_default().to_string(),
        repo,
        number,
        title,
        question,
        options,
        source,
        labelled,
        more,
        updated_at: item["updated_at"].as_str().map(str::to_string),
    })
}

/// The card for a pull request the search found with a review requested from the operator.
fn review_card(item: &Value) -> Option<PrCard> {
    let (repo, number) = item_repo_number(item)?;
    let url = item["pull_request"]["html_url"]
        .as_str()
        .or_else(|| item["html_url"].as_str())?
        .to_string();
    let author = item["user"]["login"].as_str().unwrap_or("someone");
    Some(PrCard {
        id: url.clone(),
        org: owner(&repo),
        repo,
        number: Some(number),
        title: item["title"].as_str().unwrap_or_default().to_string(),
        url: Some(url),
        colony: None,
        reason: PrReason::ReviewRequested,
        why: format!("{author} asked you for a review"),
        actions: Vec::new(),
    })
}

/// The attention reason a colony carries, if any.
fn attention_reason(s: &Session) -> Option<&str> {
    s.attention.as_ref().and_then(|a| a["reason"].as_str())
}

/// Everything the pull-request cards are read from besides the colonies.
pub(crate) struct PrInputs<'a> {
    pub loop_state: &'a LoopState,
    /// Repositories the merge train or the merge-train loop merges in on its own.
    pub driven: &'a BTreeSet<String>,
    /// Colonies whose autopilot hold is a secret redacted from the pull request description.
    pub secret_held: &'a HashSet<String>,
    /// Lower-case orgs the operator opted in.
    pub orgs: &'a BTreeSet<String>,
}

/// The policy hold on a colony, in a sentence, if it has one.
fn policy_hold(s: &Session, secret_held: &HashSet<String>) -> Option<String> {
    if attention_reason(s) == Some(crate::watchdog::CONTROL_DEFEAT_REASON) {
        let detail = s
            .attention
            .as_ref()
            .and_then(|a| a["detail"].as_str())
            .unwrap_or("repeated attempts at a refused target");
        return Some(format!(
            "the watchdog flagged a control-defeat signature: {}",
            truncate(detail, 200)
        ));
    }
    if attention_reason(s) == Some("autopilot_held") && secret_held.contains(&s.id) {
        return Some(
            "a secret was redacted from its pull request description; the publish waits until someone has looked".into(),
        );
    }
    let error = s.error.as_deref().unwrap_or_default();
    if error.to_ascii_lowercase().starts_with("refusing to") {
        return Some(format!("the publish was refused: {}", truncate(error, 200)));
    }
    None
}

/// The pull-request cards read from the colonies, plus the review requests the search found, one
/// card per pull request (the first reason in [`PrReason`] order wins), in that order.
pub(crate) fn pr_cards(sessions: &[Session], inputs: &PrInputs, reviews: &[PrCard]) -> Vec<PrCard> {
    let report = inputs.loop_state.history.last();
    let report_item = |url: &str| report.and_then(|r| r.repos.iter().flat_map(|x| &x.items).find(|i| i.pr_url == url));
    let mut cards: Vec<PrCard> = Vec::new();
    // Issue #1075: a merged pull request whose branch got commits after the merged head. Its colony
    // is merged by now, so these come from the train's record, not from the colonies.
    for u in inputs.loop_state.commits_not_merged.values() {
        let org = owner(&u.repo);
        if !inputs.orgs.contains(&org) {
            continue;
        }
        cards.push(PrCard {
            id: u.pr_url.clone(),
            org,
            repo: u.repo.clone(),
            number: pr_number(&u.pr_url),
            title: if u.title.is_empty() { u.repo.clone() } else { u.title.clone() },
            url: Some(u.pr_url.clone()),
            colony: u.colony.clone(),
            reason: PrReason::CommitsNotMerged,
            why: format!(
                "{}; its branch is kept, so open a follow-up pull request from it",
                u.sentence()
            ),
            actions: vec!["dismiss"],
        });
    }
    for s in sessions {
        if s.cleaned_up || s.superseded.is_some() || !inputs.orgs.contains(&s.org.to_ascii_lowercase()) {
            continue;
        }
        if matches!(s.status, SessionStatus::Merged | SessionStatus::Closed) {
            continue;
        }
        let url = s.pr_url.clone().filter(|u| !u.is_empty());
        let card = |reason: PrReason, why: String, actions: Vec<&'static str>| PrCard {
            id: url.clone().unwrap_or_else(|| format!("colony:{}", s.id)),
            org: s.org.to_ascii_lowercase(),
            repo: s.repo.clone(),
            number: url.as_deref().and_then(pr_number),
            title: if s.issue_title.is_empty() {
                s.repo.clone()
            } else {
                s.issue_title.clone()
            },
            url: url.clone(),
            colony: Some(s.id.clone()),
            reason,
            why,
            actions,
        };
        if let Some(why) = policy_hold(s, inputs.secret_held) {
            cards.push(card(PrReason::PolicyHold, why, Vec::new()));
            continue;
        }
        let Some(pr) = url.as_deref() else { continue };
        if s.status != SessionStatus::PrOpened {
            continue;
        }
        let memory = inputs.loop_state.repos.get(&s.repo);
        let redone = memory.is_some_and(|m| m.redo_dispatched.contains(pr));
        if let Some(why) = memory.and_then(|m| m.needs_redo.get(pr)) {
            if !redone {
                cards.push(card(
                    PrReason::NeedsRedo,
                    format!(
                        "the merge train's mechanical rebase conflicted ({}); it needs a redo or a hand",
                        truncate(why, 200)
                    ),
                    vec!["redo"],
                ));
            }
            continue;
        }
        if let Some(why) = memory.and_then(|m| m.resolving.get(pr)).and_then(|r| r.gave_up.as_deref()) {
            if !redone {
                cards.push(card(
                    PrReason::NeedsRedo,
                    format!("the resolve colony gave up on its conflicts: {}", truncate(why, 200)),
                    vec!["redo"],
                ));
            }
            continue;
        }
        if s.needs_rebase {
            if !redone {
                cards.push(card(
                    PrReason::Conflicted,
                    "it is behind or conflicts with its base, and the auto-rebase could not finish".into(),
                    vec!["redo"],
                ));
            }
            continue;
        }
        match s.ci_state {
            Some(github::CiState::Failure) => {
                let item = report_item(pr);
                // A known flake the merge loop is re-running is the loop's, not yours.
                if item.is_some_and(|i| i.action == Action::Rerun) {
                    continue;
                }
                let why = match item.filter(|i| i.action == Action::Red) {
                    Some(i) => format!("CI is red and not a known flake: {}", truncate(&i.reason, 200)),
                    None => "CI is red and not a known flake".into(),
                };
                cards.push(card(PrReason::RedCi, why, vec!["rerun"]));
            }
            Some(github::CiState::Success) if !inputs.driven.contains(&s.repo.to_ascii_lowercase()) => {
                cards.push(card(
                    PrReason::AwaitingMerge,
                    format!(
                        "CI is green and nothing merges in {} on its own: it waits for a person to merge it",
                        s.repo
                    ),
                    Vec::new(),
                ));
            }
            _ => {}
        }
    }
    cards.extend(reviews.iter().filter(|c| inputs.orgs.contains(&c.org)).cloned());
    cards.sort_by_key(|c| c.reason);
    let mut seen = HashSet::new();
    cards.retain(|c| seen.insert(c.id.clone()));
    cards
}

// ---------------------------------------------------------------------------------------------
// Per-org opt-in and what the operator answered.
// ---------------------------------------------------------------------------------------------

/// `<config_dir>/decisions.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Stored {
    /// Lower-case org → on/off, set by the operator. An org it does not name follows the default.
    pub orgs: BTreeMap<String, bool>,
    /// `owner/repo#n` → when the operator answered it here.
    pub answered: BTreeMap<String, DateTime<Utc>>,
}

fn file(app: &Shared) -> PathBuf {
    app.cfg.config_dir.join(FILE)
}

fn load(app: &Shared) -> Stored {
    crate::util::read_json_or_default(&file(app)).unwrap_or_default()
}

async fn save(app: &Shared, stored: &Stored) -> anyhow::Result<()> {
    std::fs::create_dir_all(&app.cfg.config_dir)?;
    crate::util::write_atomic(&file(app), &serde_json::to_vec_pretty(stored)?).await
}

/// One org as the inbox's settings show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct OrgOptIn {
    pub org: String,
    pub enabled: bool,
    /// What it would be without the operator's own choice.
    pub default_on: bool,
    /// The operator chose `enabled` themselves.
    pub explicit: bool,
}

/// Every org the mothership knows, each with its opt-in: the operator's own choice when they made
/// one, else on only for an org that is switched on as a workspace and already has colonies.
pub(crate) fn opt_ins(
    stored: &Stored,
    known: &BTreeSet<String>,
    with_colonies: &BTreeSet<String>,
    switched_off: &BTreeSet<String>,
) -> Vec<OrgOptIn> {
    let all: BTreeSet<String> = known
        .iter()
        .chain(with_colonies)
        .chain(stored.orgs.keys())
        .map(|o| o.to_ascii_lowercase())
        .filter(|o| !o.is_empty())
        .collect();
    all.into_iter()
        .map(|org| {
            let default_on = with_colonies.contains(&org) && !switched_off.contains(&org);
            let explicit = stored.orgs.get(&org).copied();
            OrgOptIn {
                enabled: explicit.unwrap_or(default_on),
                default_on,
                explicit: explicit.is_some(),
                org,
            }
        })
        .collect()
}

async fn org_opt_ins(app: &Shared, stored: &Stored) -> Vec<OrgOptIn> {
    let settings = app.all_org_settings();
    let mut known: BTreeSet<String> = settings.keys().map(|o| o.to_ascii_lowercase()).collect();
    known.extend(app.known_orgs().unwrap_or_default().keys().map(|o| o.to_ascii_lowercase()));
    let switched_off: BTreeSet<String> = settings
        .iter()
        .filter(|(_, s)| !crate::orgs::org_enabled(s))
        .map(|(o, _)| o.to_ascii_lowercase())
        .collect();
    let with_colonies: BTreeSet<String> = app.sessions.read().await.iter().map(|s| s.org.to_ascii_lowercase()).collect();
    opt_ins(stored, &known, &with_colonies, &switched_off)
}

// ---------------------------------------------------------------------------------------------
// The poll's pace: one org per tick, each at most every five minutes, and a pause on push-back.
// ---------------------------------------------------------------------------------------------

/// When each org was last searched and how GitHub has been answering. Pure: the clock is passed in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Gate {
    last: BTreeMap<String, DateTime<Utc>>,
    /// Failed searches in a row, per org: each doubles that org's wait.
    failures: BTreeMap<String, u32>,
    /// Push-backs in a row (403/429, abuse, secondary rate limit), across every org.
    strikes: u32,
    paused_until: Option<DateTime<Utc>>,
}

pub(crate) fn doubled(first_minutes: i64, times: u32) -> Duration {
    let minutes = first_minutes.saturating_mul(1i64 << times.min(16));
    Duration::minutes(minutes.min(BACKOFF_MAX_MINUTES))
}

impl Gate {
    pub fn paused_until(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.paused_until.filter(|p| *p > now)
    }

    /// How long an org waits after its last search.
    fn interval(&self, org: &str) -> Duration {
        match self.failures.get(org).copied().unwrap_or(0) {
            0 => Duration::minutes(POLL_EVERY_MINUTES),
            n => doubled(POLL_EVERY_MINUTES, n),
        }
    }

    pub fn due(&self, org: &str, now: DateTime<Utc>) -> bool {
        self.paused_until(now).is_none() && self.last.get(org).is_none_or(|at| now - *at >= self.interval(org))
    }

    /// The org to search now: of those due, the one searched longest ago (never searched first).
    pub fn next(&self, orgs: &[String], now: DateTime<Utc>) -> Option<String> {
        orgs.iter()
            .filter(|o| self.due(o, now))
            .min_by_key(|o| self.last.get(*o).copied())
            .cloned()
    }

    pub fn succeeded(&mut self, org: &str, now: DateTime<Utc>) {
        self.last.insert(org.to_string(), now);
        self.failures.remove(org);
        self.strikes = 0;
    }

    pub fn failed(&mut self, org: &str, now: DateTime<Utc>) {
        self.last.insert(org.to_string(), now);
        *self.failures.entry(org.to_string()).or_default() += 1;
    }

    /// GitHub pushed back: every org pauses, longer each time in a row. Returns when it resumes.
    pub fn throttled(&mut self, org: &str, now: DateTime<Utc>) -> DateTime<Utc> {
        self.last.insert(org.to_string(), now);
        self.strikes += 1;
        let until = now + doubled(BACKOFF_FIRST_MINUTES, self.strikes - 1);
        self.paused_until = Some(until);
        until
    }
}

/// What the last search of one org found.
#[derive(Clone, Debug, Default)]
pub(crate) struct OrgData {
    pub decisions: Vec<DecisionCard>,
    pub reviews: Vec<PrCard>,
    /// `owner/repo#n` → (the issue's `updated_at` when read, its latest comment): an issue that has
    /// not changed is not read again.
    pub comments: HashMap<String, (String, Option<String>)>,
    pub polled_at: Option<DateTime<Utc>>,
}

#[derive(Default)]
struct Cache {
    gate: Gate,
    orgs: BTreeMap<String, OrgData>,
    errors: BTreeMap<String, String>,
}

/// The search answers, per mothership (its config directory), so tests with their own App never share one.
static CACHE: LazyLock<Mutex<HashMap<PathBuf, Cache>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Decisions being answered right now: a second click on the same card waits its turn and is refused.
static ANSWERING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn with_cache<R>(app: &Shared, f: impl FnOnce(&mut Cache) -> R) -> R {
    let mut all = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(all.entry(app.cfg.config_dir.clone()).or_default())
}

// ---------------------------------------------------------------------------------------------
// The GitHub seam.
// ---------------------------------------------------------------------------------------------

/// What the inbox asks of GitHub, so the tests stand in for it. Module private, like the merge
/// loop's `Ops`, so its futures stay concrete.
trait Gh {
    async fn search(&self, query: &str) -> Result<Value, String>;
    /// The body of the issue's `count`-th (latest) comment.
    async fn latest_comment(&self, repo: &str, number: u64, count: u64) -> Result<Option<String>, String>;
    async fn comment(&self, repo: &str, number: u64, body: &str) -> Result<(), String>;
    async fn remove_label(&self, repo: &str, number: u64, label: &str) -> Result<(), String>;
    /// A breath between two reads in one poll.
    async fn pause(&self);
}

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The one search per org: open issues with the label or a marker, and open pull requests waiting
/// on the operator's review.
pub(crate) fn search_query(org: &str) -> String {
    format!(
        "org:{org} is:open ((is:issue AND (label:{LABEL} OR \"Open decision\" OR \"Decision needed\")) OR (is:pr AND review-requested:@me))"
    )
}

struct RealGh<'a> {
    app: &'a Shared,
}

impl RealGh<'_> {
    async fn get_json(&self, path: &str) -> Result<Value, String> {
        match github::gh_get(self.app, path, None).await {
            Ok((status, body)) if status >= 400 => Err(format!("HTTP {status}: {}", truncate(body.trim(), 300))),
            Ok((_, body)) if body.trim().is_empty() => Ok(Value::Null),
            Ok((_, body)) => serde_json::from_str(&body).map_err(|e| format!("{e}")),
            Err(e) => Err(format!("{e:#}")),
        }
    }

    async fn gh(&self, args: Vec<String>) -> Result<String, String> {
        crate::util::exec_within(GH_LIMIT, &mut self.app.gh(args))
            .await
            .map_err(|e| format!("{e:#}"))
    }
}

impl Gh for RealGh<'_> {
    async fn search(&self, query: &str) -> Result<Value, String> {
        self.get_json(&format!(
            "search/issues?q={}&advanced_search=true&per_page=50&sort=updated&order=desc",
            encode(query)
        ))
        .await
    }

    async fn latest_comment(&self, repo: &str, number: u64, count: u64) -> Result<Option<String>, String> {
        let page = self
            .get_json(&format!("repos/{repo}/issues/{number}/comments?per_page=1&page={count}"))
            .await?;
        Ok(page[0]["body"].as_str().map(str::to_string))
    }

    async fn comment(&self, repo: &str, number: u64, body: &str) -> Result<(), String> {
        self.gh(vec![
            "api".into(),
            "-X".into(),
            "POST".into(),
            format!("repos/{repo}/issues/{number}/comments"),
            "-f".into(),
            format!("body={body}"),
        ])
        .await
        .map(|_| ())
    }

    async fn remove_label(&self, repo: &str, number: u64, label: &str) -> Result<(), String> {
        match self
            .gh(vec![
                "api".into(),
                "-X".into(),
                "DELETE".into(),
                format!("repos/{repo}/issues/{number}/labels/{}", encode(label)),
            ])
            .await
        {
            // Already gone is what we wanted.
            Err(e) if e.contains("404") || e.contains("Label does not exist") => Ok(()),
            other => other.map(|_| ()),
        }
    }

    async fn pause(&self) {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// One org's search, and the latest comments of the issues that changed since the last one.
async fn poll_org<G: Gh>(
    gh: &G,
    org: &str,
    previous: &OrgData,
    answered: &BTreeMap<String, DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<OrgData, String> {
    let found = gh.search(&search_query(org)).await?;
    let mut out = OrgData {
        polled_at: Some(now),
        ..OrgData::default()
    };
    let mut reads = 0;
    for item in found["items"].as_array().into_iter().flatten() {
        if item.get("pull_request").is_some_and(|p| !p.is_null()) {
            out.reviews.extend(review_card(item));
            continue;
        }
        let Some((repo, number)) = item_repo_number(item) else {
            continue;
        };
        let id = format!("{repo}#{number}");
        let updated = item["updated_at"].as_str().unwrap_or_default().to_string();
        let count = item["comments"].as_u64().unwrap_or(0);
        let mut latest = None;
        if count > 0 {
            match previous.comments.get(&id).filter(|(at, _)| *at == updated) {
                Some((_, body)) => {
                    latest = body.clone();
                    out.comments.insert(id.clone(), (updated.clone(), latest.clone()));
                }
                None if reads < MAX_COMMENT_READS => {
                    if reads > 0 {
                        gh.pause().await;
                    }
                    reads += 1;
                    latest = gh.latest_comment(&repo, number, count).await?;
                    out.comments.insert(id.clone(), (updated.clone(), latest.clone()));
                }
                None => {}
            }
        }
        out.decisions
            .extend(decision_from_item(item, latest.as_deref(), answered.contains_key(&id)));
    }
    Ok(out)
}

/// What an answer did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Answered {
    pub id: String,
    pub comment: String,
    pub label_removed: bool,
    /// The label could not be removed (the comment is posted either way).
    pub label_error: Option<String>,
}

/// Posts the answer: exactly one comment, then the label off when the issue carries it.
async fn answer_with<G: Gh>(gh: &G, card: &DecisionCard, choice: &str, note: Option<&str>) -> Result<Answered, String> {
    if authority::external_writes_blocked() {
        return Err(crate::publish::BLOCKED.to_string());
    }
    let comment = answer_body(choice, note);
    gh.comment(&card.repo, card.number, &comment).await?;
    let (label_removed, label_error) = if card.labelled {
        match gh.remove_label(&card.repo, card.number, LABEL).await {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e)),
        }
    } else {
        (false, None)
    };
    Ok(Answered {
        id: card.id.clone(),
        comment,
        label_removed,
        label_error,
    })
}

// ---------------------------------------------------------------------------------------------
// The background poll.
// ---------------------------------------------------------------------------------------------

/// One tick: search the org that is due, if any, and keep what it found.
async fn tick<G: Gh>(app: &Shared, gh: &G, now: DateTime<Utc>) {
    // Issue #1074: the inbox does not poll an account GitHub refuses; it picks up where it left off
    // once the breaker closes.
    if crate::github_breaker::paused(app).is_some() {
        return;
    }
    let stored = load(app);
    let enabled: Vec<String> = org_opt_ins(app, &stored)
        .await
        .into_iter()
        .filter(|o| o.enabled)
        .map(|o| o.org)
        .collect();
    let Some((org, previous)) = with_cache(app, |c| {
        let org = c.gate.next(&enabled, now)?;
        let previous = c.orgs.get(&org).cloned().unwrap_or_default();
        Some((org, previous))
    }) else {
        return;
    };
    let result = poll_org(gh, &org, &previous, &stored.answered, now).await;
    with_cache(app, |c| match result {
        Ok(data) => {
            c.gate.succeeded(&org, now);
            c.errors.remove(&org);
            c.orgs.insert(org, data);
        }
        Err(e) if merge_loop::is_throttle(&e) => {
            let until = c.gate.throttled(&org, now);
            eprintln!("decisions: GitHub pushed back searching {org} ({e}); pausing until {until}");
            c.errors.insert(
                org,
                format!("GitHub pushed back; paused until {until}: {}", truncate(&e, 200)),
            );
        }
        Err(e) => {
            c.gate.failed(&org, now);
            c.errors.insert(org, truncate(&e, 300));
        }
    });
}

fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(60));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            tick(&app, &RealGh { app: &app }, Utc::now()).await;
        }
    });
}

// ---------------------------------------------------------------------------------------------
// The view and the routes.
// ---------------------------------------------------------------------------------------------

/// The repositories among the colonies that the merge train or its loop merges in on its own.
async fn driven_repos(app: &Shared, sessions: &[Session], loop_state: &LoopState) -> BTreeSet<String> {
    let train = merge_train::train_settings(app).await;
    sessions
        .iter()
        .map(|s| s.repo.as_str())
        .filter(|repo| {
            merge_train::effective_state(&train, repo) == merge_train::TrainState::On
                || merge_loop::drives(&loop_state.settings, repo)
        })
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Every card, as the inbox shows them.
pub(crate) struct Cards {
    pub decisions: Vec<DecisionCard>,
    pub prs: Vec<PrCard>,
    pub orgs: Vec<OrgOptIn>,
}

async fn cards(app: &Shared) -> Cards {
    let stored = load(app);
    let orgs = org_opt_ins(app, &stored).await;
    let enabled: BTreeSet<String> = orgs.iter().filter(|o| o.enabled).map(|o| o.org.clone()).collect();
    let (mut decisions, reviews) = with_cache(app, |c| {
        let mut decisions = Vec::new();
        let mut reviews = Vec::new();
        for (org, data) in &c.orgs {
            if enabled.contains(org) {
                decisions.extend(data.decisions.iter().cloned());
                reviews.extend(data.reviews.iter().cloned());
            }
        }
        (decisions, reviews)
    });
    // An answer given here hides a body-marker card at once, before the next search sees the comment.
    decisions.retain(|d| d.labelled || d.source == "comment" || !stored.answered.contains_key(&d.id));
    decisions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.id.cmp(&b.id)));
    let sessions = app.sessions.read().await.clone();
    let loop_state = merge_loop::load(&app.cfg.config_dir).await;
    let driven = driven_repos(app, &sessions, &loop_state).await;
    let secret_held: HashSet<String> = sessions
        .iter()
        .filter(|s| attention_reason(s) == Some("autopilot_held"))
        .filter(|s| github::pr_description_secret_note(&app.session_dir(&s.id).join("out"), s).is_some())
        .map(|s| s.id.clone())
        .collect();
    let prs = pr_cards(
        &sessions,
        &PrInputs {
            loop_state: &loop_state,
            driven: &driven,
            secret_held: &secret_held,
            orgs: &enabled,
        },
        &reviews,
    );
    Cards { decisions, prs, orgs }
}

async fn view(app: &Shared) -> Value {
    let Cards { decisions, prs, orgs } = cards(app).await;
    let now = Utc::now();
    let (paused_until, polled, errors) = with_cache(app, |c| {
        let polled: BTreeMap<String, Option<DateTime<Utc>>> = c.orgs.iter().map(|(o, d)| (o.clone(), d.polled_at)).collect();
        (c.gate.paused_until(now), polled, c.errors.clone())
    });
    let orgs: Vec<Value> = orgs
        .iter()
        .map(|o| {
            json!({
                "org": o.org,
                "enabled": o.enabled,
                "default_on": o.default_on,
                "explicit": o.explicit,
                "polled_at": polled.get(&o.org).copied().flatten(),
                "error": errors.get(&o.org),
            })
        })
        .collect();
    let writes_blocked = authority::external_writes_blocked();
    json!({
        "count": decisions.len() + prs.len(),
        "decisions": decisions,
        "prs": prs,
        "orgs": orgs,
        "writes_blocked": writes_blocked,
        "writes_blocked_reason": writes_blocked.then_some(
            "COLONIZER_NO_EXTERNAL_EFFECTS is set: the inbox is read-only, so answers and actions are off",
        ),
        "paused_until": paused_until,
        "poll_minutes": POLL_EVERY_MINUTES,
    })
}

/// `GET /api/decisions`: every decision and pull-request card, the org opt-ins, and the poll's state.
async fn get_decisions(State(app): State<Shared>) -> Json<Value> {
    Json(view(&app).await)
}

#[derive(Deserialize)]
struct OrgBody {
    /// `null` returns the org to its default.
    enabled: Option<bool>,
}

/// `PUT /api/decisions/orgs/{org}`: opts an org in or out, or (`null`) back to its default.
async fn put_org(State(app): State<Shared>, Path(org): Path<String>, Json(body): Json<OrgBody>) -> ApiResult<Value> {
    if !crate::orgs::valid_org(&org) {
        return Err(client_error(StatusCode::BAD_REQUEST, "not a GitHub org name"));
    }
    {
        let _config = app.config_write.lock().await;
        let mut stored = load(&app);
        let org = org.to_ascii_lowercase();
        match body.enabled {
            Some(on) => stored.orgs.insert(org, on),
            None => stored.orgs.remove(&org),
        };
        save(&app, &stored).await?;
    }
    Ok(Json(view(&app).await))
}

#[derive(Deserialize)]
struct AnswerBody {
    /// `owner/repo#n`, as the card names it.
    id: String,
    choice: String,
    #[serde(default)]
    note: Option<String>,
}

fn conflict(message: &str) -> crate::AppError {
    client_error(StatusCode::CONFLICT, message)
}

/// The answer's checks: a choice on one short line, a note of reasonable length.
fn check_answer(choice: &str, note: Option<&str>) -> Result<(), String> {
    let choice = choice.trim();
    if choice.is_empty() {
        return Err("pick an option or write an answer".into());
    }
    if choice.chars().count() > 500 {
        return Err("the answer is longer than 500 characters; put the detail in the note".into());
    }
    if note.is_some_and(|n| n.chars().count() > 4000) {
        return Err("the note is longer than 4000 characters".into());
    }
    Ok(())
}

/// `POST /api/decisions/answer`: posts the operator's decision on an open card.
async fn answer(State(app): State<Shared>, Json(body): Json<AnswerBody>) -> ApiResult<Value> {
    answer_on(&app, &RealGh { app: &app }, body).await.map(Json)
}

async fn answer_on<G: Gh>(app: &Shared, gh: &G, body: AnswerBody) -> Result<Value, crate::AppError> {
    if authority::external_writes_blocked() {
        return Err(conflict(
            "COLONIZER_NO_EXTERNAL_EFFECTS is set: the inbox is read-only, so nothing is posted to GitHub",
        ));
    }
    check_answer(&body.choice, body.note.as_deref()).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?;
    let Some(card) = cards(app).await.decisions.into_iter().find(|d| d.id == body.id) else {
        return Err(client_error(StatusCode::NOT_FOUND, "no open decision with that id"));
    };
    if !ANSWERING.lock().unwrap_or_else(|e| e.into_inner()).insert(card.id.clone()) {
        return Err(conflict("this decision is being answered already"));
    }
    let result = answer_with(gh, &card, &body.choice, body.note.as_deref()).await;
    ANSWERING.lock().unwrap_or_else(|e| e.into_inner()).remove(&card.id);
    let answered = result.map_err(|e| client_error(StatusCode::BAD_GATEWAY, &format!("GitHub refused the comment: {e}")))?;
    // Posted: drop the card now and remember it, so it does not come back before the next search.
    with_cache(app, |c| {
        for data in c.orgs.values_mut() {
            data.decisions.retain(|d| d.id != card.id);
        }
    });
    {
        let _config = app.config_write.lock().await;
        let mut stored = load(app);
        let now = Utc::now();
        stored.answered.insert(card.id.clone(), now);
        stored.answered.retain(|_, at| now - *at < Duration::days(ANSWERED_KEEP_DAYS));
        if let Err(e) = save(app, &stored).await {
            eprintln!("decisions: could not record the answer to {}: {e:#}", card.id);
        }
    }
    Ok(json!(answered))
}

#[derive(Deserialize)]
struct PrActionBody {
    /// The card's id.
    id: String,
    /// `rerun`, `redo` or `dismiss`.
    action: String,
}

/// `POST /api/decisions/pr-action`: a card's quick action — re-run a red pull request's failed
/// jobs, or dispatch a redo colony for a conflicted one through the merge loop's redo path.
async fn pr_action(State(app): State<Shared>, Json(body): Json<PrActionBody>) -> ApiResult<Value> {
    if authority::external_writes_blocked() {
        return Err(conflict(
            "COLONIZER_NO_EXTERNAL_EFFECTS is set: the inbox is read-only, so nothing is re-run or dispatched",
        ));
    }
    let Some(card) = cards(&app).await.prs.into_iter().find(|c| c.id == body.id) else {
        return Err(client_error(StatusCode::NOT_FOUND, "no open pull-request card with that id"));
    };
    if !card.actions.contains(&body.action.as_str()) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("\"{}\" is not an action this card offers", body.action),
        ));
    }
    match body.action.as_str() {
        "rerun" => rerun_failed(&app, &card).await.map(|runs| Json(json!({"rerun": runs}))),
        "dismiss" => {
            let id = card.id.clone();
            merge_loop::update(&app.cfg.config_dir, move |s| s.commits_not_merged.remove(&id))
                .await
                .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
            Ok(Json(json!({"dismissed": card.id})))
        }
        _ => redo(&app, &card).await.map(|colony| Json(json!({"colony": colony}))),
    }
}

/// Re-runs the failed jobs of the Actions runs behind the pull request's failing checks.
async fn rerun_failed(app: &Shared, card: &PrCard) -> Result<Vec<u64>, crate::AppError> {
    let url = card.url.clone().unwrap_or_default();
    let gh = RealGh { app };
    let out = gh
        .gh(vec![
            "pr".into(),
            "view".into(),
            url,
            "--json".into(),
            "statusCheckRollup".into(),
        ])
        .await
        .map_err(|e| client_error(StatusCode::BAD_GATEWAY, &e))?;
    let rollup: Value = serde_json::from_str(&out).map_err(|e| client_error(StatusCode::BAD_GATEWAY, &format!("{e}")))?;
    let mut runs: Vec<u64> = merge_loop::failing_checks_from(&rollup["statusCheckRollup"])
        .into_iter()
        .filter_map(|c| c.run_id)
        .collect();
    runs.sort_unstable();
    runs.dedup();
    runs.truncate(MAX_RERUNS);
    if runs.is_empty() {
        return Err(conflict(
            "no failed GitHub Actions run to re-run: the failing check is an external one, or it already re-ran",
        ));
    }
    for run in &runs {
        gh.gh(vec![
            "run".into(),
            "rerun".into(),
            run.to_string(),
            "--failed".into(),
            "-R".into(),
            card.repo.clone(),
        ])
        .await
        .map_err(|e| client_error(StatusCode::BAD_GATEWAY, &e))?;
    }
    if let Some(colony) = &card.colony {
        app.session_log(
            colony,
            "info",
            format!("decisions inbox: re-ran the failed jobs of run(s) {runs:?}"),
        )
        .await;
    }
    Ok(runs)
}

/// Dispatches the redo colony the merge loop would, once per pull request, through the merge loop's
/// own launch body and `sessions::create` — the door every launch takes, where the duplicates
/// service (`duplicates.rs`) answers. Like the loop's redo it carries `allow_duplicate`, since the
/// colony it redoes still holds the issue; what stops a second redo is the loop's own rule, checked
/// first: refused when a redo colony (or a newer pull request on the same issue) already exists, or
/// one was dispatched for this pull request before.
async fn redo(app: &Shared, card: &PrCard) -> Result<String, crate::AppError> {
    let Some(old) = app.session(card.colony.as_deref().unwrap_or_default()).await else {
        return Err(client_error(
            StatusCode::NOT_FOUND,
            "the colony behind this pull request is gone",
        ));
    };
    let pr_url = old.pr_url.clone().unwrap_or_default();
    let sessions = app.sessions.read().await.clone();
    if let Some(by) = merge_loop::superseded_by(&sessions, &old) {
        return Err(conflict(&format!("colony {by} already redoes or replaces this pull request")));
    }
    let dir = app.cfg.config_dir.clone();
    let state = merge_loop::load(&dir).await;
    if state
        .repos
        .get(&old.repo)
        .is_some_and(|m| m.redo_dispatched.contains(&pr_url))
    {
        return Err(conflict("a redo colony was already dispatched for this pull request"));
    }
    let base = match old.base.clone() {
        Some(base) => base,
        None => github::default_branch(app, &old.repo)
            .await
            .map_err(|e| client_error(StatusCode::BAD_GATEWAY, &format!("{e:#}")))?,
    };
    let body = merge_loop::dispatch_body(&merge_loop::Dispatch::Redo {
        session: Box::new(old.clone()),
        pr_url: pr_url.clone(),
        base,
        why: card.why.clone(),
    });
    let req: NewSession =
        serde_json::from_value(body).map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")))?;
    let Json(new) = sessions::create(State(app.clone()), None, Json(req)).await?;
    let repo = old.repo.clone();
    let url = pr_url.clone();
    if let Err(e) = merge_loop::update(&dir, move |s| {
        let memory = s.repos.entry(repo).or_default();
        memory.redo_dispatched.insert(url);
    })
    .await
    {
        eprintln!("decisions: could not record the redo of {pr_url}: {e:#}");
    }
    app.session_log(
        &old.id,
        "warn",
        format!("decisions inbox: redo colony {} dispatched by the operator", new.id),
    )
    .await;
    Ok(new.id)
}

fn routes() -> axum::Router<Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/decisions", routing::get(get_decisions))
        .route("/api/decisions/orgs/{org}", routing::put(put_org))
        .route("/api/decisions/answer", routing::post(answer))
        .route("/api/decisions/pr-action", routing::post(pr_action))
}

/// This module's feature descriptor (`features.rs`). Every route is the owner's: the cards span
/// every opted-in org, which no org-scoped token may read, and the writes act on GitHub.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "decisions",
    routes,
    token_scope: None,
    activity: ACTIVITY,
    kinds: &["decision.answer", "decision.pr_action"],
    start_tasks: Some(start_tasks),
};

/// An answer and a quick action are a person's acts on GitHub, so both are recorded; the opt-in is
/// a settings save.
const ACTIVITY: &[crate::activity::Rule] = &[
    crate::activity::rule(
        "POST",
        "/api/decisions/answer",
        "decision.answer",
        crate::activity::Target::None,
    ),
    crate::activity::rule(
        "POST",
        "/api/decisions/pr-action",
        "decision.pr_action",
        crate::activity::Target::None,
    ),
    crate::activity::rule(
        "PUT",
        "/api/decisions/orgs/{org}",
        "settings.save",
        crate::activity::Target::Fixed("decisions inbox", ""),
    ),
];

#[cfg(test)]
mod tests;
