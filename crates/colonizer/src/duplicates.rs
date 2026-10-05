//! One owner for "is this work already being done, and by whom" (issue #832).
//!
//! Every colony launch comes through `sessions::create` — the cockpit, `colonizer launch`, the API,
//! MCP, the colony loops, burn-down, the red team, a hand-off and the merge train's redo — and that
//! handler asks this module once before anything is created ([`check_launch`]) and again inside the
//! admission lock ([`check`]), where checking and inserting are one atomic step. The supply-chain
//! loop asks the same [`check`] before it dispatches, so a skipped target and a refused launch are
//! the same decision (issue #821), and the merge-time supersede pass reads the same supply-chain
//! rule ([`supply_chain_overlap`]).
//!
//! The rules, in the order they are asked:
//!
//! - **Issue.** A colony holds its issue while it is queued, live, publishing, or its pull request
//!   is open ([`holds_issue`]). The holder is the first such colony that is not a `claim_wait`
//!   waiter, else the oldest waiter ([`issue_held_by`]). `queue_behind_holder` turns this hold into
//!   a wait instead of a refusal.
//! - **Supply-chain target.** A colony's supply-chain claim is the set of package/advisory pairs it
//!   was launched to fix ([`claim`]): the Packages view's one target, or every finding a loop
//!   colony was dispatched with. Two claims in one repository collide when they share a package
//!   and an advisory; a side that names no advisory (a yanked or outdated release) matches every
//!   advisory of its package. The hold lasts while the colony holds an issue would, and also while
//!   it is parked, since a parked fix resumes and pushes the same bump. A supply-chain fix is never
//!   a queue: a collision is refused even with `queue_behind_holder`.
//! - **Remote claim.** A second mothership shares no memory with this one, so for an issue the
//!   GitHub claim is checked too (`claims.rs` reads it; [`remote_refusal`] words it).
//!
//! `allow_duplicate` skips every rule. A refusal is a 409 whose body carries the holder
//! (`duplicate`: kind, colony, host, status, pull request), which the cockpit shows with a link and
//! an Allow duplicate option.

use crate::claims::{ClaimKind, RemoteClaimInfo};
use crate::sessions::{Session, SessionStatus};
use crate::supersede::SupplyChainTarget;
use serde::Serialize;

/// The title prefix the Packages view's hand-off gives a colony it starts on one risk. A colony
/// from before targets were recorded on the session claims its package by this title alone.
pub(crate) const HAND_OFF_PREFIX: &str = "Supply chain: ";

/// What a launch is about to work on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Work {
    pub repo: String,
    pub issue: Option<u64>,
    /// The package/advisory pairs it fixes; empty for everything that is not a supply-chain fix.
    pub supply_chain: Vec<SupplyChainTarget>,
    /// Who launched it (`supply-chain:cargo`, `loop:…`). A supply-chain loop colony with no record
    /// of its findings claims every target of its own origin.
    pub origin: Option<String>,
}

impl Work {
    /// The work a launch request asks for, whichever path built it — the cockpit's or the API's
    /// JSON, `colonizer launch`'s body, MCP's, a loop's — so every path asks the same question.
    /// Targets are normalized the way the colony record keeps them.
    pub fn requested(repo: &str, req: &crate::sessions::NewSession) -> Self {
        Self {
            repo: repo.to_string(),
            issue: req.issue,
            supply_chain: req
                .supply_chain
                .iter()
                .chain(&req.supply_chain_targets)
                .map(|t| SupplyChainTarget::new(&t.package, &t.advisory))
                .collect(),
            origin: req.origin.clone(),
        }
    }

    /// The work a colony record about to be admitted is on: its issue and the targets it was
    /// launched with (never a title read as a target — that fallback is only for old holders).
    pub fn of(s: &Session) -> Self {
        Self {
            repo: s.repo.clone(),
            issue: s.issue,
            supply_chain: s.supply_chain.iter().chain(&s.supply_chain_targets).cloned().collect(),
            origin: s.origin.clone(),
        }
    }
}

/// The findings a supply-chain loop colony was dispatched with, from the loop's own records: what
/// a loop colony started before targets rode on the session claims.
#[derive(Clone, Debug, PartialEq)]
pub struct Recorded {
    pub session: String,
    pub targets: Vec<SupplyChainTarget>,
}

/// Which rule refused the launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldKind {
    Issue,
    SupplyChain,
    RemoteClaim,
}

/// Who already holds the work, as the 409 body's `duplicate` field carries it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Holder {
    pub kind: HoldKind,
    /// The holding colony's id, when known (a remote claim found by its branch or label may not say).
    pub colony: Option<String>,
    /// The host a remote claim names; `None` for a colony on this mothership.
    pub host: Option<String>,
    /// The holding colony's status, for a colony on this mothership.
    pub status: Option<String>,
    pub pr_url: Option<String>,
    pub issue: Option<u64>,
    /// The held work in words: `#7`, `lodash / ghsa-1`.
    pub what: String,
    /// Whether `queue_behind_holder` would wait for this holder instead of being refused.
    pub queueable: bool,
}

/// A launch refused as a duplicate: the holder, and the words the 409 says it in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub holder: Holder,
    pub message: String,
}

impl Refusal {
    /// Whether colony `id` is the holder.
    #[cfg(test)]
    pub fn is_held_by(&self, id: &str) -> bool {
        self.holder.colony.as_deref() == Some(id)
    }

    /// The 409 the API answers with; its body names the holder under `duplicate` (app.rs).
    pub fn into_error(self) -> crate::AppError {
        crate::AppError(axum::http::StatusCode::CONFLICT, anyhow::Error::new(self))
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// The answer to "is this work already being done".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nobody holds it: start.
    Allow,
    /// A local colony holds the issue and the launch asked to wait: queue behind this colony.
    Queue(String),
    /// Someone holds it: refuse, naming them.
    Refuse(Refusal),
}

/// Whether `s` is in one of the states that hold its issue against a second colony: still live or
/// queued somewhere, or published with its pull request open and waiting to be read. Shared with
/// the queue's waiter promotion and the boot-time claim reconcile (`claims.rs`).
///
/// On 2026-09-16/17 `FindsYou-Work/app` issue #7 drew **four** colonies — two of them ten seconds
/// apart, a double submission — and issue #13 drew two. Nothing here refuses the retry that
/// matters: a colony that stopped, failed, found no changes, or whose pull request is merged or
/// closed leaves the issue free.
pub(crate) fn holds_issue(s: &Session) -> bool {
    matches!(
        s.status,
        SessionStatus::Queued
            | SessionStatus::Starting
            | SessionStatus::Running
            | SessionStatus::WaitingForAnswer
            | SessionStatus::Idle
            | SessionStatus::Publishing
            | SessionStatus::PrOpened
    )
}

/// Whether `s` still holds its supply-chain claim: as it would an issue, or parked — a parked fix
/// resumes and pushes the same bump.
pub(crate) fn holds_supply_chain(s: &Session) -> bool {
    holds_issue(s) || s.status == SessionStatus::Parked
}

/// The first holding colony that is not a `claim_wait` waiter, else — once the holder is gone and
/// only waiters remain — the oldest waiter (issue #321), so a fresh launch is refused naming, or
/// queues behind, the waiter whose turn is next, never one that arrived later.
fn effective_holder(sessions: &[Session], holding: impl Fn(&Session) -> bool) -> Option<&Session> {
    sessions
        .iter()
        .find(|s| holding(s) && !s.claim_wait)
        .or_else(|| sessions.iter().filter(|s| holding(s)).min_by_key(|s| s.created_at))
}

/// The colony effectively holding `issue` in `repo`.
pub(crate) fn issue_held_by(sessions: &[Session], repo: &str, issue: u64) -> Option<Session> {
    effective_holder(sessions, |s| holds_issue(s) && s.repo == repo && s.issue == Some(issue)).cloned()
}

/// A colony's supply-chain claim: its launch target, the findings a loop dispatched it with, the
/// loop's record of them for a colony from before they rode on the session, or — for a Packages
/// hand-off from before targets were recorded — the package its title names, any advisory.
pub fn claim(s: &Session, recorded: &[Recorded]) -> Vec<SupplyChainTarget> {
    let mut out: Vec<SupplyChainTarget> = s.supply_chain.iter().cloned().collect();
    out.extend(s.supply_chain_targets.iter().cloned());
    if out.is_empty() {
        if let Some(r) = recorded.iter().find(|r| r.session == s.id) {
            out.extend(r.targets.iter().cloned());
        } else if s.origin.is_none()
            && let Some(name) = s.issue_title.strip_prefix(HAND_OFF_PREFIX).map(str::trim)
            && !name.is_empty()
        {
            out.push(SupplyChainTarget::new(name, ""));
        }
    }
    out
}

/// One pair of targets is the same work: the same package, and the same advisory or one side
/// naming none.
fn same_target(a: &SupplyChainTarget, b: &SupplyChainTarget) -> bool {
    let (aa, ba) = (a.advisory.trim(), b.advisory.trim());
    a.package.trim().eq_ignore_ascii_case(b.package.trim()) && (aa.is_empty() || ba.is_empty() || aa.eq_ignore_ascii_case(ba))
}

/// The targets of `ours` that `theirs` already covers.
fn shared_targets<'a>(ours: &'a [SupplyChainTarget], theirs: &[SupplyChainTarget]) -> Vec<&'a SupplyChainTarget> {
    ours.iter().filter(|o| theirs.iter().any(|t| same_target(o, t))).collect()
}

/// Whether two colonies of one repository are on the same supply-chain work: the rule the launch
/// refusal reads, applied at merge time by the supersede pass.
pub fn supply_chain_overlap(a: &Session, b: &Session) -> bool {
    a.repo.eq_ignore_ascii_case(&b.repo) && !shared_targets(&claim(a, &[]), &claim(b, &[])).is_empty()
}

/// Whether colony `s` is on any of `work`'s supply-chain targets.
fn on_supply_chain(s: &Session, recorded: &[Recorded], work: &Work) -> bool {
    let theirs = claim(s, recorded);
    if theirs.is_empty() {
        // A loop colony with no record of its findings: as far as anyone can tell it is on every
        // target its own origin dispatches.
        return work.origin.is_some() && s.origin == work.origin;
    }
    !shared_targets(&work.supply_chain, &theirs).is_empty()
}

/// The colony effectively holding any of `work`'s supply-chain targets.
pub(crate) fn supply_chain_held_by<'a>(sessions: &'a [Session], recorded: &[Recorded], work: &Work) -> Option<&'a Session> {
    if work.supply_chain.is_empty() {
        return None;
    }
    effective_holder(sessions, |s| {
        s.repo.eq_ignore_ascii_case(&work.repo) && holds_supply_chain(s) && on_supply_chain(s, recorded, work)
    })
}

/// Where a local holder's work stands, for a refusal.
fn where_it_is(held: &Session) -> String {
    match held.pr_url.as_deref() {
        Some(url) => format!("its pull request is open at {url}"),
        None => format!("it is {}", held.status.as_str()),
    }
}

fn local_holder(held: &Session, kind: HoldKind, what: String) -> Holder {
    Holder {
        kind,
        colony: Some(held.id.clone()),
        host: None,
        status: Some(held.status.as_str().to_string()),
        pr_url: held.pr_url.clone(),
        issue: held.issue,
        what,
        queueable: kind == HoldKind::Issue,
    }
}

/// The 409 message for a second colony on an issue another colony still holds.
pub(crate) fn issue_message(held: &Session, issue: u64) -> String {
    format!(
        "colony {} is already on #{issue} and {}. Starting a second one duplicates its \
         work: read that colony first, or pass allow_duplicate (`colonizer launch --allow-duplicate`) \
         to start another anyway, or queue_behind_holder (`colonizer launch --queue-behind-holder`) \
         to wait for it.",
        held.id,
        where_it_is(held)
    )
}

fn issue_refusal(held: &Session, issue: u64) -> Refusal {
    Refusal {
        holder: local_holder(held, HoldKind::Issue, format!("#{issue}")),
        message: issue_message(held, issue),
    }
}

/// A target in words: `package / advisory`, or the package alone when no advisory is named.
fn target_words(t: &SupplyChainTarget) -> String {
    if t.advisory.trim().is_empty() {
        t.package.clone()
    } else {
        format!("{} / {}", t.package, t.advisory)
    }
}

fn supply_chain_refusal(held: &Session, recorded: &[Recorded], work: &Work) -> Refusal {
    let theirs = claim(held, recorded);
    let shared = shared_targets(&work.supply_chain, &theirs);
    let shown: Vec<String> = if shared.is_empty() {
        work.supply_chain.iter().collect::<Vec<_>>()
    } else {
        shared
    }
    .into_iter()
    .map(target_words)
    .collect();
    let what = shown.join(", ");
    let message = format!(
        "colony {} is already on this supply-chain target ({what}) and {}. Starting a second one \
         duplicates its work: read that colony first, or pass allow_duplicate \
         (`colonizer launch --allow-duplicate`) to start another anyway.",
        held.id,
        where_it_is(held)
    );
    Refusal {
        holder: local_holder(held, HoldKind::SupplyChain, what),
        message,
    }
}

/// The local answer, pure: the issue hold first, then the supply-chain hold. Called by the launch
/// pre-check, the in-lock re-check, and the supply-chain loop's dispatch plan.
pub fn check(
    sessions: &[Session],
    recorded: &[Recorded],
    work: &Work,
    allow_duplicate: bool,
    queue_behind_holder: bool,
) -> Verdict {
    if allow_duplicate {
        return Verdict::Allow;
    }
    let mut queue_behind = None;
    if let Some(issue) = work.issue
        && let Some(held) = issue_held_by(sessions, &work.repo, issue)
    {
        if !queue_behind_holder {
            return Verdict::Refuse(issue_refusal(&held, issue));
        }
        queue_behind = Some(held.id);
    }
    if let Some(held) = supply_chain_held_by(sessions, recorded, work) {
        return Verdict::Refuse(supply_chain_refusal(held, recorded, work));
    }
    match queue_behind {
        Some(id) => Verdict::Queue(id),
        None => Verdict::Allow,
    }
}

/// The holder a GitHub claim names.
fn remote_holder(info: &RemoteClaimInfo, issue: u64) -> Holder {
    Holder {
        kind: HoldKind::RemoteClaim,
        colony: info.colony.clone(),
        host: info.host.clone(),
        status: None,
        pr_url: (info.kind == ClaimKind::PullRequest).then(|| info.detail.clone()),
        issue: Some(issue),
        what: format!("#{issue}"),
        queueable: false,
    }
}

/// A launch refused for a claim on GitHub, in `claims.rs`'s words, with the note that the holder's
/// host is unreachable when the fleet reads it so (issue #688).
pub fn remote_refusal(info: &RemoteClaimInfo, issue: u64, holder_down: bool) -> Refusal {
    let mut message = crate::claims::remote_conflict_message(info, issue);
    if holder_down {
        message.push_str(crate::claims::UNREACHABLE_HOLDER_NOTE);
    }
    Refusal {
        holder: remote_holder(info, issue),
        message,
    }
}

/// The whole launch-time answer: the local rules, then — for an issue, unless `allow_duplicate` —
/// the GitHub claim another mothership may hold. `Ok(true)` means queue behind a local holder.
/// A failed GitHub lookup degrades to the local answer rather than refusing the launch.
#[allow(clippy::result_large_err)]
pub async fn check_launch(
    app: &crate::Shared,
    work: &Work,
    allow_duplicate: bool,
    queue_behind_holder: bool,
    unreachable_ids: &[String],
) -> Result<bool, Refusal> {
    let recorded = crate::supply_chain_loop::recorded(app).await;
    let queue = match check(
        &app.sessions.read().await,
        &recorded,
        work,
        allow_duplicate,
        queue_behind_holder,
    ) {
        Verdict::Refuse(refusal) => return Err(refusal),
        Verdict::Queue(_) => true,
        Verdict::Allow => false,
    };
    let Some(issue) = work
        .issue
        .filter(|_| crate::claims::should_check_remote(work.issue, allow_duplicate))
    else {
        return Ok(queue);
    };
    let checked = crate::claims::check_remote_claim(app, &work.repo, issue).await;
    if let Err(e) = &checked {
        eprintln!(
            "claims: remote duplicate check for #{issue} in {} failed ({e:#}); falling back to the local guard",
            work.repo
        );
    }
    let Some(info) = crate::claims::remote_result_or_fallback(checked) else {
        return Ok(queue);
    };
    if queue {
        // Issue #321: the waiter tolerates only a claim of ours — the holder it queues behind, or
        // another colony on this mothership; `claim_wait_conflict` refuses the rest.
        let sessions = app.sessions.read().await;
        let ours: Vec<&str> = sessions
            .iter()
            .filter(|s| s.repo == work.repo && s.issue == Some(issue))
            .map(|s| s.id.as_str())
            .collect();
        return match crate::claims::claim_wait_conflict(Some(&info), issue, &ours) {
            Some(message) => Err(Refusal {
                holder: remote_holder(&info, issue),
                message,
            }),
            None => Ok(true),
        };
    }
    let holder_down = info
        .host
        .as_deref()
        .is_some_and(|host| crate::claims::holder_host_unreachable(host, unreachable_ids));
    Err(remote_refusal(&info, issue, holder_down))
}

#[cfg(test)]
mod tests;
