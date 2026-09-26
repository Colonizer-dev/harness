//! Starting a colony: `POST /api/sessions` (`create`), its admission rules (issue holds, overlap
//! queueing, stacking, org switches) and the settings a launch resolves.

use super::*;

#[derive(Deserialize)]
pub struct NewSession {
    pub repo: String,
    #[serde(default)]
    pub issue: Option<u64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub instructions: String,
    /// Omitted uses the publish module's `autopilot` setting.
    #[serde(default)]
    pub autopilot: Option<bool>,
    /// How this colony's completion claims are verified: `auto` (the default), `none`, or an
    /// explicit test command. Omitted uses the publish module's `verify` setting.
    #[serde(default)]
    pub verify: Option<String>,
    /// Whether a filed finding from this colony spawns a fix colony; omitted uses the publish
    /// module's `autofix` setting.
    #[serde(default)]
    pub autofix: Option<bool>,
    /// Whether a fix colony's review-passing pull request merges itself; omitted uses the publish
    /// module's `automerge` setting, which is only read when autofix is also on.
    #[serde(default)]
    pub automerge: Option<bool>,
    /// Start a colony on an issue another colony already holds. Off by default: see `issue_held_by`.
    #[serde(default)]
    pub allow_duplicate: bool,
    /// Start a colony on an epic — an issue with sub-issues, an `epic` label, or a title marking it
    /// as one. Off by default: an epic is a planning container, refused with a 409 (epic.rs).
    #[serde(default)]
    pub allow_epic: bool,
    /// Wait politely for an issue another local colony holds instead of being refused: the colony
    /// is admitted `Queued` behind the holder and starts when the issue becomes its own (issue
    /// #321). Off by default; `allow_duplicate` wins when both are set, and a claim another
    /// mothership holds is still refused either way.
    #[serde(default)]
    pub queue_behind_holder: bool,
    /// Run this colony on a named model tier — `low`, `medium` or `high` — instead of the one the
    /// routing rule picks for the task.
    #[serde(default)]
    pub model_tier: Option<String>,
    /// Run the orchestrator on this model (a Claude alias or ID, or `<provider>/<model>` naming a
    /// configured provider) instead of the one routing picks. Red-team hunters use it.
    #[serde(default)]
    pub model_override: Option<String>,
    /// Run the colony's subagents on this model instead of the agent module's `subagent_model`.
    #[serde(default)]
    pub subagent_model_override: Option<String>,
    /// Bill this colony to a named Claude account instead of the org's override or install default.
    #[serde(default)]
    pub claude_account: Option<String>,
    /// How a colony created with `after` relates to its parent. By default the colony queues until
    /// the parent's pull request merges, then starts from the fresh default branch — so a parent
    /// that merges with delete-branch never strands the child's pull request. Only `stack: true`
    /// branches from the parent's branch while it is still open, with the pull request targeting it.
    #[serde(default)]
    pub after: Option<String>,
    /// Stack this colony against its parent's branch instead of queueing for the parent's merge:
    /// the colony starts as soon as the parent has pushed its branch, and its pull request targets
    /// that branch. Off by default: a child that only needs the parent's work merged waits for it.
    #[serde(default)]
    pub stack: bool,
    /// Who is asking, when the operator is not: the burn-down scheduler tags its colonies
    /// `Some("burn_down")` so `POST /api/burn-down/stop` can find them again.
    #[serde(default)]
    pub origin: Option<String>,
    /// Opt in to overlap-aware queueing: queue behind a live same-repo colony that's already
    /// touching files, instead of developing against the same paths at once. Off by default —
    /// most callers would rather start immediately than have an unrelated colony's edits hold
    /// them up. See `overlap_queue_target`.
    #[serde(default)]
    pub serialize: Option<bool>,
}

/// A launch-time model choice, trimmed: empty is none, and a `<provider>/<model>` must name a
/// configured provider, so a typo is refused at launch instead of failing inside the colony.
pub(crate) fn launch_model(app: &crate::App, raw: Option<&str>, what: &str) -> Result<Option<String>, crate::AppError> {
    let Some(model) = raw.map(str::trim).filter(|m| !m.is_empty()) else {
        return Ok(None);
    };
    if let Some((provider, name)) = model.split_once('/')
        && (name.is_empty() || !app.providers().iter().any(|p| p.id == provider))
    {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("the {what} {model:?} names no configured provider \"{provider}\""),
        ));
    }
    Ok(Some(model.to_string()))
}

/// A colony that makes a second one on the same issue a mistake rather than a retry: one still
/// live or queued, or one whose pull request is open and waiting to be read.
///
/// On 2026-09-16/17 `FindsYou-Work/app` issue #7 drew **four** colonies — two of them ten seconds
/// apart, a double submission — and issue #13 drew two. Three of the four wrote a complete,
/// working implementation of the same feature; one was merged and the rest were closed unread.
/// Nothing here refuses the retry that matters: a colony that stopped, failed, found no changes,
/// or whose pull request is merged or closed leaves the issue free.
/// Whether `s` is in one of the states that hold its issue against a second colony: still live or
/// queued somewhere, or published with its pull request open and waiting to be read. The shared
/// predicate behind [`issue_held_by`] and the boot-time claim reconcile (`claims.rs`).
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

/// The colony effectively holding `issue`: the first holding session that is not a `claim_wait`
/// waiter, else — once the holder is gone and only waiters remain — the oldest waiter (issue #321).
/// The oldest-first tiebreak is what keeps a waiter queue honest: a fresh launch is refused naming,
/// or queues behind, the waiter whose turn is next, never one that arrived later, and a waiter can
/// never be jumped by a later one.
pub(crate) fn issue_held_by(sessions: &[Session], repo: &str, issue: u64) -> Option<Session> {
    let holding = |s: &Session| holds_issue(s) && s.repo == repo && s.issue == Some(issue);
    sessions
        .iter()
        .find(|s| holding(s) && !s.claim_wait)
        .or_else(|| sessions.iter().filter(|s| holding(s)).min_by_key(|s| s.created_at))
        .cloned()
}

/// The 409 message for a second colony on an issue another colony still holds: the holder, where
/// its work stands, and the way out. One function, so the fast-path pre-check and the authoritative
/// in-lock re-check refuse with the same words.
fn duplicate_message(held: &Session, issue: u64) -> String {
    let where_it_is = match held.pr_url.as_deref() {
        Some(url) => format!("its pull request is open at {url}"),
        None => format!("it is {}", held.status.as_str()),
    };
    format!(
        "colony {} is already on #{issue} and {where_it_is}. Starting a second one duplicates its \
         work: read that colony first, or pass allow_duplicate to start another anyway.",
        held.id
    )
}

/// Issue #453: the colony a fresh same-repo colony queues behind for overlap, if any — the oldest
/// live same-repo colony that already has a worktree, and so may be touching files. Pure, so the
/// rule is testable apart from the file scan that [`overlap_queue_target`] wraps around it.
pub(crate) fn overlap_holder(sessions: &[Session], repo: &str) -> Option<String> {
    sessions
        .iter()
        .filter(|s| s.repo == repo && s.status.is_live() && s.git_admin_dir.is_some())
        .min_by_key(|s| s.created_at)
        .map(|s| s.id.clone())
}

/// Issue #453: who a fresh colony queues behind, if anyone — the [`overlap_holder`], and only while
/// some live sibling still has touched files. This is not a real overlap check: the newcomer's own
/// file set is unknown until it boots, so any live touched files queue it
/// (`rebase::should_queue_behind_live_colony`, the conservative reading). Best effort with a short
/// overall budget: an unreadable worktree reads as untouched, never as a reason to queue.
async fn overlap_queue_target(sessions: &[Session], repo: &str) -> Option<String> {
    let holder = overlap_holder(sessions, repo)?;
    let siblings: Vec<(String, String)> = sessions
        .iter()
        .filter(|s| s.repo == repo && s.status.is_live() && s.git_admin_dir.is_some())
        .map(|s| (s.worktree.clone(), s.base.clone().unwrap_or_else(|| "main".to_string())))
        .collect();
    let mut live_files = Vec::new();
    let scan = async {
        for (worktree, base) in &siblings {
            live_files.extend(crate::rebase::touched_files(std::path::Path::new(worktree), &format!("origin/{base}")).await);
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), scan).await;
    live_files.sort();
    crate::rebase::should_queue_behind_live_colony(&live_files).then_some(holder)
}

/// The authoritative duplicate-colony claim, run while the admission write lock is held: the
/// pre-check in `create` reads under a read lock, so two launches can both pass it before either
/// inserts — this re-check closes that window, and the loser gets its holder back for a 409.
/// `Ok` carries the admitted colony, whether it queued, and how many were already waiting;
/// `Err` carries the colony already holding the issue, and nothing is inserted.
#[allow(clippy::result_large_err, clippy::too_many_arguments)]
fn try_claim_session(
    sessions: &mut Vec<Session>,
    room: bool,
    mut session: Session,
    repo: &str,
    issue: Option<u64>,
    allow_duplicate: bool,
    queue_behind_holder: bool,
    wait_for_parent: bool,
) -> Result<(Session, bool, usize), Session> {
    // Issue #321: a launch that asked to wait its turn is not refused when the issue is held — it
    // is admitted as a `claim_wait` waiter behind whoever effectively holds it, however full or
    // empty the queue. The holder's mark on GitHub stays; the waiter never claims over it.
    let mut queued_for_holder = false;
    if let (Some(number), false) = (issue, allow_duplicate)
        && let Some(held) = issue_held_by(sessions, repo, number)
    {
        if !queue_behind_holder {
            return Err(held);
        }
        queued_for_holder = true;
        session.claim_wait = true;
        session.queued_behind = Some(held.id);
    }
    // A colony still waiting for its parent's branch queues even when a slot is free: booting now
    // would branch from the default branch, which is exactly what stacking exists to avoid. A
    // queued colony holds no slot, so nothing is wasted by the wait.
    // Issue #453: a newcomer queued behind a live same-repo colony for overlap stays queued even
    // with a free slot, and never carries a parent — it still branches fresh from the default
    // branch when the queue starts it. A holder that finished between the scan and this lock
    // releases it at once, clearing the stale pointer.
    let overlap_held = !queued_for_holder
        && session.parent.is_none()
        && session
            .queued_behind
            .as_deref()
            .is_some_and(|holder| sessions.iter().any(|s| s.id == holder && s.status.is_live()));
    if !overlap_held && !queued_for_holder {
        session.queued_behind = None;
    }
    session.status = if room && !wait_for_parent && !overlap_held && !queued_for_holder {
        SessionStatus::Starting
    } else {
        SessionStatus::Queued
    };
    let queued = session.status == SessionStatus::Queued;
    let waiting = sessions.iter().filter(|s| s.status == SessionStatus::Queued).count();
    sessions.push(session.clone());
    Ok((session, queued, waiting))
}

/// What the admission lock returned for a launch: the duplicate claim's outcome, or — issue #508 —
/// a scoped token's caps refusing the launch. The pre-check at the top of `create` reads under a
/// read lock, so two launches of one token could both pass its caps before either inserted; the
/// re-check runs inside `with_slot`'s write guard, where counting and inserting are one atomic
/// step, closing that window the way the duplicate re-check does. The claim is boxed: a colony
/// record is large, and a cap refusal carries only a message.
enum Admission {
    Claimed(Box<Result<(Session, bool, usize), Session>>),
    Capped(String),
}

/// Whether colonies may file validated findings as issues. On unless switched off in Settings.
pub(crate) fn findings_enabled(app: &App, modules: &ModulesConfig) -> bool {
    let schema = schema_for("publish", &modules.publish.provider, &app.agents);
    setting(&modules.publish, &schema, "file_findings")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// The publish module's boolean setting `key`, its schema default when nothing is set.
async fn publish_bool(app: &App, key: &str) -> bool {
    let modules = app.modules.read().await.clone();
    let schema = schema_for("publish", &modules.publish.provider, &app.agents);
    setting(&modules.publish, &schema, key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether a finding that passes validation spawns a fix colony: the launch choice on the session,
/// or the publish module's `autofix` setting.
pub(crate) async fn autofix_enabled(app: &App, s: &Session) -> bool {
    match s.autofix {
        Some(on) => on,
        None => publish_bool(app, "autofix").await,
    }
}

/// Whether a fix colony's review-passing pull request merges itself: the launch choice on the
/// session, or the publish module's `automerge` setting. An explicit choice always counts — a fix
/// colony's `automerge` was written from its hunter's decision at creation, so gating it on the fix
/// colony's own autofix (which is `Some(false)`, to stop it cascading further colonies) would
/// quietly undo the hunter's opt-in. Only the module *default* is gated on autofix: a default
/// automerge while default-autofix is off is a setting nobody can have meant.
pub(crate) async fn automerge_enabled(app: &App, s: &Session) -> bool {
    match s.automerge {
        Some(on) => on,
        None => autofix_enabled(app, s).await && publish_bool(app, "automerge").await,
    }
}

/// The container image a colony boots: what the given stack's preset names, unless modules.json sets
/// an image of its own. The stack may be the configured one rather than a detected one, so it goes
/// through [`crate::presets::resolved`] — a no-op for callers holding a repository in hand, and the
/// fallback for the ones (the Setup pane, telemetry) that pass `auto` with nothing to detect from.
/// Takes the agent list rather than the app, so usage.rs can resolve the same image for its
/// changed-from-default check without an `App`.
pub(crate) fn colony_image(agents: &[AgentModule], modules: &ModulesConfig, stack: &str) -> String {
    let schema = schema_for("sandbox", &modules.sandbox.provider, agents);
    let settings = crate::config::with_preset(&modules.sandbox, &crate::presets::defaults(crate::presets::resolved(stack)));
    setting_str(&settings, &schema, "image")
}

/// Whether new colonies publish automatically: the publish module's `autopilot` setting. Takes the
/// agent list rather than the app, so usage.rs can report the same default.
pub(crate) fn autopilot_default(agents: &[AgentModule], modules: &ModulesConfig) -> bool {
    let schema = schema_for("publish", &modules.publish.provider, agents);
    setting(&modules.publish, &schema, "autopilot")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// How new colonies' completion claims are verified (issue #328): the publish module's `verify`
/// setting — `auto`, `none`, or an explicit test command.
pub(crate) fn verify_default(agents: &[AgentModule], modules: &ModulesConfig) -> String {
    let schema = schema_for("publish", &modules.publish.provider, agents);
    setting_str(&modules.publish, &schema, "verify")
}

#[allow(clippy::result_large_err)]
pub async fn create(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
    Json(req): Json<NewSession>,
) -> ApiResult<Session> {
    let repo = req.repo.trim().to_string();
    if !valid_repo(&repo) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    // A switched-off workspace refuses new work but nothing else: colonies it already has stay
    // listed, queueable and resumable, and its settings survive for the day it is switched back on.
    let owner = repo.split('/').next().unwrap_or_default();
    // A scoped launch token (issue #508) launches only inside its org/repo limits, and only under
    // its concurrency cap and daily budget — checked before anything else, so a launch the token
    // may not make never gets as far as a worktree. The scope gate itself ran already, in
    // `authorize` (`host_guard`).
    let scoped = scoped.map(|axum::Extension(tok)| tok);
    if let Some(token) = &scoped
        && !token.covers(owner, &repo)
    {
        return Err(client_error(
            StatusCode::FORBIDDEN,
            &format!("this API token's org/repo limits do not include {repo}"),
        ));
    }
    if let Some(token) = &scoped
        && let Some(reason) = crate::api_tokens::launch_cap_error(token, &app.sessions.read().await, Utc::now())
    {
        return Err(client_error(StatusCode::TOO_MANY_REQUESTS, &reason));
    }
    if !orgs::org_enabled(&app.org_settings(owner)) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("the {owner} workspace is switched off; turn it back on in its org settings to start a colony there"),
        ));
    }
    let modules = app.modules.read().await.clone();
    // Which agent module the colony launches on: this org's pick, else the install's (issue #201).
    // Recorded on the session, so boot re-resolves from that and a later change moves new colonies only.
    let agent = app
        .agents
        .iter()
        .find(|a| a.id == orgs::effective_agent_module(&app.org_settings(owner), &modules))
        .ok_or_else(|| client_error(StatusCode::BAD_REQUEST, "the selected agent module is not installed"))?;
    // The colony's account: the request's explicit choice, else the org's override, else the
    // install default. Resolved before the gate so the refusal can name the account that is missing.
    let claude_account = crate::claude_accounts::resolve_account(
        req.claude_account.as_deref(),
        app.org_settings(owner)
            .agent
            .as_ref()
            .and_then(|a| a.claude_account.as_deref()),
        &crate::claude_accounts::load_meta(&app.cfg.config_dir),
    );
    // The id becomes a path under <config>/claude-accounts, so refuse anything that is not a plain
    // account id rather than joining it. The org override and the stored default are validated where
    // they are saved; the request's explicit field is the one id that arrived over the API.
    if !crate::claude_accounts::valid_id(&claude_account) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "Claude account ids are lowercase letters, digits and dashes, 1-40 characters",
        ));
    }
    if agent.needs_claude && app.claude_cred_for(Some(&claude_account)).is_none() {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("log in with Claude in Settings first (account '{claude_account}')"),
        ));
    }
    if let Err(e) = app.cfg.linux_binary("bin/colonizer-agentd") {
        return Err(client_error(StatusCode::BAD_REQUEST, &format!("{e:#}")));
    }
    // A tier the rule does not know would silently fall back to the rule's own choice, which is not
    // what an operator naming one asked for — refuse the launch instead.
    let model_tier = match req.model_tier.as_deref() {
        None => None,
        Some(raw) => match crate::routing::Tier::parse(raw) {
            Some(tier) => Some(tier.as_str().to_string()),
            None => {
                return Err(client_error(
                    StatusCode::BAD_REQUEST,
                    &format!("unknown model tier \"{raw}\"; use low, medium or high"),
                ));
            }
        },
    };
    let model_override = launch_model(&app, req.model_override.as_deref(), "model")?;
    let subagent_model_override = launch_model(&app, req.subagent_model_override.as_deref(), "subagent model")?;
    // Relating to the parent (`after`): by default the colony queues until the parent's pull request
    // merges and then starts from the fresh default branch; `stack: true` branches from the parent's
    // branch as soon as it is pushed instead. Whitespace is refused rather than read as nothing — an
    // operator who typed something there meant to relate. A parent that can never provide what the
    // mode needs is refused now, with the reason; one that has not gotten there yet is allowed, and
    // the colony queues for it — the queue, not this handler, does the waiting.
    let after = match req.after.as_deref() {
        None => None,
        Some(raw) => {
            let id = raw.trim();
            if id.is_empty() {
                return Err(client_error(StatusCode::BAD_REQUEST, "`after` names no colony to stack on"));
            }
            Some(id.to_string())
        }
    };
    let (parent, wait_for_parent) = match after {
        None => (None, false),
        Some(parent_id) => {
            let source = app.session(&parent_id).await;
            match source.as_ref() {
                None => {
                    return Err(client_error(
                        StatusCode::NOT_FOUND,
                        &format!("there is no colony `{parent_id}` to stack on"),
                    ));
                }
                Some(source) => {
                    // A stacked colony builds on its parent's branch, and a branch belongs to one
                    // repository: refused here, like every other un-stackable parent, rather than
                    // let the boot die later inside `create_worktree` with a raw git error.
                    if source.repo != repo {
                        return Err(client_error(
                            StatusCode::CONFLICT,
                            &format!(
                                "colony `{parent_id}` is on {}, not {repo}: a stacked colony builds \
                                 on its parent's branch, and a branch belongs to one repository",
                                source.repo
                            ),
                        ));
                    }
                    match restack::queue_decision(&parent_id, Some(source), req.stack) {
                        Stacked::Refuse(reason) => return Err(client_error(StatusCode::CONFLICT, &reason)),
                        Stacked::Wait => (Some(parent_id), true),
                        Stacked::Ready(_) => (Some(parent_id), false),
                    }
                }
            }
        }
    };
    // An epic is a planning container: a colony on it duplicates the colonies on its sub-issues.
    // Every launch on an issue — the cockpit's, the API's, a loop's or a hand-off's — comes through
    // here, so this one check covers them all. `allow_epic` skips it; a failed lookup lets the
    // launch through (epic.rs).
    if let Some(message) = crate::epic::launch_refusal(&crate::epic::gh_fetch(&app), &repo, req.issue, req.allow_epic).await {
        return Err(client_error(StatusCode::CONFLICT, &message));
    }
    // Issue #321: a launch that asks to `queue_behind_holder` waits for a local holder instead of
    // being refused. GitHub is checked either way: a conflict attributable to one of this
    // mothership's own colonies on the issue is the holder being queued behind, while a merged PR
    // or a claim another mothership holds refuses the launch as ever — cross-mothership queueing
    // is out of scope.
    let mut queue_behind_holder = false;
    if let (Some(issue), false) = (req.issue, req.allow_duplicate)
        && let Some(held) = issue_held_by(&app.sessions.read().await, &repo, issue)
    {
        if !req.queue_behind_holder {
            return Err(client_error(StatusCode::CONFLICT, &duplicate_message(&held, issue)));
        }
        queue_behind_holder = true;
    }
    // A second mothership shares no memory with this one, so the local guard above cannot see its
    // colonies: the issue itself carries the claim (see claims.rs). A failed lookup degrades to the
    // local guard rather than refusing the launch.
    if crate::claims::should_check_remote(req.issue, req.allow_duplicate)
        && let Some(issue) = req.issue
    {
        let checked = crate::claims::check_remote_claim(&app, &repo, issue).await;
        if let Err(e) = &checked {
            eprintln!("claims: remote duplicate check for #{issue} in {repo} failed ({e:#}); falling back to the local guard");
        }
        let conflict = if let Some(info) = crate::claims::remote_result_or_fallback(checked) {
            if queue_behind_holder {
                // The waiter tolerates only a claim of ours — the holder it queues behind, or
                // another colony on this mothership; `claim_wait_conflict` refuses the rest.
                let sessions = app.sessions.read().await;
                let ours: Vec<&str> = sessions
                    .iter()
                    .filter(|s| s.repo == repo && s.issue == Some(issue))
                    .map(|s| s.id.as_str())
                    .collect();
                crate::claims::claim_wait_conflict(Some(&info), issue, &ours)
            } else {
                Some(crate::claims::remote_conflict_message(&info, issue))
            }
        } else {
            None
        };
        if let Some(message) = conflict {
            return Err(client_error(StatusCode::CONFLICT, &message));
        }
    }
    let (owner, name) = repo.split_once('/').context("invalid repository name")?;
    // Past the limit a colony waits its turn rather than being refused; `run_queue` starts it later.
    let max_parallel = orgs::global_max_parallel(&modules) as usize;
    // Resolved before the admission lock: `org_settings` reads the orgs file with blocking IO.
    let org_settings = app.org_settings(owner);
    let org_limit = orgs::org_max_parallel(&org_settings);
    let repo_limit = crate::queue::repo_limit(&modules, &org_settings);

    // Issue #453: overlap-aware queueing — while a live same-repo colony still has touched files,
    // a newcomer queues behind it instead of developing against the same paths at once. Opt-in via
    // `serialize`: most launches would rather start immediately than have an unrelated colony's
    // edits hold them up. Stacked colonies already wait on their parent, so the scan is skipped
    // for them regardless.
    let queued_behind = if req.serialize == Some(true) && parent.is_none() {
        overlap_queue_target(&app.sessions.read().await, &repo).await
    } else {
        None
    };

    let id = short_id();
    let slug = match req.issue {
        Some(number) => format!("issue-{number}-{id}"),
        None => format!("session-{id}"),
    };
    let title = match (req.title.trim(), req.issue) {
        ("", None) => "Open session".to_string(),
        (title, _) => truncate(title, 300),
    };
    let now = Utc::now();
    let session = Session {
        id: id.clone(),
        repo: repo.clone(),
        org: owner.to_string(),
        issue: req.issue,
        issue_title: title,
        instructions: truncate(req.instructions.trim(), 20_000),
        status: SessionStatus::Starting, // decided by admission, just before the push
        branch: format!("colonizer/{slug}"),
        base: None,
        parent: parent.clone(),
        stack: req.stack,
        stack_fork: None,
        origin: req.origin.clone(),
        launched_by_token: scoped.as_ref().map(|t| t.id.clone()),
        worktree: app
            .cfg
            .data_dir
            .join("worktrees")
            .join(owner)
            .join(name)
            .join(&slug)
            .display()
            .to_string(),
        git_admin_dir: None,
        sandbox: format!("colonizer-{id}"),
        mesh: None,
        local_port: None,
        agent: agent.id.clone(),
        autopilot: req.autopilot.unwrap_or_else(|| autopilot_default(&app.agents, &modules)),
        autofix: req.autofix,
        automerge: req.automerge,
        fix_for: None,
        pr_url: None,
        merged_at: None,
        pr_opened_at: None,
        changed_paths: Vec::new(),
        ci_state: None,
        summary: None,
        publish_stage: None,
        // A fresh colony is starting or queued, never publishing: the flag is inert.
        publishing_holds_slot: false,
        needs_rebase: false,
        rebase_orphaned: false,
        queued_behind,
        // Set by admission when the launch waits for the issue's holder (issue #321).
        claim_wait: false,
        verify: Some(
            req.verify
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map_or_else(|| verify_default(&app.agents, &modules), str::to_string),
        ),
        verification: None,
        error: None,
        cost_usd: None,
        model_usage: None,
        model_tier,
        model_override,
        subagent_model_override,
        claude_account: Some(claude_account),
        model_routing: None,
        // Filled in at boot, once the colony's model settings resolve to actual providers.
        allowed_providers: None,
        sensitivity: None,
        routed_cost_usd: None,
        routed_tokens: None,
        host_disk_bytes: None,
        cleaned_up: false,
        keep_worktree: false,
        attention: None,
        suspended: None,
        agent_session: None,
        pending_answer: None,
        last_activity_at: None,
        boot_timing: None,
        boot_cpus: None,
        boot_memory: None,
        boot_image: None,
        app_slot: None,
        boot_attempt_started_at: None,
        created_at: now,
        updated_at: now,
    };
    let dir = app.session_dir(&id);
    tokio::fs::create_dir_all(dir.join("vm")).await?;
    tokio::fs::create_dir_all(dir.join("out")).await?;
    // The room check and the push share one write lock, so two launches colliding on the last free slot
    // cannot both take it. Counted before the push, so this colony is never waiting behind itself.
    // The duplicate-issue check is re-checked here too: the fast-path pre-check above reads under a
    // read lock, so two launches can both pass it before either inserts — the loser is refused with
    // the same 409 inside the lock, where check and insert are one atomic step. A scoped token's
    // caps are re-checked beside it for the same reason (`Admission`).
    let claimed = with_slot(
        &app.sessions,
        owner,
        &repo,
        max_parallel,
        org_limit,
        repo_limit,
        |sessions, room| {
            if let Some(token) = &scoped
                && let Some(reason) = crate::api_tokens::launch_cap_error(token, sessions, now)
            {
                return Admission::Capped(reason);
            }
            Admission::Claimed(Box::new(try_claim_session(
                sessions,
                room,
                session,
                &repo,
                req.issue,
                req.allow_duplicate,
                // The request's flag, not the pre-lock reading above: a holder appearing between the
                // two is `try_claim_session`'s call, and it refuses with the holder named if the
                // launch never asked to queue.
                req.queue_behind_holder,
                wait_for_parent,
            )))
        },
    )
    .await;
    let (session, queued, waiting) = match claimed {
        Admission::Claimed(claimed) => match *claimed {
            Ok(admitted) => admitted,
            Err(held) => {
                // The colony directories created above belong to a colony that never was; take them
                // back out, best effort, before refusing.
                let _ = tokio::fs::remove_dir_all(&dir).await;
                let issue = req.issue.unwrap_or_default();
                return Err(client_error(StatusCode::CONFLICT, &duplicate_message(&held, issue)));
            }
        },
        Admission::Capped(reason) => {
            // As above, the directories belong to a colony that never was.
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return Err(client_error(StatusCode::TOO_MANY_REQUESTS, &reason));
        }
    };
    if let Err(e) = app.persist_sessions().await {
        // Nothing has been reported as done yet — no boot, no log line, no reply — so the record
        // comes back out rather than leaving a colony only memory has heard of, and the caller
        // hears that the write never happened. The directories created above go with it; the
        // removal is best effort, and a failure there is reported, not swallowed.
        app.sessions.write().await.retain(|s| s.id != id);
        let e = e.context("could not save the new colony; nothing was created");
        app.storage_failed("save the session list", &e).await;
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                eprintln!(
                    "sessions: could not remove the unused colony directory {}: {e}",
                    dir.display()
                )
            }
        }
        return Err(e.into());
    }
    app.runtime(&id).await;
    // Heard about by the append-only spend journal now, before the colony does anything else: the
    // `launched` edge has to survive the cleanup or delete that will forget this record.
    spend::record_launched(&app, &session).await;
    // The launch claims the issue on GitHub itself, so a second mothership sees it: best effort in
    // the background, never failing the launch. A `claim_wait` waiter (issue #321) publishes
    // nothing — the holder's mark is the issue's claim until the waiter is promoted and takes it.
    if crate::claims::should_check_remote(session.issue, req.allow_duplicate)
        && !session.claim_wait
        && let Some(issue) = session.issue
    {
        crate::claims::spawn_publish(app.clone(), repo.clone(), issue, id.clone());
    }
    if queued {
        if let Some(holder) = session.queued_behind.as_deref() {
            let why = if session.claim_wait {
                format!("queued behind colony {holder}: it holds this issue, so this colony starts once the issue is its own")
            } else {
                format!(
                    "queued behind colony {holder}: it is working in the same repository, so this colony starts once it finishes"
                )
            };
            app.session_log(&id, "info", why).await;
        } else if let (true, Some(parent_id)) = (wait_for_parent, session.parent.as_deref()) {
            let why = if session.stack {
                format!("queued behind colony {parent_id}: it starts once that colony has pushed its branch")
            } else {
                format!("queued behind colony {parent_id}: it starts once that colony's pull request merges")
            };
            app.session_log(&id, "info", why).await;
        } else {
            let ahead = if waiting == 0 {
                String::new()
            } else {
                format!(", behind {waiting} already waiting")
            };
            let limits = crate::queue::limits_message(max_parallel, org_limit, repo_limit);
            app.session_log(&id, "info", format!("queued: {limits}{ahead}")).await;
        }
    } else {
        tokio::spawn(boot(app.clone(), id, false));
    }
    // Adoption by use: a colony started here is the operator's answer to "do you want this org?", so
    // the org counts as seen and no prompt later asks about one they are already working in. The
    // avatar from the pending sighting is recorded with it, so the org does not fall back to its
    // initial for the minutes until the next refresh re-records it.
    let pending_avatar = app.new_orgs.read().await.get(owner).cloned().flatten();
    app.mark_org_known(owner, pending_avatar.as_deref()).await;
    // A one-line summary of the task, written by a cheap model off the launch path (summaries.rs).
    tokio::spawn(crate::summaries::summarize_colony(app.clone(), session.id.clone()));
    Ok(Json(session))
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::sessions::tests::*;
    use tokio::sync::RwLock;

    /// Nothing configured means nothing automatic; the publish module's defaults flow into a session
    /// that did not choose, and a module-default automerge without autofix is nothing, while an
    /// explicit automerge on a fix colony counts on its own — that explicit value IS the hunter's
    /// decision, set at the fix colony's creation.
    #[tokio::test]
    async fn autofix_and_automerge_fall_back_to_the_module_and_count_explicit_choices() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        let s = app.session("abc").await.unwrap();
        assert!(!autofix_enabled(&app, &s).await);
        assert!(!automerge_enabled(&app, &s).await);

        // The publish module's defaults flow down when the session did not choose.
        {
            let mut modules = app.modules.write().await;
            modules.publish.settings.insert("autofix".into(), json!(true));
            modules.publish.settings.insert("automerge".into(), json!(true));
        }
        assert!(autofix_enabled(&app, &s).await);
        assert!(automerge_enabled(&app, &s).await);

        // Automerge only counts when autofix is enabled: with autofix off, there are no fix
        // colonies, so the module-default automerge is a setting nobody can have meant.
        {
            let mut modules = app.modules.write().await;
            modules.publish.settings.remove("autofix");
        }
        assert!(!autofix_enabled(&app, &s).await);
        assert!(!automerge_enabled(&app, &s).await);

        // A fix colony carries its automerge explicitly from its hunter, so it counts even though
        // its autofix is `Some(false)` — the flag that stops a fix colony cascading.
        let mut fix = colony("acme", SessionStatus::Idle);
        fix.autofix = Some(false);
        fix.automerge = Some(true);
        assert!(automerge_enabled(&app, &fix).await);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn overlap_queueing_holds_the_oldest_live_same_repo_colony_with_a_worktree() {
        fn holder(id: &str, repo: &str, status: SessionStatus, ago_secs: i64) -> Session {
            let mut s = colony("acme", status);
            s.id = id.into();
            s.repo = repo.into();
            s.git_admin_dir = Some("git".into());
            s.created_at = Utc::now() - chrono::Duration::seconds(ago_secs);
            s
        }
        let sessions = vec![
            holder("new", "acme/repo", SessionStatus::Running, 10),
            holder("old", "acme/repo", SessionStatus::Running, 100),
            holder("other-repo", "acme/other", SessionStatus::Running, 200),
            holder("published", "acme/repo", SessionStatus::PrOpened, 300),
            holder("queued", "acme/repo", SessionStatus::Queued, 400),
        ];
        assert_eq!(
            overlap_holder(&sessions, "acme/repo").as_deref(),
            Some("old"),
            "the oldest live holder wins"
        );
        assert_eq!(overlap_holder(&sessions, "acme/other").as_deref(), Some("other-repo"));
        assert_eq!(overlap_holder(&sessions, "acme/empty"), None, "no live worktree, no holder");
        // A live colony that never booted a worktree holds nothing back.
        let mut no_worktree = holder("wt-less", "acme/repo", SessionStatus::Running, 500);
        no_worktree.git_admin_dir = None;
        let mut only_quiet = vec![no_worktree];
        only_quiet.extend(sessions.into_iter().filter(|s| s.repo != "acme/repo" || !s.status.is_live()));
        assert_eq!(overlap_holder(&only_quiet, "acme/repo"), None);
    }

    #[test]
    fn an_overlap_queued_colony_stays_queued_until_its_holder_finishes() {
        fn queued_behind(holder: &str) -> Session {
            let mut s = colony("acme", SessionStatus::Starting);
            s.id = "new".into();
            s.repo = "acme/repo".into();
            s.queued_behind = Some(holder.into());
            s
        }
        let mut live_holder = colony("acme", SessionStatus::Running);
        live_holder.id = "holder".into();
        live_holder.repo = "acme/repo".into();
        // Room and no parent, yet queued: the live holder keeps it waiting, and the pointer stays.
        let mut sessions = vec![live_holder.clone()];
        let (admitted, queued, _) = try_claim_session(
            &mut sessions,
            true,
            queued_behind("holder"),
            "acme/repo",
            None,
            false,
            false,
            false,
        )
        .expect("no issue race");
        assert!(
            queued && admitted.status == SessionStatus::Queued,
            "held behind the live colony"
        );
        assert_eq!(admitted.queued_behind.as_deref(), Some("holder"));
        // The holder published: the same pointer releases at once, pointing nowhere stale.
        live_holder.status = SessionStatus::PrOpened;
        let mut sessions = vec![live_holder];
        let (admitted, queued, _) = try_claim_session(
            &mut sessions,
            true,
            queued_behind("holder"),
            "acme/repo",
            None,
            false,
            false,
            false,
        )
        .expect("no issue race");
        assert!(
            !queued && admitted.status == SessionStatus::Starting,
            "released once the holder finished"
        );
        assert_eq!(admitted.queued_behind, None);
    }

    /// A `create` request with nothing but the repo and, where the test names one, whether to opt
    /// into overlap-aware queueing.
    fn overlap_request(repo: &str, serialize: Option<bool>) -> Json<NewSession> {
        Json(NewSession {
            repo: repo.into(),
            issue: None,
            title: String::new(),
            instructions: String::new(),
            autopilot: None,
            verify: None,
            autofix: None,
            automerge: None,
            allow_duplicate: false,
            allow_epic: false,
            queue_behind_holder: false,
            model_tier: None,
            model_override: None,
            subagent_model_override: None,
            claude_account: None,
            after: None,
            stack: false,
            origin: None,
            serialize,
        })
    }

    /// Issue #453's review: overlap-aware queueing must be opt-in. The same live, touched-file
    /// holder is on the nest both times — only whether the request carries `serialize: true`
    /// decides whether the newcomer queues behind it.
    #[tokio::test]
    async fn overlap_queueing_only_applies_when_the_request_opts_in_with_serialize() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);

        // A live same-repo colony with a real worktree and an untracked file: `touched_files`
        // reads that as something touched via `git status --porcelain`, without needing a remote
        // or any commits.
        let worktree = root.join("holder-worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&worktree)
            .status()
            .expect("git init");
        std::fs::write(worktree.join("touched.txt"), "x").unwrap();

        let mut holder = colony("acme", SessionStatus::Running);
        holder.id = "holder".into();
        holder.repo = "acme/app".into();
        holder.git_admin_dir = Some("git".into());
        holder.worktree = worktree.to_string_lossy().to_string();
        holder.created_at = Utc::now() - chrono::Duration::seconds(100);
        app.sessions.write().await.push(holder);

        // No `serialize` at all: the default stays off, so the newcomer never even scans for an
        // overlap and starts unheld.
        let created = create(State(app.clone()), None, overlap_request("acme/app", None))
            .await
            .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
        assert_eq!(
            created.queued_behind, None,
            "overlap queueing is opt-in; a plain launch never scans for it"
        );

        // `serialize: false` reads the same as absent.
        let created = create(State(app.clone()), None, overlap_request("acme/app", Some(false)))
            .await
            .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
        assert_eq!(created.queued_behind, None, "an explicit false is still off");

        // `serialize: true`: the same live, touched-file colony now holds the newcomer behind it.
        let created = create(State(app.clone()), None, overlap_request("acme/app", Some(true)))
            .await
            .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
        assert_eq!(
            created.queued_behind.as_deref(),
            Some("holder"),
            "serialize: true asks to queue behind a live colony with touched files"
        );
        assert_eq!(
            created.status,
            SessionStatus::Queued,
            "queued behind the live holder rather than starting alongside it"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    /// A colony on the issue, in a state where a second one duplicates its work.
    fn on_issue(id: &str, issue: u64, status: SessionStatus) -> Session {
        let mut s = colony("acme", status);
        s.id = id.into();
        s.issue = Some(issue);
        s
    }

    #[test]
    fn a_live_or_published_colony_holds_its_issue_against_a_second_one() {
        // FindsYou-Work/app #7 drew four colonies and #13 drew two, because nothing asked.
        for status in [
            SessionStatus::Queued,
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::WaitingForAnswer,
            SessionStatus::Idle,
            SessionStatus::Publishing,
            SessionStatus::PrOpened,
        ] {
            let sessions = vec![on_issue("first", 7, status)];
            let held = issue_held_by(&sessions, "acme/repo", 7);
            assert_eq!(
                held.map(|s| s.id),
                Some("first".to_string()),
                "a colony that is {} still holds #7",
                status.as_str()
            );
        }
    }

    #[test]
    fn a_finished_colony_leaves_its_issue_free_to_try_again() {
        for status in [
            SessionStatus::Merged,
            SessionStatus::Closed,
            SessionStatus::NoChanges,
            SessionStatus::Stopped,
            SessionStatus::Failed,
        ] {
            let sessions = vec![on_issue("first", 7, status)];
            assert!(
                issue_held_by(&sessions, "acme/repo", 7).is_none(),
                "{} is done with #7, so a retry is not a duplicate",
                status.as_str()
            );
        }
    }

    #[test]
    fn the_hold_is_per_repository_and_per_issue() {
        let sessions = vec![
            on_issue("other-repo", 7, SessionStatus::Running),
            on_issue("other-issue", 8, SessionStatus::Running),
        ];
        let mut elsewhere = sessions.clone();
        elsewhere[0].repo = "acme/different".into();
        assert!(
            issue_held_by(&elsewhere, "acme/repo", 7).is_none(),
            "the same issue number in another repository is a different issue"
        );
        assert!(
            issue_held_by(&sessions, "acme/repo", 9).is_none(),
            "an untouched issue is free"
        );
        assert_eq!(
            issue_held_by(&sessions, "acme/repo", 7).map(|s| s.id),
            Some("other-repo".to_string()),
            "the colony on this repo's #7 is the one that holds it"
        );
    }

    #[test]
    fn a_stopped_or_failed_colony_frees_its_issue_for_an_explicit_retry() {
        // The sweep above covers every terminal state; stopped and failed get their own assertion
        // because they are the retries that matter — a run that died halfway, not one that shipped.
        for status in [SessionStatus::Stopped, SessionStatus::Failed] {
            let sessions = vec![on_issue("dead", 7, status)];
            assert!(
                issue_held_by(&sessions, "acme/repo", 7).is_none(),
                "{} is done with #7, so a retry is not a duplicate",
                status.as_str()
            );
            // And the atomic claim the handler admits with lets that retry through.
            let mut sessions = sessions;
            let mut retry = colony("acme", SessionStatus::Starting);
            retry.id = "retry".into();
            retry.issue = Some(7);
            let claimed = try_claim_session(&mut sessions, true, retry, "acme/repo", Some(7), false, false, false);
            assert!(claimed.is_ok(), "a retry after {} is admitted, not refused", status.as_str());
        }
    }

    #[test]
    fn allow_duplicate_bypasses_the_hold_that_blocks_a_second_claim() {
        // The holder here is still live, unlike the finished colonies above: without
        // `allow_duplicate` the claim is refused with the holder handed back; passing
        // `allow_duplicate: true` for the same issue is admitted anyway.
        let mut sessions = vec![on_issue("first", 7, SessionStatus::Running)];

        let mut blocked = colony("acme", SessionStatus::Starting);
        blocked.id = "blocked".into();
        blocked.issue = Some(7);
        let refused = try_claim_session(&mut sessions, true, blocked, "acme/repo", Some(7), false, false, false);
        assert!(
            matches!(&refused, Err(held) if held.id == "first"),
            "a live holder refuses a second claim without allow_duplicate"
        );
        assert_eq!(sessions.len(), 1, "the refused claim inserted nothing");

        let mut second = colony("acme", SessionStatus::Starting);
        second.id = "second".into();
        second.issue = Some(7);
        let admitted = try_claim_session(&mut sessions, true, second, "acme/repo", Some(7), true, false, false);
        assert!(
            admitted.is_ok(),
            "allow_duplicate lets a second colony start on an issue another still holds"
        );
        assert_eq!(sessions.len(), 2, "the admitted duplicate is inserted alongside the holder");
    }

    /// A `claim_wait` waiter for issue 7, queued behind `holder`, created `ago_secs` ago so the
    /// oldest-first tiebreak is deterministic.
    fn waiter_on_issue(id: &str, holder: &str, ago_secs: i64) -> Session {
        let mut s = colony("acme", SessionStatus::Queued);
        s.id = id.into();
        s.issue = Some(7);
        s.claim_wait = true;
        s.queued_behind = Some(holder.into());
        s.created_at = Utc::now() - chrono::Duration::seconds(ago_secs);
        s
    }

    #[test]
    fn a_launch_asked_to_queue_waits_behind_the_holder_instead_of_being_refused() {
        // Issue #321: the polite third option — the default refuses, `allow_duplicate` duplicates,
        // `queue_behind_holder` admits the launch as a waiter behind the holder, even with a slot
        // free: its turn comes when the queue gets to it, not before.
        let mut sessions = vec![on_issue("holder", 7, SessionStatus::Running)];
        let mut polite = colony("acme", SessionStatus::Starting);
        polite.id = "polite".into();
        polite.issue = Some(7);
        let (admitted, queued, _) = try_claim_session(&mut sessions, true, polite, "acme/repo", Some(7), false, true, false)
            .expect("a waiter is admitted, not refused");
        assert!(
            queued && admitted.status == SessionStatus::Queued,
            "queued even with a free slot"
        );
        assert!(admitted.claim_wait, "the colony is a waiter for its issue");
        assert_eq!(admitted.queued_behind.as_deref(), Some("holder"), "queued behind the holder");
        assert_eq!(sessions.len(), 2, "the waiter is inserted");
        // And the holder still holds the issue: the waiter claims nothing while it waits.
        assert_eq!(
            issue_held_by(&sessions, "acme/repo", 7).map(|s| s.id),
            Some("holder".to_string()),
            "the waiter does not take the hold over by waiting"
        );
    }

    #[test]
    fn the_default_still_refuses_and_allow_duplicate_still_wins_over_queueing() {
        // The two existing launches are unchanged, and `allow_duplicate` takes precedence when a
        // request sets both: it starts now, it does not wait its turn.
        let mut sessions = vec![on_issue("holder", 7, SessionStatus::Running)];
        let mut plain = colony("acme", SessionStatus::Starting);
        plain.id = "plain".into();
        plain.issue = Some(7);
        assert!(
            matches!(
                try_claim_session(&mut sessions, true, plain, "acme/repo", Some(7), false, false, false),
                Err(held) if held.id == "holder"
            ),
            "a launch that did not ask to queue is refused as ever"
        );
        let mut duplicate = colony("acme", SessionStatus::Starting);
        duplicate.id = "duplicate".into();
        duplicate.issue = Some(7);
        let (admitted, queued, _) = try_claim_session(&mut sessions, true, duplicate, "acme/repo", Some(7), true, true, false)
            .expect("allow_duplicate bypasses the hold");
        assert!(!queued && admitted.status == SessionStatus::Starting, "starts, not waits");
        assert!(!admitted.claim_wait, "a duplicate is no waiter");
    }

    #[test]
    fn with_only_waiters_left_the_oldest_one_holds_the_issue() {
        // The holder is gone; queue order decides. A fresh launch is refused naming the oldest
        // waiter — starting ahead of it would jump the queue, and waving the newcomer through
        // would duplicate the first waiter's work the moment its turn came.
        let sessions = vec![
            waiter_on_issue("first", "holder", 100),
            waiter_on_issue("second", "holder", 50),
        ];
        let held = issue_held_by(&sessions, "acme/repo", 7).expect("a waiter holds the issue once the holder is gone");
        assert_eq!(held.id, "first", "the oldest waiter holds it");
        let message = duplicate_message(&held, 7);
        assert!(
            message.contains("colony first is already on #7") && message.contains("allow_duplicate"),
            "{message}"
        );
        // A polite launch queues behind that same waiter, and the atomic claim refuses the
        // default one for it.
        let mut sessions = sessions;
        let mut fresh = colony("acme", SessionStatus::Starting);
        fresh.id = "fresh".into();
        fresh.issue = Some(7);
        assert!(
            matches!(
                try_claim_session(&mut sessions, true, fresh.clone(), "acme/repo", Some(7), false, false, false),
                Err(held) if held.id == "first"
            ),
            "a fresh default launch is refused naming the oldest waiter"
        );
        let (admitted, _, _) = try_claim_session(&mut sessions, true, fresh, "acme/repo", Some(7), false, true, false)
            .expect("a polite launch waits");
        assert_eq!(admitted.queued_behind.as_deref(), Some("first"), "behind the oldest waiter");
    }

    #[test]
    fn a_real_holder_outranks_the_waiters_however_young_it_is() {
        // The first non-waiter holder wins whatever the creation order: waiters only take over
        // once there is no holder left at all.
        let mut holder = on_issue("holder", 7, SessionStatus::Running);
        holder.created_at = Utc::now();
        let sessions = vec![waiter_on_issue("old-waiter", "gone", 200), holder];
        assert_eq!(
            issue_held_by(&sessions, "acme/repo", 7).map(|s| s.id),
            Some("holder".to_string()),
            "the holder keeps the issue; the waiter keeps waiting"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[allow(clippy::result_large_err)]
    async fn two_simultaneous_claims_on_one_issue_let_exactly_one_through() {
        // The TOCTOU window this guards: two launches both passing the read-locked pre-check before
        // either inserts. Both collide here inside the write lock instead, through the same
        // `try_claim_session` the handler admits with — one is admitted, the other gets its holder.
        let sessions = std::sync::Arc::new(RwLock::new(Vec::new()));
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let mut tasks = Vec::new();
        for i in 0..2 {
            let (sessions, barrier) = (sessions.clone(), barrier.clone());
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                let mut fresh = colony("acme", SessionStatus::Starting);
                fresh.id = format!("racer-{i}");
                fresh.issue = Some(7);
                with_slot(&sessions, "acme", "acme/repo", 8, None, 8, |guard, room| {
                    try_claim_session(guard, room, fresh, "acme/repo", Some(7), false, false, false)
                })
                .await
            }));
        }
        let mut admitted = 0;
        let mut refused = 0;
        for task in tasks {
            match task.await.expect("claim task joined") {
                Ok(_) => admitted += 1,
                Err(_) => refused += 1,
            }
        }
        assert_eq!(admitted, 1, "exactly one racer is admitted");
        assert_eq!(refused, 1, "the other gets the holder back for its 409");
        let done = sessions.read().await;
        assert_eq!(done.len(), 1, "the loser inserted nothing");
        let held = issue_held_by(&done, "acme/repo", 7).expect("the winner holds #7");
        assert!(
            held.id == "racer-0" || held.id == "racer-1",
            "the holder is the admitted racer, not a stranger: {}",
            held.id
        );
        // And the loser's 409 reads the way the handler's does.
        let message = duplicate_message(&held, 7);
        assert!(
            message.contains(&format!("colony {} is already on #7", held.id)) && message.contains("allow_duplicate"),
            "{message}"
        );
    }

    /// A throwaway App whose config switches the `acme` workspace off, the way an old install's
    /// `orgs.json` plus one settings save leaves it.
    async fn app_with_org_switched_off(id: &str, status: SessionStatus) -> (Shared, PathBuf) {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = test_app(&root);
        std::fs::create_dir_all(app.cfg.config_dir.clone()).unwrap();
        std::fs::write(app.cfg.config_dir.join("orgs.json"), r#"{"acme": {"enabled": false}}"#).unwrap();
        let mut s = colony("acme", status);
        s.id = id.to_string();
        s.git_admin_dir = Some("git".into());
        app.sessions.write().await.push(s);
        tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
        (app, root)
    }

    #[tokio::test]
    async fn a_switched_off_org_refuses_new_colonies_and_names_the_way_back_on() {
        let (app, root) = app_with_org_switched_off("kept", SessionStatus::Stopped).await;
        let err = create(
            State(app.clone()),
            None,
            Json(NewSession {
                repo: "acme/app".into(),
                issue: None,
                title: String::new(),
                instructions: String::new(),
                autopilot: None,
                verify: None,
                autofix: None,
                automerge: None,
                allow_duplicate: false,
                allow_epic: false,
                queue_behind_holder: false,
                model_tier: None,
                model_override: None,
                subagent_model_override: None,
                claude_account: None,
                after: None,
                stack: false,
                origin: None,
                serialize: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let message = err.1.to_string();
        assert!(message.contains("acme"), "{message}");
        assert!(message.contains("switched off"), "{message}");
        assert!(
            message.contains("org settings"),
            "the message says what to do about it: {message}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn colonies_of_a_switched_off_org_stay_listed_and_resume() {
        let (app, root) = app_with_org_switched_off("kept", SessionStatus::Stopped).await;
        let listed = list(State(app.clone()), None).await.0;
        let kept = listed.iter().find(|s| s.id == "kept").unwrap();
        assert_eq!(kept.org, "acme", "the colony is still in the list");
        // The real resume path, not just its gate: the org's switch does not make `resume` refuse
        // the colony — it is claimed and handed to a fresh boot like any other. The boot itself
        // never runs here: the spawned task is dropped with the one-thread test runtime before it
        // is polled, so nothing reaches for GitHub or a microVM.
        let resumed = resume(State(app.clone()), Path("kept".into()))
            .await
            .unwrap_or_else(|e| panic!("resume refused a colony of a switched-off org: {:#}", e.1))
            .0;
        assert_eq!(resumed.id, "kept");
        assert_eq!(
            resumed.status,
            SessionStatus::Starting,
            "the resume claimed the colony and started a boot"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn starting_a_colony_marks_its_org_known_so_the_operator_is_never_asked_about_it() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        // The smallest install `create` insists on: an agent module matching the configured provider
        // and a guest binary that claims to be an ELF.
        let assets = root.join("assets");
        let dir = assets.join("modules/agents/claude-code");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("module.json"), r#"{"id":"claude-code","entry":["run"]}"#).unwrap();
        std::fs::create_dir_all(assets.join("bin")).unwrap();
        std::fs::write(assets.join("bin/colonizer-agentd"), b"\x7fELF padding").unwrap();
        let agent = AgentModule {
            id: "claude-code".into(),
            name: "Claude Code".into(),
            description: String::new(),
            dir,
            entry: vec!["run".into()],
            needs_claude: false,
            schema: json!({}),
            egress: None,
            resume_dir: None,
        };
        let app = crate::tests::test_app_with_agents(&root, vec![agent], |cfg| cfg.assets = Some(assets));
        // The org is still awaiting an answer when the colony starts, sighting and avatar both.
        *app.new_orgs.write().await =
            std::collections::BTreeMap::from([("acme".to_string(), Some("https://a/acme.png".to_string()))]);

        let created = create(
            State(app.clone()),
            None,
            Json(NewSession {
                repo: "acme/app".into(),
                issue: None,
                title: String::new(),
                instructions: String::new(),
                autopilot: None,
                verify: None,
                autofix: None,
                automerge: None,
                allow_duplicate: false,
                allow_epic: false,
                queue_behind_holder: false,
                model_tier: None,
                model_override: None,
                subagent_model_override: None,
                claude_account: None,
                after: None,
                stack: false,
                origin: None,
                serialize: None,
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
        assert_eq!(created.org, "acme");
        assert_eq!(
            app.known_orgs().unwrap().get("acme").cloned(),
            Some(crate::orgs::KnownOrg {
                avatar_url: Some("https://a/acme.png".into()),
            }),
            "working in an org is an answer, and the sighting's avatar is recorded with it; the prompt \
             must never ask about it later"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    // -- stacking (create with `after`) ----------------------------------------------------------

    /// A `create` request with nothing but the repo and, where the test names one, the parent.
    /// Unstacked unless the test says otherwise: the default queues behind the parent's merge.
    fn stack_request(repo: &str, after: Option<String>, stack: bool) -> Json<NewSession> {
        Json(NewSession {
            repo: repo.into(),
            issue: None,
            title: String::new(),
            instructions: String::new(),
            autopilot: None,
            verify: None,
            autofix: None,
            automerge: None,
            allow_duplicate: false,
            allow_epic: false,
            queue_behind_holder: false,
            model_tier: None,
            model_override: None,
            subagent_model_override: None,
            claude_account: None,
            after,
            stack,
            origin: None,
            serialize: None,
        })
    }

    #[tokio::test]
    async fn a_colony_asked_to_stack_on_another_queues_until_that_one_pushes() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        let mut parent = colony("acme", SessionStatus::Running);
        parent.id = "parent".into();
        parent.branch = "colonizer/issue-1-parent".into();
        parent.repo = "acme/app".into();
        app.sessions.write().await.push(parent);

        let created = create(
            State(app.clone()),
            None,
            stack_request("acme/app", Some("parent".into()), true),
        )
        .await
        .unwrap_or_else(|e| panic!("create refused a stacked colony: {:#}", e.1));
        assert_eq!(
            created.parent.as_deref(),
            Some("parent"),
            "the colony records what it is stacked on"
        );
        assert_eq!(
            created.status,
            SessionStatus::Queued,
            "the parent has not pushed a branch, so the colony queues even though a slot is free"
        );
        assert_eq!(
            created.base, None,
            "the base is the boot's business, resolved fresh when the wait ends"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A parent colony for the queue-by-default tests below: open issue, same repository.
    async fn parent_on(app: &Shared, status: SessionStatus) {
        let mut parent = colony("acme", status);
        parent.id = "parent".into();
        parent.branch = "colonizer/issue-1-parent".into();
        parent.repo = "acme/app".into();
        app.sessions.write().await.push(parent);
    }

    #[tokio::test]
    async fn by_default_a_colony_behind_an_open_pull_request_queues_for_the_merge() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        parent_on(&app, SessionStatus::PrOpened).await;

        let created = create(
            State(app.clone()),
            None,
            stack_request("acme/app", Some("parent".into()), false),
        )
        .await
        .unwrap_or_else(|e| panic!("create refused a queued colony: {:#}", e.1));
        assert_eq!(created.parent.as_deref(), Some("parent"));
        assert!(!created.stack, "queueing, not stacking, is the default");
        assert_eq!(
            created.status,
            SessionStatus::Queued,
            "the parent's pull request is still open, so the colony queues even though a slot is free"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_explicit_stack_starts_from_the_open_pull_request() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        parent_on(&app, SessionStatus::PrOpened).await;

        let created = create(
            State(app.clone()),
            None,
            stack_request("acme/app", Some("parent".into()), true),
        )
        .await
        .unwrap_or_else(|e| panic!("create refused a stacked colony: {:#}", e.1));
        assert_eq!(created.parent.as_deref(), Some("parent"));
        assert!(created.stack);
        assert_eq!(
            created.status,
            SessionStatus::Starting,
            "the parent's branch is pushed, so an explicit stack starts at once"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn by_default_a_colony_behind_a_merged_parent_starts_at_once() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        parent_on(&app, SessionStatus::Merged).await;

        let created = create(
            State(app.clone()),
            None,
            stack_request("acme/app", Some("parent".into()), false),
        )
        .await
        .unwrap_or_else(|e| panic!("create refused a queued colony: {:#}", e.1));
        assert_eq!(
            created.status,
            SessionStatus::Starting,
            "the parent's work is already merged, so there is nothing to wait for"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn by_default_a_colony_behind_a_closed_parent_is_refused_naming_the_stack_flag() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        parent_on(&app, SessionStatus::Closed).await;

        let err = create(
            State(app.clone()),
            None,
            stack_request("acme/app", Some("parent".into()), false),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        let message = err.1.to_string();
        assert!(message.contains("parent"), "{message}");
        assert!(
            message.contains("stack: true"),
            "the refusal says how to stack anyway: {message}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn create_refuses_to_stack_on_a_colony_that_can_never_lend_a_branch() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        let mut dead = colony("acme", SessionStatus::Failed);
        dead.id = "dead".into();
        dead.branch = "colonizer/issue-1-dead".into();
        dead.repo = "acme/app".into();
        app.sessions.write().await.push(dead);

        let err = create(State(app.clone()), None, stack_request("acme/app", Some("dead".into()), true))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT, "a refusal, like the duplicate-issue one");
        let message = err.1.to_string();
        assert!(message.contains("dead"), "{message}");
        assert!(message.contains("failed"), "it says which reason applies: {message}");
        let sessions = app.sessions.read().await;
        assert_eq!(sessions.len(), 1, "nothing was created");
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stacking_on_a_colony_that_does_not_exist_is_refused_as_a_404_naming_it() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        let err = create(
            State(app.clone()),
            None,
            stack_request("acme/app", Some("ghost".into()), true),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::NOT_FOUND,
            "the same answer asking for an unknown colony gets"
        );
        let message = err.1.to_string();
        assert!(message.contains("ghost") && message.contains("no colony"), "{message}");
        assert!(app.sessions.read().await.is_empty(), "nothing was created");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_after_of_nothing_but_whitespace_is_refused_not_read_as_absent() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        let err = create(State(app.clone()), None, stack_request("acme/app", Some("   ".into()), true))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST, "the request names nothing stackable");
        assert!(err.1.to_string().contains("`after`"), "{}", err.1);
        assert!(
            app.sessions.read().await.is_empty(),
            "silently starting unstacked would branch from the wrong place without a word"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stacking_on_a_colony_of_another_repository_is_refused_naming_both() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        let mut parent = colony("acme", SessionStatus::PrOpened);
        parent.id = "parent".into();
        parent.branch = "colonizer/issue-1-parent".into();
        parent.repo = "acme/app".into();
        app.sessions.write().await.push(parent);

        let err = create(
            State(app.clone()),
            None,
            stack_request("acme/other", Some("parent".into()), true),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT, "a refusal at create, like the other stack ones");
        let message = err.1.to_string();
        assert!(message.contains("acme/app"), "the parent's repository is named: {message}");
        assert!(message.contains("acme/other"), "and so is this one's: {message}");
        let sessions = app.sessions.read().await;
        assert_eq!(sessions.len(), 1, "nothing was created to fail a boot later");
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }
}
