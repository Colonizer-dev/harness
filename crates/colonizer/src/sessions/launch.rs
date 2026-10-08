//! Starting a colony: `POST /api/sessions` (`create`), its admission rules (issue holds, overlap
//! queueing, stacking, org switches) and the settings a launch resolves.

use super::*;

#[derive(Deserialize, Default)]
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
    /// The supply-chain target this colony is for (issue #673): a package and the advisory it was
    /// launched to fix. A live colony for the same target refuses a second one, like an issue hold;
    /// `allow_duplicate` overrides.
    #[serde(default)]
    pub supply_chain: Option<crate::supersede::SupplyChainTarget>,
    /// Every package/advisory pair a supply-chain loop colony is dispatched to fix (issue #832). Each
    /// claims the work the way `supply_chain` does; an empty advisory claims every advisory of its
    /// package (a yanked or outdated release has none).
    #[serde(default)]
    pub supply_chain_targets: Vec<crate::supersede::SupplyChainTarget>,
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
    /// Pin this colony to a named fleet member (issue #688): its id or its display name, or this
    /// member's own. Cross-member execution is not built yet, so a pin to a peer is refused with the
    /// reason; omitting it lets placement pick a member and record why, without moving the colony.
    #[serde(default)]
    pub host: Option<String>,
    /// Opt in to overlap-aware queueing: queue behind a live same-repo colony that's already
    /// touching files, instead of developing against the same paths at once. Off by default —
    /// most callers would rather start immediately than have an unrelated colony's edits hold
    /// them up. See `overlap_queue_target`.
    #[serde(default)]
    pub serialize: Option<bool>,
    /// A hand-off's seed (issue #738): the rendered, redacted conversation and the branch the colony
    /// starts from. Never part of the JSON body — the route that builds it (`handoff_in`) is in-process
    /// — so it is skipped by serde and `Default` leaves it empty for every ordinary launch.
    #[serde(skip)]
    pub(crate) handoff: Option<crate::handoff::Seed>,
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

// The duplicate rules live in one place (issue #832); the queue and the claim reconcile read the
// issue hold through these names.
pub(crate) use crate::duplicates::{holds_issue, issue_held_by};

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
/// `Err` carries the refusal naming the holder, and nothing is inserted. The rules are the shared
/// duplicates service's (`duplicates::check`), asked about the work the colony record is on.
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
) -> Result<(Session, bool, usize), crate::duplicates::Refusal> {
    // Issue #321: a launch that asked to wait its turn is not refused when the issue is held — it
    // is admitted as a `claim_wait` waiter behind whoever effectively holds it, however full or
    // empty the queue. The holder's mark on GitHub stays; the waiter never claims over it.
    // Issue #673: the supply-chain hold is re-checked beside the issue hold under the same lock, and
    // is never a queue.
    let work = crate::duplicates::Work {
        repo: repo.to_string(),
        issue,
        ..crate::duplicates::Work::of(&session)
    };
    let mut queued_for_holder = false;
    match crate::duplicates::check(sessions, &[], &work, allow_duplicate, queue_behind_holder) {
        crate::duplicates::Verdict::Refuse(refusal) => return Err(refusal),
        crate::duplicates::Verdict::Queue(holder) => {
            queued_for_holder = true;
            session.claim_wait = true;
            session.queued_behind = Some(holder);
        }
        crate::duplicates::Verdict::Allow => {}
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
    Claimed(Box<Result<(Session, bool, usize), crate::duplicates::Refusal>>),
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
    // The module's `requires` preflight (issue #633): a colony whose agent binary neither the
    // harness stages nor the colony image carries is refused before anything is created. The image
    // here is the configured stack's (detection needs the worktree, made at boot); the stock
    // presets all answer the same way, so the verdict matches the boot-time check.
    let sandbox_schema = schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let stack = orgs::effective_stack(&modules, &sandbox_schema, &app.org_settings(owner));
    let staged = crate::modules::harness_staged_binaries(&app.cfg);
    if let Err(problem) = crate::modules::check_requires(agent, &colony_image(&app.agents, &modules, &stack), &staged) {
        return Err(client_error(StatusCode::BAD_REQUEST, &problem));
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
    // A supply-chain target with a side missing would hold against other half-named targets it was
    // never really for: refused here, normalized (trimmed, lowercased) into the record.
    let supply_chain = match req.supply_chain.as_ref() {
        None => None,
        Some(target) => {
            if target.package.trim().is_empty() || target.advisory.trim().is_empty() {
                return Err(client_error(
                    StatusCode::BAD_REQUEST,
                    "a supply-chain target names both a package and an advisory",
                ));
            }
            Some(crate::supersede::SupplyChainTarget::new(&target.package, &target.advisory))
        }
    };
    // A loop's targets name a package each; the advisory may be empty (it then claims them all).
    if req.supply_chain_targets.iter().any(|t| t.package.trim().is_empty()) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "every supply-chain target names a package",
        ));
    }
    let supply_chain_targets: Vec<crate::supersede::SupplyChainTarget> = req
        .supply_chain_targets
        .iter()
        .map(|t| crate::supersede::SupplyChainTarget::new(&t.package, &t.advisory))
        .collect();
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
    // A maintainer can opt a repo out of Colonizer entirely: a `.colonizer-ignore` file, a
    // `colonizer: ignore` label on the issue, or `enabled = false` under `[colonizer]` in
    // `.colonizer/config.toml` (ignore.rs). The repo's own owner can launch there anyway, so the
    // viewer is only fetched when a signal was actually found.
    if let Some(message) = crate::ignore::launch_refusal(&crate::epic::gh_fetch(&app), &repo, req.issue).await {
        let overridden = crate::github::viewer(&app)
            .await
            .ok()
            .and_then(|v| v["login"].as_str().map(|login| crate::ignore::is_owner(&repo, login)))
            .unwrap_or(false);
        if !overridden {
            return Err(client_error(StatusCode::CONFLICT, &message));
        }
    }
    // The fleet as placement candidates (issue #688): this member plus the last-known row of every
    // peer the fleet view has polled — cached data only, never a fresh poll. With no fleet this is
    // just this member, and placement below changes nothing but the reason it records.
    let (local, peers) = crate::fleet::placement_candidates(&app).await;
    // The peers the fleet reads as unreachable: a claim their host left is still refused, but the
    // refusal can say their colony is not being re-run here (issue #688).
    let unreachable_ids: Vec<String> = peers.iter().filter(|c| !c.online).map(|c| c.id.clone()).collect();
    // Issue #832: one answer to "is this work already being done" for every launch path — the
    // issue hold (or, issue #321, a wait behind a local holder when `queue_behind_holder` asks),
    // the supply-chain hold (issue #673), and the claim a second mothership left on GitHub. The
    // authoritative local re-check runs in the admission lock (`try_claim_session`).
    let work = crate::duplicates::Work::requested(&repo, &req);
    if let Err(refusal) =
        crate::duplicates::check_launch(&app, &work, req.allow_duplicate, req.queue_behind_holder, &unreachable_ids).await
    {
        return Err(refusal.into_error());
    }
    // Placement (issue #688): a pure policy whose verdict is recorded on the colony. Nothing here
    // executes remotely (issue #1252), so an unpinned choice of a peer is recorded and the colony runs
    // here, a pin to a peer is refused rather than silently moved, and an unknown pin is a bad request.
    let host_pin = req.host.as_deref().map(str::trim).filter(|host| !host.is_empty());
    let placement_reason = match crate::placement::place(host_pin, &local, &peers) {
        Ok(chosen) if chosen.local => chosen.reason,
        // A peer has room, but nothing launches on another member yet: say so, run here.
        Ok(chosen) if host_pin.is_none() => {
            format!("{}; running on another member is not built yet (#1252)", chosen.reason)
        }
        // Pinned to a peer that can take the colony: refused; remote execution is not built.
        Ok(chosen) => {
            return Err(client_error(
                StatusCode::CONFLICT,
                &format!(
                    "pinned to {}: running a colony on another member is not built yet (#1252)",
                    chosen.host_name
                ),
            ));
        }
        Err(unknown @ crate::placement::Refusal::UnknownHost { .. }) => {
            return Err(client_error(StatusCode::BAD_REQUEST, &unknown.to_string()));
        }
        // A pinned member that cannot take the colony: refused, never moved elsewhere.
        Err(refusal) => return Err(client_error(StatusCode::CONFLICT, &refusal.to_string())),
    };
    let (owner, name) = repo.split_once('/').context("invalid repository name")?;
    // Past the limit a colony waits its turn rather than being refused; `run_queue` starts it later.
    let max_parallel = crate::capacity::max_parallel(&app, &modules).await;
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
        // A hand-off colony starts from the branch its transcript was recorded on (issue #738); an
        // ordinary launch starts from the repository default, resolved at boot.
        base: req.handoff.as_ref().and_then(|seed| seed.branch.clone()),
        parent: parent.clone(),
        stack: req.stack,
        stack_fork: None,
        origin: req.origin.clone(),
        auto_note: None,
        launched_by_token: scoped.as_ref().map(|t| t.id.clone()),
        placement: Some(placement_reason),
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
        preview_port: None,
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
        // A fresh colony has no failure behind it, unseen or otherwise.
        unseen_failure: false,
        queued_behind,
        blocked_reason: None,
        pr_rewrite_nudged: false,
        // Set by admission when the launch waits for the issue's holder (issue #321).
        claim_wait: false,
        priority: None,
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
        allowed_models: None,
        sensitivity: None,
        model_substitutions: Vec::new(),
        routed_cost_usd: None,
        routed_tokens: None,
        host_disk_bytes: None,
        cleaned_up: false,
        keep_worktree: false,
        attention: None,
        suspended: None,
        run_end_cause: None,
        parked: None,
        hold_resumes: 0,
        hold_cause: None,
        hold_cause_repeats: 0,
        verify_fix_rounds: 0,
        auto_fixes: Vec::new(),
        provider_retries: 0,
        agent_session: None,
        pending_answer: None,
        switch_note: None,
        resume_note: None,
        secret_fix_rounds: 0,
        push_conflict_rounds: 0,
        publish_resume_pending: false,
        prewarm: None,
        supply_chain,
        supply_chain_targets,
        superseded: None,
        // A fresh colony has no suspension behind it for the boot's `restore` to name (issue #700).
        was_suspended: false,
        last_activity_at: None,
        boot_timing: None,
        boot_cpus: None,
        boot_memory: None,
        boot_image: None,
        app_slot: None,
        boot_attempt_started_at: None,
        failure_class: None,
        boot_retries: 0,
        retry_at: None,
        created_at: now,
        updated_at: now,
    };
    let dir = app.session_dir(&id);
    tokio::fs::create_dir_all(dir.join("vm")).await?;
    tokio::fs::create_dir_all(dir.join("out")).await?;
    // A hand-off's rendered conversation is written beside the colony's own directories — never inside
    // the guest-writable `transcripts/` mount — and read once, by the boot, into the first prompt
    // (issue #738). Written before admission so a refused launch's `remove_dir_all` takes it back out.
    if let Some(seed) = &req.handoff
        && let Err(e) = tokio::fs::write(crate::handoff::seed_path(&dir), seed.text.as_bytes()).await
    {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Err(e.into());
    }
    // The room check and the push share one write lock, so two launches colliding on the last free slot
    // cannot both take it. Counted before the push, so this colony is never waiting behind itself.
    // The duplicate-issue check is re-checked here too: the fast-path pre-check above reads under a
    // read lock, so two launches can both pass it before either inserts — the loser is refused with
    // the same 409 inside the lock, where check and insert are one atomic step. A scoped token's
    // caps are re-checked beside it for the same reason (`Admission`).
    // Issue #880: while the mothership drains for an update or restart a fresh launch queues
    // instead of booting, like a colony admitted by the queue's own gate. The drain is read again
    // inside the lock, beside `room`: a drain that begins between here and the claim must not let a
    // boot slip through.
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
                room && !app.drain.draining(),
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
            Err(refusal) => {
                // The colony directories created above belong to a colony that never was; take them
                // back out, best effort, before refusing.
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return Err(refusal.into_error());
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
            // Issue #880: while the mothership drains for an update or restart, say so rather than
            // naming limits that are not what is holding the colony.
            let why = if app.drain.draining() {
                "the mothership is draining for an update or restart, so no colony starts yet".to_string()
            } else {
                let limits = crate::queue::limits_message(max_parallel, org_limit, repo_limit);
                format!("queued: {limits}")
            };
            app.session_log(&id, "info", format!("{why}{ahead}")).await;
        }
    } else {
        tokio::spawn(boot(app.clone(), id, false, None));
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
mod tests;
