//! Trusted auto-colonize (issue #1219): a new issue becomes a queued colony straight away, but only
//! when a person the org already trusts wrote it.
//!
//! An issue body is untrusted text that a colony turns into code and commands, so an issue, an edit
//! or a comment from outside the org is a prompt-injection route. The setting is
//! `auto_colonize: off | trusted`, per org and per repository (a repository's own choice wins), and
//! off by default. **Trusted** means the author is
//!
//! - an org member or owner (`GET /orgs/{org}/memberships/{login}`, active),
//! - a repository collaborator with write access or more (`GET /repos/{repo}/collaborators/{login}/permission`),
//! - an allowlisted login (`auto_colonize_allow`), or
//! - the signed-in account itself, which is what files an issue Colonizer filed.
//!
//! The check always goes to the GitHub API and is cached per `(repository, login)` for a short TTL;
//! the `author_association` GitHub puts on an event is never read. A lookup that fails is not cached
//! and never counts as trust: the issue waits in review.
//!
//! A stranger's issue never starts by itself. It lands in the list as **Needs review: external
//! author**, where a person can Colonize anyway (the ordinary launch) or Dismiss. Also held back:
//! an issue a stranger edited after it was filed (GitHub's edit history names every editor), an issue
//! labelled `no-colonize` (opt out) or `needs-human` (review), anything in a hidden org, and anything
//! over the repository's hourly rate cap. In a colony that started on auto, comments from strangers
//! are left out of the brief.
//!
//! Only issues filed **after** auto mode was turned on are taken: switching it on never colonizes
//! the backlog. Every trust decision is a line in the activity log.
//!
//! The list reads each issue's verdict as `intake: {mode, reason, label, author, trusted, trust,
//! dismissed}` (`github::list_issues`), the sweep (`start_tasks`) acts on the same verdicts, and
//! GitHub sits behind [`TrustSource`] so both share every step with the tests.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{LazyLock, Mutex as StdMutex},
    time::Duration,
};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    ApiResult, App, Shared,
    activity::{self, Entry},
    app::client_error,
    orgs::{self, OrgSettings},
    sessions::Session,
    util::{exec_within, valid_repo},
};

/// The `origin` of a colony the sweep started.
pub(crate) const ORIGIN: &str = "auto_colonize";
/// A label that opts an issue out of auto mode.
pub(crate) const LABEL_OPT_OUT: &str = "no-colonize";
/// A label that sends an issue to a person first.
pub(crate) const LABEL_NEEDS_HUMAN: &str = "needs-human";
/// How long a trust answer is believed.
pub(crate) const TRUST_TTL_SECS: i64 = 300;
/// New colonies per repository per hour when the org sets no cap.
pub(crate) const DEFAULT_RATE_PER_HOUR: u32 = 5;
/// The largest cap a setting may carry.
pub(crate) const MAX_RATE_PER_HOUR: u32 = 100;
const GH_LIMIT: Duration = Duration::from_secs(30);
const SWEEP_EVERY: Duration = Duration::from_secs(60);
const LIST_LIMIT: Duration = Duration::from_secs(25);
/// Issues asked about in one GraphQL call.
const EDIT_BATCH: usize = 40;
/// How long a decision or launch is remembered.
const REMEMBER_DAYS: i64 = 60;

// ---------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------

/// `auto_colonize`: whether new issues from trusted authors are colonized without a click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoColonize {
    #[default]
    Off,
    Trusted,
}

/// Where an effective mode came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Repo,
    Org,
    Default,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Repo => "repo",
            Source::Org => "org",
            Source::Default => "default",
        }
    }
}

fn org_entry<'a>(all: &'a BTreeMap<String, OrgSettings>, org: &str) -> Option<&'a OrgSettings> {
    all.get(org)
        .or_else(|| all.iter().find(|(k, _)| k.eq_ignore_ascii_case(org)).map(|(_, v)| v))
}

fn repo_override(settings: &OrgSettings, repo: &str) -> Option<AutoColonize> {
    settings
        .auto_colonize_repos
        .iter()
        .find(|(k, _)| k.trim().eq_ignore_ascii_case(repo))
        .map(|(_, v)| *v)
}

/// The org this feature treats as hidden. Today that is a workspace switched off (`enabled: false`),
/// the only hiding there is; the per-org `hidden` switch of issue #1213 joins here when it lands.
pub(crate) fn org_hidden(settings: &OrgSettings) -> bool {
    !orgs::org_enabled(settings)
}

/// The mode `repo` runs in, and whose setting that is: the repository's own, else its org's, else off.
pub(crate) fn effective_mode(all: &BTreeMap<String, OrgSettings>, repo: &str) -> (AutoColonize, Source) {
    let owner = repo.split('/').next().unwrap_or_default();
    let Some(settings) = org_entry(all, owner) else {
        return (AutoColonize::Off, Source::Default);
    };
    if let Some(mode) = repo_override(settings, repo) {
        return (mode, Source::Repo);
    }
    match settings.auto_colonize {
        Some(mode) => (mode, Source::Org),
        None => (AutoColonize::Off, Source::Default),
    }
}

pub(crate) fn rate_cap(settings: Option<&OrgSettings>) -> u32 {
    settings
        .and_then(|s| s.auto_colonize_rate)
        .unwrap_or(DEFAULT_RATE_PER_HOUR)
        .min(MAX_RATE_PER_HOUR)
}

/// A GitHub login as a path segment: letters, digits, hyphens, and the `[bot]` suffix of an app.
fn valid_login(login: &str) -> bool {
    let base = login.strip_suffix("[bot]").unwrap_or(login);
    !base.is_empty() && base.len() <= 39 && base.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// What `orgs::validate` asks of the four settings.
pub(crate) fn validate(settings: &OrgSettings) -> Result<(), String> {
    if settings.auto_colonize_rate.is_some_and(|n| n > MAX_RATE_PER_HOUR) {
        return Err(format!(
            "auto_colonize_rate is at most {MAX_RATE_PER_HOUR} colonies per repository per hour; 0 pauses auto mode"
        ));
    }
    if let Some(bad) = settings.auto_colonize_allow.iter().find(|l| !valid_login(l.trim())) {
        return Err(format!("auto_colonize_allow entries are GitHub logins; {bad:?} is not one"));
    }
    if let Some(bad) = settings.auto_colonize_repos.keys().find(|r| !valid_repo(r.trim())) {
        return Err(format!(
            "auto_colonize_repos entries are repositories like acme/api; {bad:?} is not one"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Trust
// ---------------------------------------------------------------------------------------------

/// Why a login is (or is not) trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Trust {
    Member,
    Collaborator(String),
    Allowlisted,
    Own,
    Owner,
    Stranger,
}

impl Trust {
    pub(crate) fn trusted(&self) -> bool {
        !matches!(self, Trust::Stranger)
    }

    fn kind(&self) -> &'static str {
        match self {
            Trust::Member => "member",
            Trust::Collaborator(_) => "collaborator",
            Trust::Allowlisted => "allowlisted",
            Trust::Own => "own",
            Trust::Owner => "owner",
            Trust::Stranger => "stranger",
        }
    }

    fn why(&self) -> String {
        match self {
            Trust::Member => "org member".into(),
            Trust::Collaborator(p) => format!("collaborator with {p} access"),
            Trust::Allowlisted => "allowlisted login".into(),
            Trust::Own => "the signed-in account".into(),
            Trust::Owner => "the owner of the repository".into(),
            Trust::Stranger => "not an org member, collaborator or allowlisted login".into(),
        }
    }
}

/// What the trust check asks of GitHub. Errors are text and fail closed.
pub(crate) trait TrustSource {
    /// The login's active role in `org`, or `None` when it is not a member.
    async fn org_role(&self, org: &str, login: &str) -> Result<Option<String>, String>;
    /// The login's effective permission on `repo`: `admin`, `write`, `read` or `none`.
    async fn permission(&self, repo: &str, login: &str) -> Result<String, String>;
    /// The logins that edited each issue's body, by number.
    async fn editors(&self, repo: &str, numbers: &[u64]) -> Result<BTreeMap<u64, Vec<String>>, String>;
}

/// One remembered trust answer and when it was given.
type Answer = (DateTime<Utc>, Trust);

/// A short-lived memory of trust answers, per `(repository, login)`.
pub(crate) struct TrustCache {
    ttl: chrono::Duration,
    seen: StdMutex<HashMap<(String, String), Answer>>,
}

impl TrustCache {
    pub(crate) fn new(ttl_secs: i64) -> Self {
        Self {
            ttl: chrono::Duration::seconds(ttl_secs),
            seen: StdMutex::new(HashMap::new()),
        }
    }

    fn get(&self, repo: &str, login: &str, now: DateTime<Utc>) -> Option<Trust> {
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let (at, trust) = seen.get(&(repo.to_ascii_lowercase(), login.to_ascii_lowercase()))?;
        (now - *at < self.ttl).then(|| trust.clone())
    }

    fn put(&self, repo: &str, login: &str, now: DateTime<Utc>, trust: &Trust) {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        // Expired answers are swept as new ones arrive, so the map stays as big as the people active.
        seen.retain(|_, (at, _)| now - *at < self.ttl);
        seen.insert((repo.to_ascii_lowercase(), login.to_ascii_lowercase()), (now, trust.clone()));
    }
}

static CACHE: LazyLock<TrustCache> = LazyLock::new(|| TrustCache::new(TRUST_TTL_SECS));

/// What a trust check needs to know about where it is asked.
pub(crate) struct Ctx<'a> {
    pub repo: &'a str,
    pub allow: &'a [String],
    /// The signed-in account's login, when known.
    pub own: &'a str,
}

impl Ctx<'_> {
    fn owner(&self) -> &str {
        self.repo.split('/').next().unwrap_or_default()
    }
}

/// Whether `login` is trusted on `ctx.repo`, asking GitHub unless a fresh answer is cached.
pub(crate) async fn resolve_trust<S: TrustSource>(
    src: &S,
    cache: &TrustCache,
    ctx: &Ctx<'_>,
    login: &str,
    now: DateTime<Utc>,
) -> Result<Trust, String> {
    let login = login.trim();
    if !valid_login(login) {
        return Ok(Trust::Stranger);
    }
    if ctx.allow.iter().any(|a| a.trim().eq_ignore_ascii_case(login)) {
        return Ok(Trust::Allowlisted);
    }
    if !ctx.own.is_empty() && ctx.own.eq_ignore_ascii_case(login) {
        return Ok(Trust::Own);
    }
    if ctx.owner().eq_ignore_ascii_case(login) {
        return Ok(Trust::Owner);
    }
    // An app is never a member or a collaborator: only the allowlist (above) vouches for one.
    if login.ends_with("[bot]") {
        return Ok(Trust::Stranger);
    }
    if let Some(hit) = cache.get(ctx.repo, login, now) {
        return Ok(hit);
    }
    let member = src.org_role(ctx.owner(), login).await;
    if matches!(member, Ok(Some(_))) {
        cache.put(ctx.repo, login, now, &Trust::Member);
        return Ok(Trust::Member);
    }
    let permission = src.permission(ctx.repo, login).await;
    let trust = match (&permission, &member) {
        (Ok(p), _) if matches!(p.as_str(), "admin" | "write" | "maintain") => Trust::Collaborator(p.clone()),
        // Nothing vouched for it: a stranger only when GitHub answered both questions.
        (Ok(_), Ok(None)) => Trust::Stranger,
        (Ok(_), Ok(Some(_))) => Trust::Member,
        (Err(e), _) | (_, Err(e)) => return Err(e.clone()),
    };
    cache.put(ctx.repo, login, now, &trust);
    Ok(trust)
}

// ---------------------------------------------------------------------------------------------
// The verdict
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    TrustedAuthor,
    Started,
    RateCapped,
    AutoOff,
    HiddenOrg,
    OptedOut,
    NeedsHuman,
    Dismissed,
    ExternalAuthor,
    EditedByStranger,
    Unverified,
    PredatesAuto,
}

impl Reason {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Reason::TrustedAuthor => "trusted_author",
            Reason::Started => "started",
            Reason::RateCapped => "rate_capped",
            Reason::AutoOff => "auto_off",
            Reason::HiddenOrg => "hidden_org",
            Reason::OptedOut => "opted_out",
            Reason::NeedsHuman => "needs_human",
            Reason::Dismissed => "dismissed",
            Reason::ExternalAuthor => "external_author",
            Reason::EditedByStranger => "edited_by_stranger",
            Reason::Unverified => "unverified",
            Reason::PredatesAuto => "predates_auto",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Reason::TrustedAuthor => "Auto: trusted author",
            Reason::Started => "A colony already started",
            Reason::RateCapped => "Auto: waiting for the hourly cap",
            Reason::AutoOff => "Auto mode is off",
            Reason::HiddenOrg => "Hidden org: never auto-colonized",
            Reason::OptedOut => "Opted out: no-colonize",
            Reason::NeedsHuman => "Needs review: needs-human label",
            Reason::Dismissed => "Dismissed",
            Reason::ExternalAuthor => "Needs review: external author",
            Reason::EditedByStranger => "Needs review: edited by an outside author",
            Reason::Unverified => "Needs review: trust could not be checked",
            Reason::PredatesAuto => "Filed before auto mode was turned on",
        }
    }

    /// `auto` for an issue auto mode takes (or has taken), `review` for everything else.
    fn is_auto(self) -> bool {
        matches!(self, Reason::TrustedAuthor | Reason::Started | Reason::RateCapped)
    }
}

/// One issue's verdict.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Intake {
    pub reason: Reason,
    pub author: String,
    pub trust: Option<Trust>,
    /// The outside editor, when that is why it waits.
    pub editor: Option<String>,
}

impl Intake {
    pub(crate) fn auto(&self) -> bool {
        self.reason.is_auto()
    }

    /// Whether the sweep should start a colony for it now.
    fn launchable(&self) -> bool {
        self.reason == Reason::TrustedAuthor
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "mode": if self.auto() { "auto" } else { "review" },
            "reason": self.reason.code(),
            "label": self.reason.label(),
            "author": self.author,
            "trusted": self.trust.as_ref().is_some_and(Trust::trusted),
            "trust": self.trust.as_ref().map(Trust::kind),
            "editor": self.editor,
            "dismissed": self.reason == Reason::Dismissed,
        })
    }
}

/// One open issue as the verdict needs it.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub number: u64,
    pub author: String,
    pub labels: Vec<String>,
    pub created_at: Option<DateTime<Utc>>,
}

/// What a repository's verdicts depend on besides the issues.
pub(crate) struct Policy {
    pub mode: AutoColonize,
    pub hidden: bool,
    pub allow: Vec<String>,
    pub cap: u32,
    pub own: String,
    /// When auto mode was armed for this repository; issues filed earlier are not taken.
    pub armed: Option<DateTime<Utc>>,
    pub dismissed: BTreeSet<u64>,
    /// Issues a colony was already started on, by auto mode or otherwise.
    pub started: BTreeSet<u64>,
    /// How many auto colonies this repository started in the last hour.
    pub started_last_hour: u32,
}

fn has_label(labels: &[String], wanted: &str) -> bool {
    labels.iter().any(|l| l.trim().eq_ignore_ascii_case(wanted))
}

/// The verdict for every row, in order. Pure apart from the GitHub reads behind `src`.
pub(crate) async fn assess<S: TrustSource>(
    src: &S,
    cache: &TrustCache,
    repo: &str,
    policy: &Policy,
    rows: &[Row],
    now: DateTime<Utc>,
) -> Vec<Intake> {
    let ctx = Ctx {
        repo,
        allow: &policy.allow,
        own: &policy.own,
    };
    let mut out: Vec<Option<Intake>> = vec![None; rows.len()];
    let verdict = |row: &Row, reason: Reason, trust: Option<Trust>| Intake {
        reason,
        author: row.author.clone(),
        trust,
        editor: None,
    };
    // Authors are looked up once each. A hidden org is never asked about.
    let mut authors: BTreeMap<String, Result<Trust, String>> = BTreeMap::new();
    if !policy.hidden {
        for row in rows {
            let key = row.author.to_ascii_lowercase();
            if let std::collections::btree_map::Entry::Vacant(slot) = authors.entry(key) {
                slot.insert(resolve_trust(src, cache, &ctx, &row.author, now).await);
            }
        }
    }
    let mut candidates: Vec<usize> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let trust = authors.get(&row.author.to_ascii_lowercase()).cloned();
        let known = trust.as_ref().and_then(|t| t.as_ref().ok()).cloned();
        let reason = if policy.hidden {
            Some(Reason::HiddenOrg)
        } else if has_label(&row.labels, LABEL_OPT_OUT) {
            Some(Reason::OptedOut)
        } else if policy.dismissed.contains(&row.number) {
            Some(Reason::Dismissed)
        } else if has_label(&row.labels, LABEL_NEEDS_HUMAN) {
            Some(Reason::NeedsHuman)
        } else if policy.mode == AutoColonize::Off {
            Some(Reason::AutoOff)
        } else {
            match &trust {
                Some(Ok(t)) if t.trusted() => None,
                Some(Ok(_)) => Some(Reason::ExternalAuthor),
                _ => Some(Reason::Unverified),
            }
        };
        match reason {
            Some(reason) => out[i] = Some(verdict(row, reason, known)),
            None => candidates.push(i),
        }
    }
    // Edits: asked only about issues that would otherwise start or already started.
    let mut editors: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    let numbers: Vec<u64> = candidates.iter().map(|&i| rows[i].number).collect();
    for chunk in numbers.chunks(EDIT_BATCH) {
        // A history that cannot be read leaves its issues without editors, which reads as unverified.
        if let Ok(found) = src.editors(repo, chunk).await {
            editors.extend(found);
        }
    }
    let mut waiting: Vec<usize> = Vec::new();
    for &i in &candidates {
        let row = &rows[i];
        let trust = authors
            .get(&row.author.to_ascii_lowercase())
            .and_then(|t| t.as_ref().ok())
            .cloned();
        let Some(list) = editors.get(&row.number) else {
            out[i] = Some(verdict(row, Reason::Unverified, trust));
            continue;
        };
        let mut stranger: Option<String> = None;
        let mut unverified = false;
        for editor in list {
            if editor.eq_ignore_ascii_case(&row.author) {
                continue;
            }
            match resolve_trust(src, cache, &ctx, editor, now).await {
                Ok(t) if t.trusted() => {}
                Ok(_) => {
                    stranger = Some(editor.clone());
                    break;
                }
                Err(_) => unverified = true,
            }
        }
        if let Some(editor) = stranger {
            out[i] = Some(Intake {
                editor: Some(editor),
                ..verdict(row, Reason::EditedByStranger, trust)
            });
        } else if unverified {
            out[i] = Some(verdict(row, Reason::Unverified, trust));
        } else if policy.started.contains(&row.number) {
            out[i] = Some(verdict(row, Reason::Started, trust));
        } else if !row.created_at.zip(policy.armed).is_some_and(|(made, armed)| made > armed) {
            out[i] = Some(verdict(row, Reason::PredatesAuto, trust));
        } else {
            waiting.push(i);
        }
    }
    // The hourly cap: the oldest issues take the room that is left, the rest wait for the next hour.
    waiting.sort_by_key(|&i| (rows[i].created_at, rows[i].number));
    let room = policy.cap.saturating_sub(policy.started_last_hour) as usize;
    for (n, &i) in waiting.iter().enumerate() {
        let trust = authors
            .get(&rows[i].author.to_ascii_lowercase())
            .and_then(|t| t.as_ref().ok())
            .cloned();
        let reason = if n < room { Reason::TrustedAuthor } else { Reason::RateCapped };
        out[i] = Some(verdict(&rows[i], reason, trust));
    }
    out.into_iter()
        .zip(rows)
        .map(|(v, row)| v.unwrap_or_else(|| verdict(row, Reason::Unverified, None)))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The brief
// ---------------------------------------------------------------------------------------------

/// Whether a colony started on auto mode (its brief leaves strangers' comments out).
pub(crate) fn is_auto(s: &Session) -> bool {
    s.origin.as_deref() == Some(ORIGIN)
}

/// Keeps the comments of trusted authors (marked `"trusted": true`, which is what `build_prompt`
/// reads in an auto colony) and drops the rest, noting how many went as `omitted_comments`. Returns
/// how many were dropped.
pub(crate) async fn filter_comments<S: TrustSource>(
    src: &S,
    cache: &TrustCache,
    ctx: &Ctx<'_>,
    issue: &mut Value,
    now: DateTime<Utc>,
) -> usize {
    let Some(comments) = issue.get_mut("comments").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut kept = Vec::new();
    let mut dropped = 0;
    for mut comment in std::mem::take(comments) {
        let login = comment["author"]["login"].as_str().unwrap_or_default().to_string();
        match resolve_trust(src, cache, ctx, &login, now).await {
            Ok(t) if t.trusted() => {
                comment["trusted"] = json!(true);
                kept.push(comment);
            }
            _ => dropped += 1,
        }
    }
    *comments = kept;
    issue["omitted_comments"] = json!(dropped);
    dropped
}

/// The comments a colony started on auto mode may read: only those marked trusted. Used by the
/// prompt builder, so a copy that was never filtered shows none.
pub(crate) fn brief_comments(issue: &Value) -> impl Iterator<Item = &Value> {
    issue["comments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["trusted"].as_bool() == Some(true))
}

/// What the boot of an auto colony checks again, right before the colony reads the issue: the author
/// is still trusted, no label has pulled it back, nobody outside the org edited it, and the comments
/// are reduced to the trusted ones. An `Err` is the reason the colony must not start.
pub(crate) async fn vet<S: TrustSource>(
    src: &S,
    cache: &TrustCache,
    ctx: &Ctx<'_>,
    issue: &mut Value,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let number = issue["number"].as_u64().unwrap_or_default();
    let author = issue["author"]["login"].as_str().unwrap_or_default().to_string();
    let labels: Vec<String> = issue["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["name"].as_str().map(str::to_string))
        .collect();
    if has_label(&labels, LABEL_OPT_OUT) || has_label(&labels, LABEL_NEEDS_HUMAN) {
        return Err("it now carries a no-colonize or needs-human label".into());
    }
    match resolve_trust(src, cache, ctx, &author, now).await {
        Ok(t) if t.trusted() => {}
        Ok(_) => return Err(format!("@{author} is not an org member, collaborator or allowlisted login")),
        Err(e) => return Err(format!("@{author}'s trust could not be checked ({e})")),
    }
    let editors = src
        .editors(ctx.repo, &[number])
        .await
        .map_err(|e| format!("its edit history could not be read ({e})"))?;
    for editor in editors.get(&number).into_iter().flatten() {
        if editor.eq_ignore_ascii_case(&author) {
            continue;
        }
        match resolve_trust(src, cache, ctx, editor, now).await {
            Ok(t) if t.trusted() => {}
            _ => return Err(format!("@{editor}, who is outside the org, edited it")),
        }
    }
    filter_comments(src, cache, ctx, issue, now).await;
    Ok(())
}

/// The boot hook (`boot.rs`): vets the issue of an auto colony with the real GitHub. A colony that
/// did not start on auto mode is untouched.
pub(crate) async fn vet_at_boot(app: &App, s: &Session, issue: &mut Value) -> Result<(), String> {
    if !is_auto(s) {
        return Ok(());
    }
    let all = app.all_org_settings();
    let owner = s.repo.split('/').next().unwrap_or_default();
    let allow = org_entry(&all, owner)
        .map(|o| o.auto_colonize_allow.clone())
        .unwrap_or_default();
    let own = own_login(app).await;
    let ctx = Ctx {
        repo: &s.repo,
        allow: &allow,
        own: &own,
    };
    let result = vet(&GhSource { app }, &CACHE, &ctx, issue, Utc::now()).await;
    let author = issue["author"]["login"].as_str().unwrap_or_default().to_string();
    let mut entry = Entry::new(if result.is_ok() { "intake.trust" } else { "intake.review" }, "colony").colony(s);
    entry.detail = Some(match &result {
        Ok(()) => format!("boot re-check passed for @{author}"),
        Err(why) => format!("boot re-check refused the colony: {why}"),
    });
    activity::record(app, entry).await;
    result.map_err(|why| format!("auto-colonize stopped this colony: {why}; the issue is back in review"))
}

async fn own_login(app: &App) -> String {
    match crate::github::viewer(app).await {
        Ok(user) => user["login"].as_str().unwrap_or_default().to_string(),
        Err(_) => String::new(),
    }
}

// ---------------------------------------------------------------------------------------------
// GitHub
// ---------------------------------------------------------------------------------------------

struct GhSource<'a> {
    app: &'a App,
}

impl GhSource<'_> {
    async fn api(&self, args: Vec<String>) -> Result<String, String> {
        exec_within(GH_LIMIT, &mut self.app.gh(args))
            .await
            .map_err(|e| format!("{e:#}"))
    }
}

fn not_found(error: &str) -> bool {
    let text = error.to_ascii_lowercase();
    text.contains("http 404") || text.contains("not found")
}

impl TrustSource for GhSource<'_> {
    async fn org_role(&self, org: &str, login: &str) -> Result<Option<String>, String> {
        let path = format!("orgs/{org}/memberships/{login}");
        match self
            .api(vec!["api".into(), path, "--jq".into(), r#".state + " " + .role"#.into()])
            .await
        {
            Ok(out) => {
                let out = out.trim();
                Ok(out.strip_prefix("active ").map(str::to_string))
            }
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn permission(&self, repo: &str, login: &str) -> Result<String, String> {
        let path = format!("repos/{repo}/collaborators/{login}/permission");
        match self.api(vec!["api".into(), path, "--jq".into(), ".permission".into()]).await {
            Ok(out) => Ok(out.trim().to_string()),
            Err(e) if not_found(&e) => Ok("none".into()),
            Err(e) => Err(e),
        }
    }

    async fn editors(&self, repo: &str, numbers: &[u64]) -> Result<BTreeMap<u64, Vec<String>>, String> {
        if numbers.is_empty() {
            return Ok(BTreeMap::new());
        }
        let (owner, name) = repo.split_once('/').ok_or("not a repository")?;
        let query = edits_query(numbers);
        let out = self
            .api(vec![
                "api".into(),
                "graphql".into(),
                "-f".into(),
                format!("query={query}"),
                "-f".into(),
                format!("owner={owner}"),
                "-f".into(),
                format!("name={name}"),
            ])
            .await?;
        let value: Value = serde_json::from_str(&out).map_err(|e| format!("unreadable GraphQL answer: {e}"))?;
        parse_editors(&value, numbers)
    }
}

/// One GraphQL query naming every issue's edit history under its own alias (`i<number>`).
pub(crate) fn edits_query(numbers: &[u64]) -> String {
    let mut q = String::from("query($owner:String!,$name:String!){repository(owner:$owner,name:$name){");
    for n in numbers {
        q.push_str(&format!(
            "i{n}:issue(number:{n}){{userContentEdits(first:50){{totalCount nodes{{editor{{login}}}}}}}} "
        ));
    }
    q.push_str("}}");
    q
}

/// Reads [`edits_query`]'s answer. An edit whose editor is gone reads as `ghost`, and a history
/// longer than the page reads as an editor nobody trusts, so neither can slip through.
pub(crate) fn parse_editors(value: &Value, numbers: &[u64]) -> Result<BTreeMap<u64, Vec<String>>, String> {
    if let Some(errors) = value.get("errors") {
        return Err(format!(
            "GraphQL refused the edit history: {}",
            errors.to_string().chars().take(200).collect::<String>()
        ));
    }
    let repository = &value["data"]["repository"];
    if repository.is_null() {
        return Err("GraphQL answered without the repository".into());
    }
    let mut out = BTreeMap::new();
    for n in numbers {
        let edits = &repository[format!("i{n}")]["userContentEdits"];
        if edits.is_null() {
            return Err(format!("GraphQL answered without issue #{n}"));
        }
        let mut logins: Vec<String> = edits["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|node| node["editor"]["login"].as_str().unwrap_or("ghost").to_string())
            .collect();
        let total = edits["totalCount"].as_u64().unwrap_or(0) as usize;
        if total > logins.len() {
            logins.push("<more edits>".into());
        }
        out.insert(*n, logins);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

/// What the sweep remembers between runs (`auto-colonize.json` in the config dir).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct Memory {
    /// `owner/name#number` of issues a person dismissed.
    dismissed: BTreeMap<String, DateTime<Utc>>,
    /// Issues the sweep started a colony on (or found one on), never started twice.
    launched: BTreeMap<String, DateTime<Utc>>,
    /// The verdict last logged per issue, so the log says each decision once.
    decided: BTreeMap<String, (String, DateTime<Utc>)>,
    /// When auto mode was armed per scope (`org:<org>` or `repo:<owner/name>`).
    armed: BTreeMap<String, DateTime<Utc>>,
}

static STATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn state_file(app: &App) -> std::path::PathBuf {
    app.cfg.config_dir.join("auto-colonize.json")
}

fn load_state(app: &App) -> Memory {
    std::fs::read(state_file(app))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

async fn save_state(app: &App, state: &Memory) {
    let write = async {
        std::fs::create_dir_all(&app.cfg.config_dir)?;
        crate::util::write_atomic(&state_file(app), &serde_json::to_vec_pretty(state)?).await
    };
    if let Err(e) = write.await {
        eprintln!("auto-colonize: could not save its state: {e:#}");
    }
}

fn key(repo: &str, number: u64) -> String {
    format!("{}#{number}", repo.to_ascii_lowercase())
}

impl Memory {
    fn prune(&mut self, now: DateTime<Utc>) {
        let cutoff = now - chrono::Duration::days(REMEMBER_DAYS);
        self.dismissed.retain(|_, at| *at > cutoff);
        self.launched.retain(|_, at| *at > cutoff);
        self.decided.retain(|_, (_, at)| *at > cutoff);
    }

    fn numbers(map: &BTreeMap<String, DateTime<Utc>>, repo: &str) -> BTreeSet<u64> {
        let prefix = format!("{}#", repo.to_ascii_lowercase());
        map.keys().filter_map(|k| k.strip_prefix(&prefix)?.parse().ok()).collect()
    }
}

/// The policy a repository's verdicts run under, from the saved settings, the colonies and the state.
async fn policy_for(app: &App, repo: &str, state: &Memory, now: DateTime<Utc>) -> Policy {
    let all = app.all_org_settings();
    let owner = repo.split('/').next().unwrap_or_default();
    let org = org_entry(&all, owner);
    let (mode, source) = effective_mode(&all, repo);
    // The scope the sweep armed: the org's when the org itself runs trusted, else the repository's.
    let armed_key = if org.is_some_and(|o| o.auto_colonize == Some(AutoColonize::Trusted)) {
        format!("org:{}", owner.to_ascii_lowercase())
    } else {
        format!("repo:{}", repo.to_ascii_lowercase())
    };
    let _ = source;
    let sessions = app.sessions.read().await;
    let mut started = Memory::numbers(&state.launched, repo);
    let mut last_hour = 0;
    for s in sessions.iter().filter(|s| s.repo.eq_ignore_ascii_case(repo)) {
        if let Some(n) = s.issue {
            started.insert(n);
        }
        if is_auto(s) && now - s.created_at < chrono::Duration::hours(1) {
            last_hour += 1;
        }
    }
    Policy {
        mode,
        hidden: org.is_some_and(org_hidden),
        allow: org.map(|o| o.auto_colonize_allow.clone()).unwrap_or_default(),
        cap: rate_cap(org),
        own: own_login(app).await,
        armed: state.armed.get(&armed_key).copied(),
        dismissed: Memory::numbers(&state.dismissed, repo),
        started,
        started_last_hour: last_hour,
    }
}

// ---------------------------------------------------------------------------------------------
// The list
// ---------------------------------------------------------------------------------------------

fn row_from_list(issue: &Value) -> Option<Row> {
    Some(Row {
        number: issue["number"].as_u64()?,
        author: issue["author"]["login"].as_str().unwrap_or_default().to_string(),
        labels: issue["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str().map(str::to_string))
            .collect(),
        created_at: issue["createdAt"]
            .as_str()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc)),
    })
}

/// Adds `intake` to every issue of a `gh issue list --json …,createdAt` array. A failure reads as
/// "could not check", never as trust.
pub(crate) async fn annotate(app: &App, repo: &str, issues: Value) -> Value {
    let Value::Array(mut list) = issues else { return issues };
    let rows: Vec<(usize, Row)> = list
        .iter()
        .enumerate()
        .filter_map(|(i, v)| Some((i, row_from_list(v)?)))
        .collect();
    let now = Utc::now();
    let state = load_state(app);
    let policy = policy_for(app, repo, &state, now).await;
    let only_rows: Vec<Row> = rows.iter().map(|(_, r)| r.clone()).collect();
    let verdicts = tokio::time::timeout(LIST_LIMIT, assess(&GhSource { app }, &CACHE, repo, &policy, &only_rows, now))
        .await
        .unwrap_or_else(|_| {
            only_rows
                .iter()
                .map(|r| Intake {
                    reason: Reason::Unverified,
                    author: r.author.clone(),
                    trust: None,
                    editor: None,
                })
                .collect()
        });
    for ((i, _), verdict) in rows.iter().zip(verdicts) {
        list[*i]["intake"] = verdict.to_json();
    }
    Value::Array(list)
}

// ---------------------------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------------------------

/// This module's feature descriptor (`features.rs`). No `token_scope`: the routes stay owner-only,
/// and the activity lines are written by the handlers and the sweep themselves.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "auto_colonize",
    routes,
    token_scope: None,
    activity: &[],
    kinds: &[
        "intake.auto",
        "intake.dismiss",
        "intake.mode",
        "intake.review",
        "intake.trust",
    ],
    start_tasks: Some(start_tasks),
};

pub(crate) fn routes() -> axum::Router<Shared> {
    use axum::routing;
    axum::Router::new()
        .route(
            "/api/repos/{owner}/{name}/auto-colonize",
            routing::get(get_mode).put(put_mode),
        )
        .route("/api/repos/{owner}/{name}/issues/{number}/intake", routing::post(post_intake))
}

fn repo_of(owner: &str, name: &str) -> Result<String, crate::AppError> {
    let repo = format!("{owner}/{name}");
    if valid_repo(&repo) {
        Ok(repo)
    } else {
        Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"))
    }
}

/// `GET /api/repos/{owner}/{name}/auto-colonize`: the repository's effective mode and what it rests on.
async fn get_mode(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let all = app.all_org_settings();
    let (mode, source) = effective_mode(&all, &repo);
    let org = org_entry(&all, &owner);
    let state = load_state(&app);
    let policy = policy_for(&app, &repo, &state, Utc::now()).await;
    Ok(Json(json!({
        "repo": repo,
        "mode": mode,
        "source": source.as_str(),
        "org_mode": org.and_then(|o| o.auto_colonize).unwrap_or_default(),
        "hidden": policy.hidden,
        "rate_per_hour": policy.cap,
        "started_last_hour": policy.started_last_hour,
        "allow": policy.allow,
        "armed_at": policy.armed,
    })))
}

#[derive(Deserialize)]
struct ModeBody {
    /// `off`, `trusted`, or null to follow the org.
    mode: Option<AutoColonize>,
}

/// `PUT /api/repos/{owner}/{name}/auto-colonize {"mode": "off"|"trusted"|null}`: the repository's
/// own choice, kept in its org's settings; null removes it.
async fn put_mode(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Json(body): Json<ModeBody>,
) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    {
        let _config = app.config_write.lock().await;
        let mut all = app.all_org_settings();
        let org = all.entry(owner.clone()).or_default();
        org.auto_colonize_repos.retain(|k, _| !k.trim().eq_ignore_ascii_case(&repo));
        if let Some(mode) = body.mode {
            org.auto_colonize_repos.insert(repo.clone(), mode);
        }
        app.save_org_settings(&all)
            .await
            .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    }
    let mut entry = Entry::new("intake.mode", "you");
    entry.org = Some(owner.clone());
    entry.repo = Some(repo.clone());
    entry.detail = Some(match body.mode {
        Some(AutoColonize::Trusted) => "auto_colonize set to trusted for the repository".into(),
        Some(AutoColonize::Off) => "auto_colonize set to off for the repository".into(),
        None => "the repository now follows its org's auto_colonize".into(),
    });
    activity::record(&app, entry).await;
    get_mode(State(app), Path((owner, name))).await
}

#[derive(Deserialize)]
struct IntakeBody {
    action: String,
}

/// `POST /api/repos/{owner}/{name}/issues/{number}/intake {"action": "dismiss"|"restore"}`.
async fn post_intake(
    State(app): State<Shared>,
    Path((owner, name, number)): Path<(String, String, u64)>,
    Json(body): Json<IntakeBody>,
) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let dismiss = match body.action.as_str() {
        "dismiss" => true,
        "restore" => false,
        _ => return Err(client_error(StatusCode::BAD_REQUEST, "action is dismiss or restore")),
    };
    {
        let _state = STATE_LOCK.lock().await;
        let mut state = load_state(&app);
        let now = Utc::now();
        if dismiss {
            state.dismissed.insert(key(&repo, number), now);
        } else {
            state.dismissed.remove(&key(&repo, number));
        }
        state.decided.remove(&key(&repo, number));
        save_state(&app, &state).await;
    }
    let mut entry = Entry::new(if dismiss { "intake.dismiss" } else { "intake.review" }, "you");
    entry.org = Some(owner);
    entry.repo = Some(repo);
    entry.issue = Some(number);
    entry.detail = Some(
        if dismiss {
            "dismissed from review"
        } else {
            "restored to review"
        }
        .into(),
    );
    activity::record(&app, entry).await;
    Ok(Json(json!({"issue": number, "dismissed": dismiss})))
}

// ---------------------------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------------------------

pub(crate) fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            sweep(&app).await;
        }
    });
}

/// One issue the search found, with what a launch needs.
struct Found {
    repo: String,
    row: Row,
    title: String,
}

/// The scopes auto mode is on for: `org:<org>` for an org that runs trusted, `repo:<repo>` for a
/// repository that does while its org does not. Hidden and switched-off orgs are never in it.
pub(crate) fn scopes(all: &BTreeMap<String, OrgSettings>) -> Vec<String> {
    let mut out = Vec::new();
    for (org, settings) in all {
        if org_hidden(settings) {
            continue;
        }
        if settings.auto_colonize == Some(AutoColonize::Trusted) {
            out.push(format!("org:{org}"));
        } else {
            for (repo, mode) in &settings.auto_colonize_repos {
                if *mode == AutoColonize::Trusted && valid_repo(repo.trim()) {
                    out.push(format!("repo:{}", repo.trim()));
                }
            }
        }
    }
    out
}

fn parse_found(item: &Value) -> Option<Found> {
    let repo = item["repository_url"].as_str()?.rsplit("/repos/").next()?.to_string();
    if !valid_repo(&repo) || item.get("pull_request").is_some() {
        return None;
    }
    let row = row_from_list(&json!({
        "number": item["number"],
        "author": {"login": item["user"]["login"]},
        "labels": item["labels"],
        "createdAt": item["created_at"],
    }))?;
    // `author_association` is deliberately not read: trust is looked up, never taken from the event.
    Some(Found {
        repo,
        row,
        title: item["title"].as_str().unwrap_or_default().to_string(),
    })
}

async fn search(app: &App, scope: &str, armed: DateTime<Utc>) -> Result<Vec<Found>, String> {
    let query = format!("is:issue is:open {scope} created:>{}", armed.format("%Y-%m-%dT%H:%M:%SZ"));
    let q = format!("q={query}");
    let args = [
        "api",
        "--method",
        "GET",
        "search/issues",
        "-f",
        q.as_str(),
        "-f",
        "per_page=100",
        "-f",
        "sort=created",
        "-f",
        "order=desc",
    ];
    let out = exec_within(GH_LIMIT, &mut app.gh(args)).await.map_err(|e| format!("{e:#}"))?;
    let value: Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    Ok(value["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_found)
        .collect())
}

fn describe(i: &Intake) -> String {
    let who = match &i.trust {
        Some(t) => format!("@{} ({})", i.author, t.why()),
        None => format!("@{}", i.author),
    };
    match &i.editor {
        Some(editor) => format!("{}: {who}; edited by @{editor}", i.reason.label()),
        None => format!("{}: {who}", i.reason.label()),
    }
}

async fn sweep(app: &Shared) {
    let now = Utc::now();
    let all = app.all_org_settings();
    let wanted = scopes(&all);
    let _state_guard = STATE_LOCK.lock().await;
    let mut state = load_state(app);
    // Arm what just came on and forget what went off, so switching it on never takes the backlog.
    let before = state.armed.clone();
    state
        .armed
        .retain(|scope, _| wanted.iter().any(|w| w.eq_ignore_ascii_case(scope)));
    for scope in &wanted {
        state.armed.entry(scope.to_ascii_lowercase()).or_insert(now);
    }
    state.prune(now);
    let mut changed = before != state.armed;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for scope in &wanted {
        let armed = state.armed.get(&scope.to_ascii_lowercase()).copied().unwrap_or(now);
        let found = match search(app, scope, armed).await {
            Ok(found) => found,
            Err(e) => {
                eprintln!("auto-colonize: could not search {scope}: {e}");
                continue;
            }
        };
        let mut by_repo: BTreeMap<String, Vec<Found>> = BTreeMap::new();
        for f in found {
            by_repo.entry(f.repo.to_ascii_lowercase()).or_default().push(f);
        }
        for (repo_key, group) in by_repo {
            if !seen.insert(repo_key) {
                continue;
            }
            let repo = group[0].repo.clone();
            changed |= sweep_repo(app, &mut state, &repo, &group, now).await;
        }
    }
    if changed {
        save_state(app, &state).await;
    }
}

/// Decides one repository's new issues and starts what may start. True when the state changed.
async fn sweep_repo(app: &Shared, state: &mut Memory, repo: &str, group: &[Found], now: DateTime<Utc>) -> bool {
    let policy = policy_for(app, repo, state, now).await;
    if policy.mode != AutoColonize::Trusted {
        return false;
    }
    let rows: Vec<Row> = group.iter().map(|f| f.row.clone()).collect();
    let verdicts = assess(&GhSource { app }, &CACHE, repo, &policy, &rows, now).await;
    let org = repo.split('/').next().unwrap_or_default().to_string();
    let mut changed = false;
    for (found, verdict) in group.iter().zip(&verdicts) {
        let k = key(repo, found.row.number);
        let line = |kind: &str, detail: String| {
            let mut e = Entry::new(kind, "colony");
            e.org = Some(org.clone());
            e.repo = Some(repo.to_string());
            e.issue = Some(found.row.number);
            e.title = Some(found.title.clone());
            e.detail = Some(detail);
            e
        };
        // The trust decision about the author, once per issue.
        if !state.decided.contains_key(&k)
            && let Some(trust) = &verdict.trust
        {
            let verdict_word = if trust.trusted() { "trusted" } else { "not trusted" };
            activity::record(
                app,
                line("intake.trust", format!("@{} {verdict_word}: {}", verdict.author, trust.why())),
            )
            .await;
        }
        // The verdict, each time it changes.
        let code = verdict.reason.code().to_string();
        if state.decided.get(&k).map(|(c, _)| c.as_str()) != Some(code.as_str()) && !verdict.launchable() {
            if verdict.reason != Reason::Started {
                let kind = if verdict.auto() { "intake.auto" } else { "intake.review" };
                activity::record(app, line(kind, describe(verdict))).await;
            }
            state.decided.insert(k.clone(), (code, now));
            changed = true;
        }
        if !verdict.launchable() {
            continue;
        }
        let body = json!({
            "repo": repo,
            "issue": found.row.number,
            "title": found.title,
            "origin": ORIGIN,
        });
        let Ok(new_session) = serde_json::from_value::<crate::sessions::NewSession>(body) else {
            continue;
        };
        match crate::sessions::create(State(app.clone()), None, Json(new_session)).await {
            Ok(Json(session)) => {
                let note = format!("auto: trusted author @{}", verdict.author);
                app.update_session(&session.id, |s| s.auto_note = Some(note.clone())).await;
                let mut e = line(
                    "intake.auto",
                    format!("{note} ({})", verdict.trust.as_ref().map(Trust::why).unwrap_or_default()),
                );
                e.colony = Some(session.id.clone());
                activity::record(app, e).await;
                state.launched.insert(k.clone(), now);
                state.decided.insert(k, ("started".into(), now));
                changed = true;
            }
            // Refused for good (an epic, an issue another colony holds): never tried again.
            Err(e) if e.0 == StatusCode::CONFLICT => {
                activity::record(app, line("intake.review", format!("not started: {:#}", e.1))).await;
                state.launched.insert(k, now);
                changed = true;
            }
            // Anything else (a login gap, a full disk) is tried again on the next sweep.
            Err(e) => eprintln!("auto-colonize: could not start {repo}#{}: {:#}", found.row.number, e.1),
        }
    }
    changed
}

#[cfg(test)]
mod tests;
