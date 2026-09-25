//! Colony boot: the async sequence that turns a queued colony into a running microVM — resolve
//! the base and issue, prepare the worktree, assemble prompt, mounts and secrets, size and start
//! the sandbox, wait for the agent daemon — and the failure path that reaps an orphaned microVM.

use crate::{
    App, CLAUDE_API_HOST, Shared,
    config::{ModulesConfig, setting, setting_str, setting_u64},
    egress,
    events::start_link,
    github,
    lifecycle::teardown_vm,
    memory,
    modules::schema_for,
    orgs, providers, resolve_guest_claude_bin,
    sandbox::{self, BootSpec, Mount, Secret},
    sessions::{
        AGENTD_NOT_READY, AGENTD_PORT, MeshInfo, Session, SessionLogger, SessionStatus, agent_env, agent_needs_node, agentd_http,
        colony_image, findings_enabled,
    },
    stack,
    util::{append_line, random_token, truncate, write_private},
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::{Map, Value, json};
use std::{path::PathBuf, time::Duration};

/// The stack a colony boots and the line that explains the choice. An explicit preset or an org pin
/// is the operator's decision and needs no explaining, so it comes back with no message; the two
/// detection outcomes both carry one, because a wrong guess has to be diagnosable from the session
/// log alone. Pure, so the rule is testable apart from the directory read and the logging that
/// [`resolve_stack`] wraps around it — the way `queue::has_room` and `watchdog::decide` are written.
fn stack_choice(configured: &str, detected: Option<&crate::presets::Detected>) -> (String, Option<String>) {
    if configured != crate::presets::AUTO {
        return (configured.to_string(), None);
    }
    match detected {
        Some(found) => {
            let image = crate::presets::find(found.stack).map(|p| p.image).unwrap_or_default();
            (
                found.stack.to_string(),
                Some(format!("detected {} from {}, using {}", found.stack, found.marker, image)),
            )
        }
        None => (
            crate::presets::AUTO_FALLBACK.to_string(),
            Some(format!(
                "no stack marker found in the repository; using the {} stack",
                crate::presets::AUTO_FALLBACK
            )),
        ),
    }
}

/// The stack one colony boots: the configured one, or, when that is `auto`, what the repository's own
/// marker files say. The answer is always concrete — `auto` never comes back out of this — and both
/// detection branches log, because a wrong guess has to be diagnosable from the session log alone,
/// without re-running anything.
async fn resolve_stack(
    modules: &ModulesConfig,
    schema: &Value,
    org: &orgs::OrgSettings,
    worktree: &std::path::Path,
    log: &SessionLogger,
) -> String {
    let configured = orgs::effective_stack(modules, schema, org);
    // Only an `auto` install pays for the directory listing.
    let detected = (configured == crate::presets::AUTO)
        .then(|| crate::presets::detect_in(worktree))
        .flatten();
    let (stack, message) = stack_choice(&configured, detected.as_ref());
    if let Some(message) = message {
        log.info(message).await;
    }
    stack
}

pub(crate) async fn boot(app: Shared, id: String, resume: bool) {
    if let Err(e) = boot_inner(&app, &id, resume).await {
        let message = format!("{e:#}");
        let Some(s) = app.session(&id).await else { return };
        if s.status != SessionStatus::Starting {
            // The status moved out from under this task — usually a stop that ran before
            // `sandbox::boot` created the microVM, so the stop's `msb rm` removed nothing
            // and this boot's teardown below is the only reaper left for the orphan. The
            // old code returned here assuming the stop handler had cleaned up, leaking a
            // running `colonizer-{id}` no reaper ever removes. Re-check under the
            // lifecycle lock, which the stop handler holds across its claim+teardown: if
            // the colony is back to `Starting` a newer boot/resume claimed it (both share
            // the deterministic sandbox name, so tearing down here could kill its fresh
            // VM), and if it went live a stale boot must not touch the live VM — return
            // in both cases. Otherwise the colony is still not live (Stopped/Failed): reap
            // the orphan (`msb rm --force` on a missing name no-ops).
            let lifecycle = app.session_lock(&id).await;
            let _guard = lifecycle.lock().await;
            let Some(s) = app.session(&id).await else { return };
            if !stale_boot_needs_teardown(s.status) {
                return;
            }
            teardown_vm(&app, &s).await;
            return;
        }
        app.session_log(&id, "error", format!("session failed to start: {message}"))
            .await;
        teardown_vm(&app, &s).await;
        let mut attention = None;
        app.update_session(&id, |s| {
            s.status = SessionStatus::Failed;
            s.error = Some(truncate(&message, 2000));
            attention = s.clear_attention();
        })
        .await;
        app.note_cleared_attention(&id, attention).await;
        // A colony that never got going frees the issue for a retry, on GitHub as well as locally.
        if let Some(s) = app.session(&id).await {
            crate::claims::spawn_release_if_needed(app.clone(), &s);
        }
    }
}

/// Whether a boot that failed after its colony left `Starting` must still reap the microVM: only
/// when the colony is still not live (Stopped/Failed). A colony back to `Starting` was claimed by
/// a newer boot/resume sharing the deterministic sandbox name, and a live one (Running/Idle/…)
/// owns a VM this stale task must not touch.
pub(crate) fn stale_boot_needs_teardown(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Stopped | SessionStatus::Failed)
}

async fn ensure_starting(app: &App, id: &str) -> Result<Session> {
    match app.session(id).await {
        Some(s) if s.status == SessionStatus::Starting => Ok(s),
        _ => bail!("session was stopped while starting"),
    }
}

/// Closes a boot phase and publishes the breakdown so far, so a colony still `starting` shows which
/// phases it has got through, and a boot that fails keeps them. Written without `total_ms`, which
/// only the finished boot carries.
async fn mark_phase(app: &App, id: &str, timing: &mut crate::timing::Phases, name: &str) {
    timing.mark(name);
    let breakdown = timing.progress_json();
    app.update_session(id, |x| x.boot_timing = Some(breakdown)).await;
}

/// The repository's default branch, with access failures worded the way boots report them.
/// Transient blips ride out the boot retry budget first; only a lasting or permanent failure
/// reaches the caller.
async fn default_base(app: &Shared, repo: &str, log: &SessionLogger, started_at: Option<u64>) -> Result<String> {
    let label = format!("resolving the default branch of {repo}");
    let branch = github::with_boot_retry(&label, Some(log), started_at, || github::default_branch(app, repo)).await;
    match branch {
        Ok(base) => Ok(base),
        Err(e) => Err(github::access_error(app, repo, e).await),
    }
}

/// The network fence a colony boots with, resolved from the egress policy (#303): the harness's
/// own port-scoped infrastructure allows — never the broad `host` profile, which allows every
/// host-loopback port and would let the untrusted colony agent drive the cockpit API
/// (127.0.0.1:7878) or any other loopback service (#375) — plus what the operator's policy adds,
/// plus the non-overridable deny set, all compiled by `egress::compile` in msb's evaluation
/// order. The infrastructure allows: when the mesh is on, the WireGuard direct-path rules (passed
/// in already-awaited because `direct_path_rules` is async and shells out) and the headscale
/// control port; when any model route exists, the provider gateway port; and in allowlist mode,
/// the TLS-edge secret hosts on 443, which the `public` profile would otherwise have covered and
/// whose traffic must work by construction. Answers the profiles, the rules, and the record a
/// boot files at `<session dir>/egress.json`.
pub(crate) fn colony_network(
    mesh: Option<(Vec<String>, u16)>,
    routes: &providers::ColonyRoutes,
    gateway: std::net::SocketAddr,
    resolved: &egress::Resolved,
    tls_hosts: &[String],
) -> (Vec<String>, Vec<String>, egress::Record) {
    let mut infra = mesh
        .map(|(direct_path, control)| {
            let mut infra = direct_path;
            infra.push(format!("allow@host:tcp:{control}"));
            infra
        })
        .unwrap_or_default();
    if !routes.routes.is_empty() {
        infra.push(format!("allow@host:tcp:{}", gateway.port()));
    }
    if resolved.policy.mode == egress::EgressMode::Allowlist {
        // One rule per host even when two secrets name the same one: duplicates are harmless to
        // msb but would make the record read as two fences.
        let mut seen: Vec<String> = Vec::new();
        for host in tls_hosts {
            if !seen.contains(host) {
                seen.push(host.clone());
                infra.push(format!("allow@{host}:tcp:443"));
            }
        }
    }
    let compiled = egress::compile(&resolved.policy, &infra);
    let record = egress::record(resolved, &compiled, github::unix_now());
    (compiled.profiles, compiled.rules, record)
}

/// The hosts msb swaps secrets for on TLS: the credential and colony secrets a boot hands over.
/// In allowlist mode these are allowed by construction on 443 — a colony with an injected secret
/// that cannot reach its host would be a boot that only looks fenced.
fn tls_edge_hosts(secrets: &[sandbox::Secret]) -> Vec<String> {
    secrets.iter().flat_map(|secret| secret.hosts.iter().cloned()).collect()
}

async fn boot_inner(app: &Shared, id: &str, resume: bool) -> Result<()> {
    let log = app.logger(id);
    // Phases close in order and partition the boot; see crates/colonizer/src/timing.rs.
    let mut timing = crate::timing::Phases::new();
    let s = ensure_starting(app, id).await?;
    // An empty breakdown up front, so a boot that stops before its first phase still reads as a boot that stopped.
    app.update_session(id, |x| x.boot_timing = Some(timing.progress_json())).await;
    // The retry clock starts before the first pre-worktree step and is persisted, so a harness
    // restart resumes the same budget instead of starting a new one: `lifecycle::recover`
    // re-queues a boot that died before its worktree existed, carrying this stamp with it.
    let boot_started_at = match s.boot_attempt_started_at {
        Some(started_at) => Some(started_at),
        None => {
            let started_at = github::unix_now();
            app.update_session(id, |x| x.boot_attempt_started_at = Some(started_at)).await;
            Some(started_at)
        }
    };
    let modules = app.modules.read().await.clone();
    let agent = app
        .agents
        .iter()
        .find(|a| a.id == s.agent)
        .cloned()
        .context("agent module is not installed")?;

    let issue = match s.issue {
        Some(number) => {
            let label = format!("fetching issue {}#{number}", s.repo);
            log.info(label.clone()).await;
            let fetched = github::with_boot_retry(&label, Some(&log), boot_started_at, || {
                github::fetch_issue(app, &s.repo, number)
            })
            .await;
            match fetched {
                Ok(issue) => Some(issue),
                Err(e) => return Err(github::access_error(app, &s.repo, e).await),
            }
        }
        None => None,
    };
    // A resumed colony keeps the base it started from; its branch already exists on top of it. A
    // colony stacked on another one takes the parent's branch, resolved now — so a long wait ends on
    // a fresh answer rather than the one given at create time. The queue only starts a stacked
    // colony once the parent's branch exists, so `Wait` and `Refuse` here mean the parent moved
    // under the queue's feet: fail the boot rather than silently branch from the wrong place. The
    // decision is `stack::boot_base`, pure and tested; here is only its I/O.
    let parent = match s.parent.as_deref() {
        Some(parent_id) => app.session(parent_id).await,
        None => None,
    };
    let mut stacked_on: Option<String> = None;
    let base = match stack::boot_base(
        s.base.clone().filter(|_| resume),
        s.parent.as_deref(),
        parent.as_ref(),
        s.stack,
    ) {
        stack::BootBase::Kept(base) => {
            // What a stacked colony kept is the parent's branch it started from; the prompt says so.
            if s.parent.is_some() {
                stacked_on = Some(base.clone());
            }
            base
        }
        stack::BootBase::Parent { colony, branch } => {
            log.info(format!("stacked on colony {colony}: branching from its branch {branch}"))
                .await;
            stacked_on = Some(branch.clone());
            branch
        }
        stack::BootBase::Default => default_base(app, &s.repo, &log, boot_started_at).await?,
        stack::BootBase::Wait { colony } => {
            bail!("the colony `{colony}` this one is stacked on has no branch to build on yet")
        }
        stack::BootBase::Refuse(reason) => bail!("{reason}"),
    };
    let title = issue.as_ref().and_then(|i| i["title"].as_str()).map(String::from);
    app.update_session(id, |x| {
        if let Some(title) = title {
            x.issue_title = title;
        }
        x.base = Some(base.clone());
    })
    .await;
    ensure_starting(app, id).await?;

    mark_phase(app, id, &mut timing, "issue").await;

    let bare = app.bare_repo(&s.repo);
    let wt = PathBuf::from(&s.worktree);
    let admin = if resume {
        // The worktree and branch outlive the microVM, so a resumed colony picks them up as they are.
        log.info(format!("resuming on the kept worktree, branch {}", s.branch)).await;
        PathBuf::from(s.git_admin_dir.as_deref().context("this colony has no worktree to resume")?)
    } else {
        let lock = app.repo_lock(&s.repo).await;
        let _guard = lock.lock().await;
        github::with_boot_retry(
            &format!("syncing the local clone of {}", s.repo),
            Some(&log),
            boot_started_at,
            || github::sync_repo(app, &s.repo, &bare, &log),
        )
        .await?;
        log.info(format!("creating worktree on branch {} from origin/{base}", s.branch))
            .await;
        github::with_boot_retry(
            &format!("creating the worktree for branch {}", s.branch),
            Some(&log),
            boot_started_at,
            || github::create_worktree(app, &bare, &wt, &s.branch, &base),
        )
        .await?
    };
    // A stacked child branched from `origin/<base>` just now; record the sha before later prunes
    // delete the parent's ref, so the publish-time restack knows which commits are the child's own.
    // Best effort: without it the restack falls back to the merge-base while the ref survives.
    let stack_fork = if !resume && stacked_on.is_some() {
        github::fork_sha(app, &bare, &base).await.ok()
    } else {
        None
    };
    app.update_session(id, |x| {
        x.git_admin_dir = Some(admin.display().to_string());
        if stack_fork.is_some() {
            x.stack_fork = stack_fork;
        }
        // The first durable artifact is down; the retry clock has nothing left to budget.
        x.boot_attempt_started_at = None;
    })
    .await;
    let s = ensure_starting(app, id).await?;

    // The worktree exists by now — freshly checked out or kept from before — so a configured `auto`
    // reads the repository this colony will actually work in, and the choice is on the session log
    // before the VM boots. `org_settings` reads the orgs file with blocking IO; it moves up with the
    // resolution because the org's own pin decides what detection is even asked.
    let org_settings = app.org_settings(&s.org);
    let sandbox_schema = schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let stack = resolve_stack(&modules, &sandbox_schema, &org_settings, &wt, &log).await;

    mark_phase(app, id, &mut timing, "git").await;

    let dir = app.session_dir(id);
    let vm_dir = dir.join("vm");
    let out_dir = dir.join("out");
    // Cloned out of the lock before the awaits below: `touched_files` shells out to git per
    // sibling, and the sessions guard must not be held across that.
    let colonies = app.sessions.read().await.clone();
    let touched = github::touched_files(app, &colonies, &s).await;
    let siblings = github::siblings_of(&colonies, &s, &touched);
    let mut prompt = github::build_prompt(&s, issue.as_ref(), &base, resume, &siblings, stacked_on.as_deref());
    // Colony secrets in scope: named in the prompt (never their values) and handed to msb below,
    // which substitutes each one only on TLS to its hosts.
    let colony_secrets = crate::colony_secrets::for_colony(&app.cfg.config_dir, &s.repo);
    prompt.push_str(&crate::colony_secrets::prompt_block(
        &colony_secrets.iter().map(|(meta, _)| meta).collect::<Vec<_>>(),
    ));
    write_private(&vm_dir.join("token"), random_token().as_bytes())?;
    // The colony's own agent module's settings (issue #201): an org may run its colonies on a
    // module other than the install's, whose settings are not this module's to read.
    let mut agent_choice = orgs::effective_agent_for(&modules, &org_settings, &agent.id);
    // A mapping colony draws with archify whatever its org has switched on (maps.rs).
    if s.origin.as_deref() == Some(crate::maps::MAP_ORIGIN) {
        let plugins = agent_choice
            .settings
            .get("plugins")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let with = crate::maps::with_archify(plugins);
        agent_choice.settings.insert("plugins".into(), Value::String(with));
        // A map is one bounded job — read the repository, write one validated JSON file — so it
        // runs as a single agent on a fast model rather than an orchestrator at full effort
        // handing every grep to a subagent and waiting on it.
        for (key, value) in crate::maps::MAP_AGENT_SETTINGS {
            agent_choice.settings.insert((*key).into(), Value::String((*value).into()));
        }
    }
    let mut runner_env = agent_env(&agent, &agent_choice);
    // Per-task model routing (routing.rs): the tier comes from the issue in front of the colony
    // unless the operator named one at launch, and the tier's model replaces the module's own when
    // that tier has one. Read off the effective settings, so an org override is honoured.
    let route_settings = crate::routing::RoutingSettings {
        enabled: setting(&agent_choice, &agent.schema, "route_per_task")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        chosen: s.model_tier.as_deref().and_then(crate::routing::Tier::parse),
    };
    let task_labels: Vec<String> = issue
        .as_ref()
        .and_then(|i| i["labels"].as_array())
        .map(|ls| {
            ls.iter()
                .map(|l| l["name"].as_str().unwrap_or_default().trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    let body_text = issue
        .as_ref()
        .and_then(|i| i["body"].as_str())
        .unwrap_or(s.instructions.as_str());
    let mut task_signals = crate::routing::signals(
        &s.issue_title,
        body_text,
        &task_labels,
        // The RESOLVED stack, not the configured preset: `auto` is not a preset the harness
        // knows, so asking `find` about it would call every auto-detected repository unknown
        // and refuse it the cheapest tier — including the ones detection identified exactly.
        crate::presets::find(&stack).is_some(),
    );
    // Sensitivity (issue #472): how strict the class is that this task's named paths fall into
    // decides which providers the gateway will let the colony reach. Recorded here and enforced
    // there — not filtered into `allowed_providers` — so a task that turns out to touch a
    // restricted path it did not name up front is not locked out at boot.
    let sensitivity_config = crate::sensitivity::SensitivityConfig::load(&wt);
    let named_paths = crate::routing::paths_in(&s.issue_title, body_text);
    let sensitivity = crate::sensitivity::classify_paths(named_paths, &sensitivity_config);
    // Jev shadow mode (jev.rs): an optional external classifier's second opinion, fetched here in
    // the async boot path — never inside `routing::decide`, which stays synchronous and pure. Off by
    // default, and a silent no-op without both the setting and a `JEV_API_KEY` secret: it is recorded
    // for later comparison and never changes the tier a colony runs on.
    let jev_enabled = setting(&agent_choice, &agent.schema, "jev_shadow_mode")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    task_signals.jev = crate::jev::shadow_opinion(jev_enabled, &s.issue_title, &task_labels, &task_signals).await;
    let tier_decision = crate::routing::decide(&route_settings, &task_signals);
    let model_low = setting_str(&agent_choice, &agent.schema, "model_low");
    let model = setting_str(&agent_choice, &agent.schema, "model");
    let model_high = setting_str(&agent_choice, &agent.schema, "model_high");
    let routed_model = crate::routing::model_for(tier_decision.tier, &model_low, &model, &model_high);
    let module_model = runner_env
        .get("COLONIZER_MODEL")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // Cost gate (issue #470): routing down makes the cheaper tier reload the task's context at
    // input price, which sometimes costs more than running the task directly would have. Opt-in —
    // until real per-colony token volumes are measured (#469) the operator supplies the estimate,
    // and an all-zero one skips the gate entirely, so every colony that has not opted in boots
    // exactly as before. Only the rule's own low pick is gated: medium is the module's model and
    // high an escalation, neither with a context-reload tradeoff, and an operator's explicit tier
    // is an instruction a cost estimate must never second-guess.
    let mut cost_record = Value::Null;
    let mut gated = false;
    let mut effective_model = routed_model.to_string();
    if tier_decision.tier == crate::routing::Tier::Low {
        let tokens = crate::routing::RoutingCostTokens {
            context_tokens: setting_u64(&agent_choice, &agent.schema, "route_cost_context_tokens"),
            output_tokens: setting_u64(&agent_choice, &agent.schema, "route_cost_output_tokens"),
            reread_tokens: setting_u64(&agent_choice, &agent.schema, "route_cost_reread_tokens"),
        };
        if tokens != crate::routing::RoutingCostTokens::default() {
            let provider_list = app.providers();
            let direct_pricing = providers::pricing_for(&provider_list, &module_model);
            let routed_pricing = providers::pricing_for(&provider_list, routed_model);
            if let (Some(direct_pricing), Some(routed_pricing)) = (direct_pricing, routed_pricing) {
                let estimate = crate::routing::estimate_cost(direct_pricing, routed_pricing, tokens);
                gated = setting(&agent_choice, &agent.schema, "route_cost_gate")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                    && !estimate.worth_routing()
                    && tier_decision.source == crate::routing::Source::Rule;
                if gated {
                    effective_model = module_model.clone();
                }
                cost_record = json!({
                    "tokens": tokens,
                    "direct_usd": estimate.direct_usd,
                    "routed_usd": estimate.routed_usd,
                    "worth_routing": estimate.worth_routing(),
                    "gated": gated,
                });
            }
        }
    }
    if !effective_model.is_empty() {
        runner_env.insert("COLONIZER_MODEL".into(), Value::String(effective_model.clone()));
    }
    // A model the operator named at launch beats routing: it is what they asked this colony to run.
    if let Some(model) = s.model_override.as_deref() {
        runner_env.insert("COLONIZER_MODEL".into(), Value::String(model.into()));
    }
    if let Some(model) = s.subagent_model_override.as_deref() {
        runner_env.insert("COLONIZER_SUBAGENT_MODEL".into(), Value::String(model.into()));
    }
    // The runner reads only COLONIZER_MODEL: the tier settings are for the mothership's provider
    // tally, and leaving them in would make the boot probe check providers this colony is not using.
    runner_env.remove("COLONIZER_MODEL_LOW");
    runner_env.remove("COLONIZER_MODEL_HIGH");
    let model_changed = !effective_model.is_empty() && effective_model != module_model;
    let mut message = format!("model routing: {}", tier_decision.reason);
    if model_changed {
        message.push_str(&format!("; running on {effective_model}"));
    }
    if gated {
        message.push_str("; cost gate: routing down would cost more, so the colony stays on its module model");
    }
    if tier_decision.misroute() {
        message.push_str(&format!("; misroute: the rule wants {}", tier_decision.rule.as_str()));
    }
    log.info(message).await;
    // Only the strict class gets a line: for every other class the gateway would do exactly what it
    // would have done anyway, and a log that mostly repeats that is noise.
    if sensitivity == crate::sensitivity::Sensitivity::Restricted {
        log.info(
            "sensitivity: this task's paths classify restricted; the gateway will refuse any provider not marked trusted in providers.json".to_string(),
        )
        .await;
    }
    // A shadow opinion that disagrees with the rule is worth a low-key note for later promotion
    // analysis; it never blocks boot or looks like an error.
    if tier_decision.jev_agrees() == Some(false) {
        log.info(format!(
            "jev shadow mode: the second opinion says {} where the rule says {}",
            tier_decision.jev.as_ref().map(|jev| jev.tier.as_str()).unwrap_or("?"),
            tier_decision.rule.as_str()
        ))
        .await;
    }
    let record = json!({
        "tier": tier_decision.tier,
        "rule": tier_decision.rule,
        "source": tier_decision.source,
        "score": tier_decision.score,
        "reason": tier_decision.reason,
        "model": if model_changed { json!(effective_model) } else { Value::Null },
        "misroute": tier_decision.misroute(),
        "signals": task_signals,
        "jev": tier_decision.jev,
        "cost": cost_record,
        "sensitivity": sensitivity.as_str(),
    });
    app.update_session(id, |x| {
        x.model_routing = Some(record.clone());
        x.sensitivity = Some(sensitivity.as_str().to_string());
    })
    .await;
    let line = json!({
        "ts": Utc::now(),
        "kind": "decision",
        "session": id,
        "repo": s.repo.clone(),
        "issue": s.issue,
        "decision": record,
    })
    .to_string();
    // A lost routing record is a lost measurement, not a failed boot: say so and carry on.
    if let Err(e) = append_line(&app.routing_file(), &line).await {
        log.error(format!("could not save the routing decision: {e:#}")).await;
    }
    let gateway_token = random_token();
    write_private(&app.gateway_token_file(id), gateway_token.as_bytes())?;
    let routing = providers::colony_routes(app, &gateway_token);
    if !routing.routes.is_empty() {
        runner_env.insert(
            "COLONIZER_MODEL_ROUTES".into(),
            Value::String(serde_json::to_string(&routing.routes)?),
        );
    }
    // A `<provider>/` prefix nobody configured is a typo'd route, not a Claude model: the runner would
    // only warn and send those requests to Anthropic (router.mjs), so the boot refuses instead — here,
    // after tier substitution, so only the models this colony will actually run are checked.
    if let Some((value, prefix)) = routing.unrouted_provider(&runner_env) {
        bail!("model setting '{value}' names provider '{prefix}', which is not configured");
    }
    let used = routing.used(&runner_env);
    // Recorded on the session because the gateway needs it long after boot: every proxied call is
    // checked against this set (issue #409), so a colony's token opens only these providers.
    app.update_session(id, |x| {
        x.allowed_providers = Some(used.iter().map(|p| p.id.clone()).collect())
    })
    .await;
    let probes = futures_util::future::join_all(used.iter().map(|p| crate::gateway::probe_cached(app, p))).await;
    for (provider, health) in used.iter().zip(probes) {
        if health["reachable"] != true {
            let then = match &provider.fallback_model {
                Some(model) => format!("its requests will fall back to {model}"),
                None => "its requests will fail until it is back (set a fallback model to use Claude instead)".into(),
            };
            let error = health["error"].as_str().unwrap_or("unknown error");
            app.session_log(
                id,
                "warn",
                format!("model provider {} is unreachable ({error}); {then}", provider.id),
            )
            .await;
        }
    }
    mark_phase(app, id, &mut timing, "providers").await;

    if findings_enabled(app, &modules) {
        runner_env.insert("COLONIZER_FINDINGS".into(), Value::String("true".into()));
    }
    // A loop's colony gets loop_stop, and loop_next when the loop is self-paced (loops.rs).
    if let Some(self_paced) = crate::loops::colony_self_paced(app, s.origin.as_deref()).await {
        runner_env.insert("COLONIZER_LOOP".into(), Value::String("true".into()));
        runner_env.insert("COLONIZER_LOOP_SELF_PACED".into(), Value::String(self_paced.to_string()));
    }
    let memory_on = orgs::effective_memory_enabled(&modules, &org_settings);
    if memory_on {
        runner_env.insert("COLONIZER_MEMORY_DIR".into(), Value::String("/colonizer/memory".into()));
    }
    // What the colony can and cannot run is part of the agent's brief (runner.mjs), so it names the
    // image this colony actually boots — the resolved stack's, not the configured one — or an agent
    // in a repository detected as Rust would brief itself for a Node machine.
    runner_env.insert(
        "COLONIZER_IMAGE".into(),
        Value::String(colony_image(&app.agents, &modules, &stack)),
    );

    // The private mesh needs the three vendored binaries. Without them a colony is reached on a
    // loopback port rather than failing to boot.
    let mesh_on = modules.mesh_enabled() && app.cfg.assets.as_deref().is_some_and(crate::mesh::binaries_present);
    if modules.mesh_enabled() && !mesh_on {
        app.session_log(
            id,
            "warn",
            "the private mesh is unavailable on this platform; using a loopback port".into(),
        )
        .await;
    }
    let mut env: Vec<(String, String)> = vec![
        ("IS_SANDBOX".into(), "1".into()),
        ("DISABLE_AUTOUPDATER".into(), "1".into()),
        ("GIT_DIR".into(), admin.display().to_string()),
        ("GIT_WORK_TREE".into(), "/workspace".into()),
        ("GIT_INDEX_FILE".into(), "/tmp/colonizer-git-index".into()),
        ("GIT_CONFIG_COUNT".into(), "1".into()),
        ("GIT_CONFIG_KEY_0".into(), "safe.directory".into()),
        ("GIT_CONFIG_VALUE_0".into(), "*".into()),
    ];
    let mut mounts = vec![
        Mount {
            source: wt.clone(),
            target: "/workspace".into(),
            read_only: false,
        },
        Mount {
            source: bare.clone(),
            target: bare.display().to_string(),
            read_only: true,
        },
        Mount {
            source: vm_dir.clone(),
            target: "/colonizer".into(),
            read_only: true,
        },
        Mount {
            source: out_dir,
            target: "/harness/out".into(),
            read_only: false,
        },
        Mount {
            source: app.cfg.linux_binary("bin/colonizer-agentd")?,
            target: "/opt/colonizer/bin/colonizer-agentd".into(),
            read_only: true,
        },
        Mount {
            source: agent.dir.clone(),
            target: "/opt/colonizer/agent".into(),
            read_only: true,
        },
    ];
    // Claude Code plugin directories, mounted read-only from the mothership.
    //
    // Outside /workspace on purpose: publish runs `git add -A`, so a plugin
    // staged inside the worktree would be committed into the pull request.
    // Read-only so one colony cannot edit what the next one loads — the same
    // reason memory scopes are read-only.
    //
    // A setting names a directory, never a path: it is resolved under the
    // mothership's plugins folder, so it cannot reach an arbitrary host path.
    let plugin_names = crate::plugins::parse_list(
        runner_env
            .get("COLONIZER_PLUGIN_DIRS")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    if !plugin_names.is_empty() {
        let mut targets = Vec::new();
        let mut resolved = Vec::new();
        for name in &plugin_names {
            // The operator's data directory first, then what shipped with the app; the same resolution the
            // skillset list in Settings shows (plugins.rs).
            let source = crate::plugins::resolve(&app.cfg, name)?;
            resolved.push((name.as_str(), source.clone()));
            if let Some(vendored) = crate::plugins::shadowed_vendored(&app.cfg, name) {
                log.info(format!(
                    "skillset {name:?}: the local copy at {} shadows the vendored one at {}",
                    source.display(),
                    vendored.display()
                ))
                .await;
            }
            let target = format!("/opt/colonizer/plugins/{name}");
            mounts.push(Mount {
                source,
                target: target.clone(),
                read_only: true,
            });
            targets.push(target);
        }
        // Two packs answering to the same skill name are ambiguous by
        // construction (docs/skill-packs.md): bail naming both packs.
        crate::plugins::check_skill_uniqueness(&resolved)?;
        // The runner only ever sees in-VM paths, never the mothership's.
        runner_env.insert("COLONIZER_PLUGIN_DIRS".into(), Value::String(targets.join(",")));
        // Belt and braces for ECC, whose hooks are dropped at staging time. Its
        // own flag is checked only after a hook process has already spawned, so
        // this is the second line of defence, not the first.
        env.push(("ECC_HOOKS_ENABLED".into(), "false".into()));
        log.info(format!(
            "loading {} plugin director{}",
            targets.len(),
            if targets.len() == 1 { "y" } else { "ies" }
        ))
        .await;
    }
    if let Some(assets) = app.cfg.assets.as_ref() {
        // Remembered whether or not plugins are on: an update must not remove
        // the directory any of this colony's mounts resolved through.
        let slot = assets.display().to_string();
        app.update_session(id, |x| x.app_slot = Some(slot)).await;
    }

    // Token savings (docs/protocol.md): what a switched-on setting needs inside the colony. When the
    // install lacks it, the setting is off for this colony and the log says why: saving tokens is never
    // the reason a colony doesn't start.
    let switched_on = |env: &Map<String, Value>, key: &str| env.get(key).and_then(Value::as_str) == Some("true");
    if switched_on(&runner_env, "COLONIZER_RTK") {
        match app.cfg.linux_binary("bin/rtk") {
            Ok(source) => mounts.push(Mount {
                source,
                target: "/opt/colonizer/bin/rtk".into(),
                read_only: true,
            }),
            Err(e) => {
                runner_env.remove("COLONIZER_RTK");
                log.info(format!(
                    "compact command output is switched on, but rtk can't be used ({e:#}); running without it"
                ))
                .await;
            }
        }
    }
    if switched_on(&runner_env, "COLONIZER_HEADROOM") {
        match crate::headroom::installed(app) {
            Some(source) => mounts.push(Mount {
                source,
                target: "/opt/colonizer/headroom".into(),
                read_only: true,
            }),
            None => {
                runner_env.remove("COLONIZER_HEADROOM");
                log.info("Headroom is switched on, but its bundle isn't downloaded yet (Settings → Agent starts the download); running without it").await;
            }
        }
    }
    if switched_on(&runner_env, "COLONIZER_CAVEMAN") {
        match app
            .cfg
            .asset("vendor/caveman/SKILL.md")
            .and_then(|_| app.cfg.asset("vendor/caveman"))
        {
            Ok(source) => mounts.push(Mount {
                source,
                target: "/opt/colonizer/caveman".into(),
                read_only: true,
            }),
            Err(_) => {
                runner_env.remove("COLONIZER_CAVEMAN");
                log.info("terse replies are switched on, but caveman's ruleset isn't installed (scripts/install.sh stages it); running without it").await;
            }
        }
    }
    // Remembered for the secrets below: msb keeps the value host-side and the guest env only
    // holds a placeholder, swapped at the TLS edge for the listed hosts.
    let mut jev_key: Option<String> = None;
    if switched_on(&runner_env, "COLONIZER_JEV_COMPACTION") {
        match jev_compaction(
            app.cfg
                .asset("vendor/fast-jev-compaction/hooks/hooks.json")
                .and_then(|_| app.cfg.asset("vendor/fast-jev-compaction"))
                .ok(),
            crate::jev::api_key(),
        ) {
            Ok((source, key)) => {
                mounts.push(Mount {
                    source,
                    target: "/opt/colonizer/jev-compaction".into(),
                    read_only: true,
                });
                jev_key = Some(key);
            }
            Err(reason) => {
                runner_env.remove("COLONIZER_JEV_COMPACTION");
                log.info(reason).await;
            }
        }
    }

    // Written only now, after the last change to `runner_env`: the plugin paths and the token-savings
    // switches above are decided here, and a session.json written earlier carried a plugin's bare name
    // instead of its in-VM path, and switches the install could not honour.
    let session_json = json!({
        "session_id": id,
        "workspace": "/workspace",
        "listen": format!("0.0.0.0:{AGENTD_PORT}"),
        "agent": {"module": agent.id, "command": agent.vm_command(), "env": runner_env},
        "initial_prompt": prompt,
    });
    std::fs::write(vm_dir.join("session.json"), serde_json::to_vec_pretty(&session_json)?)?;
    std::fs::write(vm_dir.join("boot.sh"), BOOT_SCRIPT)?;

    if memory_on && memory::uses_mem0(app).await {
        // mem0's notes are written into the session directory, which is already the colony's
        // read-only /colonizer, so there is nothing to mount and nothing of mem0's inside.
        let root = vm_dir.join("memory");
        let task = memory::task_query(&s.issue_title, issue.as_ref(), &s.instructions);
        match memory::materialize_mem0(app, &root, &s.org, &s.repo, &task).await {
            Ok(m) => {
                let order = if m.ranked { ", most relevant to this task first" } else { "" };
                app.session_log(id, "info", format!("shared memory: {} notes from mem0{order}", m.notes))
                    .await;
            }
            Err(e) => {
                app.session_log(
                    id,
                    "warn",
                    format!("shared memory from mem0 is unavailable ({e:#}); this colony starts without it"),
                )
                .await;
                memory::write_empty_scopes(&root, &s.org, &s.repo)?;
            }
        }
    } else if memory_on {
        for (scope, key) in [("global", String::new()), ("org", s.org.clone()), ("repo", s.repo.clone())] {
            // Mount points must exist inside the read-only /colonizer mount.
            std::fs::create_dir_all(vm_dir.join("memory").join(scope))?;
            let source = app.memory.ensure_scope(scope, &key)?;
            mounts.push(Mount {
                source,
                target: format!("/colonizer/memory/{scope}"),
                read_only: true,
            });
        }
    }
    // The vendored node runtime, mounted read-only only when the agent's in-VM command
    // starts with `node` (the claude-code module's `["node","runner.mjs"]` entry, rewritten by
    // `vm_command()` to `["node","/opt/colonizer/agent/runner.mjs"]`). Unlike the token-saving
    // mounts above, a missing or invalid binary fails the boot through the usual `boot_inner`
    // error path: without node the agent cannot start at all.
    if agent_needs_node(&agent.vm_command()) {
        let source = app
            .cfg
            .linux_binary("bin/node-guest")
            .context("node runtime bin/node-guest is missing or unusable; run scripts/install.sh")?;
        mounts.push(node_mount(source));
    }
    let mut secrets = Vec::new();
    if agent.needs_claude {
        // The colony's own account, recorded at launch — never the install default by accident.
        let account = s.claude_account.clone().unwrap_or_else(|| "default".into());
        let cred = app
            .claude_cred_for(s.claude_account.as_deref())
            .with_context(|| format!("log in with Claude in Settings first (account '{account}')"))?;
        mounts.push(Mount {
            source: resolve_guest_claude_bin(app).await?,
            target: "/opt/claude/bin/claude".into(),
            read_only: true,
        });
        secrets.push(Secret {
            env: cred.env.into(),
            value: cred.value,
            hosts: vec![CLAUDE_API_HOST.into()],
        });
    }
    if let Some(key) = jev_key {
        // The hook reads TYPESAFE_API_KEY; the guest sees only msb's placeholder for it.
        secrets.push(Secret {
            env: "TYPESAFE_API_KEY".into(),
            value: key,
            hosts: vec!["api.typesafe.ai".into()],
        });
    }
    if !colony_secrets.is_empty() {
        log.info(format!(
            "colony secrets: {}",
            colony_secrets
                .iter()
                .map(|(meta, _)| meta.env.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .await;
    }
    secrets.extend(crate::colony_secrets::boot_secrets(&colony_secrets));
    let mut publish = None;
    // The mesh half of the network fence is captured here — `direct_path_rules` is async (it
    // shells out to `ip`/`ifconfig`) — and the fence itself is decided, purely, in
    // `colony_network` once routing is known.
    let mut mesh_net = None;
    if mesh_on {
        let mesh = app.mesh().await?;
        log.info("starting the private mesh").await;
        mesh.ensure_started().await?;
        mesh.delete_nodes_named(&s.sandbox).await?;
        write_private(&vm_dir.join("mesh-authkey"), mesh.mint_vm_key().await?.as_bytes())?;
        mounts.push(Mount {
            // The guest's own build when the host's is not a Linux one (a Mac builds Mach-O for the
            // mesh it runs itself); otherwise the single vendored copy serves both sides.
            source: app
                .cfg
                .asset("vendor/tailscale-guest")
                .or_else(|_| app.cfg.asset("vendor/tailscale"))?,
            target: "/opt/colonizer/tailscale".into(),
            read_only: true,
        });
        env.push(("COLONIZER_MESH_LOGIN_SERVER".into(), mesh.vm_login_server()));
        env.push(("COLONIZER_MESH_HOSTNAME".into(), s.sandbox.clone()));
        // Reach the host only where the colony must — the headscale control port and the WireGuard
        // direct path — not every loopback service; see `colony_network` for the fence (#375).
        mesh_net = Some((mesh.direct_path_rules().await, mesh.ports().control));
        app.update_session(id, |x| {
            x.mesh = Some(MeshInfo {
                name: s.sandbox.clone(),
                ip: None,
            })
        })
        .await;
    } else {
        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        publish = Some((port, AGENTD_PORT));
        app.update_session(id, |x| x.local_port = Some(port)).await;
    }
    let resolved_egress = crate::egress::resolve(&modules, &org_settings);
    let tls_hosts = tls_edge_hosts(&secrets);
    let (net_profiles, net_rules, egress_record) =
        colony_network(mesh_net, &routing, app.cfg.gateway_bind, &resolved_egress, &tls_hosts);
    // The record is the fleet-view query surface (`GET /api/sessions/{id}/egress`): what the
    // colony could reach, and under whose word, for as long as the session dir survives.
    std::fs::write(dir.join("egress.json"), serde_json::to_vec_pretty(&egress_record)?)?;

    // The chosen stack fills in image and machine size — detected from the
    // repository when the configured preset was `auto`, otherwise the one the
    // operator or the org pinned — and anything set explicitly in modules.json
    // still wins. See crates/colonizer/src/presets.rs.
    let sandbox_settings = crate::config::with_preset(&modules.sandbox, &crate::presets::defaults(&stack));
    let spec = BootSpec {
        name: s.sandbox.clone(),
        image: setting_str(&sandbox_settings, &sandbox_schema, "image"),
        cpus: setting_u64(&sandbox_settings, &sandbox_schema, "cpus").max(1),
        memory: setting_str(&sandbox_settings, &sandbox_schema, "memory"),
        root_disk: setting_str(&sandbox_settings, &sandbox_schema, "root_disk"),
        max_duration: setting_str(&sandbox_settings, &sandbox_schema, "max_duration"),
        workdir: "/workspace".into(),
        mounts,
        env,
        secrets,
        // Allowlist mode names no profile: the deny comes from the flag, not from `--net`.
        net_deny_egress: net_profiles.is_empty(),
        net_profiles,
        net_rules,
        publish,
        command: vec!["sh".into(), "/colonizer/boot.sh".into()],
    };
    // Record the machine size this launch chose before it is put to work, so the session always
    // shows the microVM `msb run` was handed even when a later phase fails and the boot never
    // completes. Spec values are the only per-colony numbers about the VM — agentd exposes no guest
    // CPU% or RSS — so they are captured here, at source.
    app.update_session(id, |x| {
        x.boot_cpus = Some(spec.cpus);
        x.boot_memory = Some(spec.memory.clone());
        x.boot_image = Some(spec.image.clone());
    })
    .await;
    mark_phase(app, id, &mut timing, "mesh-start").await;

    // `msb run` pulls an uncached image itself, so this is not what makes the
    // download happen — it is what stops it being an unexplained wait. On a cold
    // image the first colony otherwise sits on a spinner for gigabytes with
    // nothing said. Pre-warm from Settings (POST /api/sandbox/pull) to keep the
    // download off the launch path entirely.
    //
    // Hoisting the pull out of `msb run` also splits it out of the vm-boot
    // timing, which #15 could not separate without an extra call on every
    // launch. The cache check is that call, and it is already paid for here.
    if !sandbox::is_cached(&app.cfg.msb, &spec.image).await {
        log.info(format!(
            "pulling {} — this happens once per image, and can take a while",
            spec.image
        ))
        .await;
        if let Err(e) = sandbox::pull(&app.cfg.msb, &spec.image).await {
            // Not fatal: `msb run` will try the pull again and report properly.
            log.info(format!(
                "pre-pull of {} did not finish ({e:#}); the boot will pull it",
                spec.image
            ))
            .await;
        }
    }
    mark_phase(app, id, &mut timing, "image-pull").await;

    log.info(format!(
        "booting microVM {} ({}, {} vCPU, {})",
        spec.name, spec.image, spec.cpus, spec.memory
    ))
    .await;
    sandbox::boot(&app.cfg.msb, &spec).await?;
    // The pull is its own phase above, so this is the VM itself — unless the
    // pre-pull failed, in which case `msb run` pulls and this absorbs it.
    mark_phase(app, id, &mut timing, "vm-boot").await;
    let s = ensure_starting(app, id).await?;

    if mesh_on {
        log.info("waiting for the microVM to join the private mesh").await;
        let node = app.mesh().await?.wait_online(&s.sandbox, Duration::from_secs(120)).await?;
        let _ = std::fs::remove_file(vm_dir.join("mesh-authkey"));
        log.info(format!("{} joined the mesh at {}", s.sandbox, node.ip)).await;
        app.update_session(id, |x| {
            x.mesh = Some(MeshInfo {
                name: x.sandbox.clone(),
                ip: Some(node.ip.clone()),
            })
        })
        .await;
    }

    mark_phase(app, id, &mut timing, "mesh-join").await;

    let s = ensure_starting(app, id).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        match agentd_http(app, &s, "GET", "/v1/health").await {
            Ok((200, _)) => break,
            _ if tokio::time::Instant::now() > deadline => bail!("{}", AGENTD_NOT_READY),
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    log.info("agent daemon is ready").await;
    timing.mark("agentd");

    log.info(timing.summary()).await;
    let breakdown = timing.to_json();
    app.update_session(id, |x| x.boot_timing = Some(breakdown)).await;

    ensure_starting(app, id).await?;
    start_link(app, id).await;
    Ok(())
}

/// The read-only mount exposing the vendored node binary at its in-VM path.
pub(crate) fn node_mount(source: PathBuf) -> Mount {
    Mount {
        source,
        target: "/opt/node/bin/node".into(),
        read_only: true,
    }
}

/// Decides the Jev compaction switch for one colony: the staged payload and the mothership's
/// TypeSafe key both have to be there. Takes the probe result rather than the app, so tests cover
/// it without a colony on disk. Ok carries the mount source and the key the secret below needs.
pub(crate) fn jev_compaction(
    payload: Option<PathBuf>,
    key: Option<String>,
) -> std::result::Result<(PathBuf, String), &'static str> {
    let source = payload.ok_or(
        "Jev compaction is switched on, but fast-jev-compaction isn't installed (scripts/install.sh stages it); running without it",
    )?;
    let key =
        key.ok_or("Jev compaction is switched on, but the mothership has no TypeSafe key (set JEV_API_KEY); running without it")?;
    Ok((source, key))
}

const BOOT_SCRIPT: &str = r#"#!/bin/sh
# Generated by colonizer. Runs as the microVM's main process.
set -u
mkdir -p /var/lib/colonizer
# Git metadata is mounted read-only; give git a private, writable index.
if [ -f "${GIT_DIR:-}/index" ]; then cp "$GIT_DIR/index" "$GIT_INDEX_FILE"; fi
export PATH="/opt/node/bin:/opt/claude/bin:$PATH"
if [ -f /colonizer/mesh-authkey ]; then
  mkdir -p /var/lib/tailscale
  /opt/colonizer/tailscale/tailscaled --statedir=/var/lib/tailscale --socket=/run/tailscaled.sock \
    --no-logs-no-support >/var/lib/colonizer/tailscaled.log 2>&1 &
  i=0
  while [ ! -S /run/tailscaled.sock ] && [ "$i" -lt 80 ]; do sleep 0.25; i=$((i + 1)); done
  # --accept-dns=false keeps microsandbox's DNS gateway, which its secret injection relies on.
  /opt/colonizer/tailscale/tailscale --socket=/run/tailscaled.sock up \
    --login-server="$COLONIZER_MESH_LOGIN_SERVER" --auth-key=file:/colonizer/mesh-authkey \
    --hostname="$COLONIZER_MESH_HOSTNAME" --accept-dns=false >>/var/lib/colonizer/tailscaled.log 2>&1 \
    || echo "colonizer: joining the mesh failed" >&2
fi
exec /opt/colonizer/bin/colonizer-agentd --config /colonizer/session.json --token-file /colonizer/token --state-dir /var/lib/colonizer
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colony_is_fenced_to_public_with_only_the_host_ports_it_needs() {
        // A non-default gateway port, so a regression to the 41750 fallback shows up as a mismatch.
        let gateway: std::net::SocketAddr = "127.0.0.1:52000".parse().unwrap();
        let control = 41740;
        let wireguard = vec![
            "allow@192.168.1.4:udp:41743".to_string(),
            "allow@10.1.2.3:udp:41743".to_string(),
        ];
        let routed = providers::ColonyRoutes {
            routes: vec![Value::Bool(true)],
            providers: Vec::new(),
        };
        for (mesh_on, has_routes) in [(true, true), (true, false), (false, true), (false, false)] {
            let mesh = mesh_on.then(|| (wireguard.clone(), control));
            let routes = if has_routes {
                &routed
            } else {
                &providers::ColonyRoutes::default()
            };
            // The default policy is Open with no operator entries: today's fence, plus the
            // always-blocked deny set every colony carries (#303).
            let resolved = crate::egress::resolve(&ModulesConfig::default(), &orgs::OrgSettings::default());
            let (profiles, rules, record) =
                colony_network(mesh.clone(), routes, gateway, &resolved, &["api.anthropic.com".into()]);
            // `public` alone, never the broad `host` profile (#375) — in every combination, so a
            // future change cannot quietly hand the colony every host-loopback port again.
            assert_eq!(
                profiles,
                vec!["public".to_string()],
                "mesh_on={mesh_on} has_routes={has_routes}"
            );
            assert!(
                !profiles.iter().any(|p| p == "host"),
                "mesh_on={mesh_on} has_routes={has_routes}"
            );
            // DNS first (the deny set behind it must not take the forwarder down), then the
            // infrastructure allows — direct path, control port, gateway port — and only then the
            // deny set, with nothing of ours after it in an Open mode that configures nothing.
            let mut expected = vec!["allow@dns".to_string()];
            if let Some((direct_path, _)) = &mesh {
                expected.extend(direct_path.clone());
            }
            if mesh_on {
                expected.push(format!("allow@host:tcp:{control}"));
            }
            if has_routes {
                expected.push(format!("allow@host:tcp:{}", gateway.port()));
            }
            expected.extend(egress::ALWAYS_BLOCKED.iter().map(|t| format!("deny@{t}")));
            // Open mode: the TLS-edge hosts ride the `public` profile, so no 443 allow of ours.
            assert_eq!(rules, expected, "mesh_on={mesh_on} has_routes={has_routes}");
            assert!(
                rules.iter().all(|rule| rule != "allow@api.anthropic.com:tcp:443"),
                "open mode needs no TLS-edge allow: {rules:?}"
            );
            assert_eq!(record.profiles, profiles);
            assert_eq!(record.rules, rules);
            // And of the host rules, nothing but the control and gateway ports, TCP only.
            let host_allows = [
                format!("allow@host:tcp:{control}"),
                format!("allow@host:tcp:{}", gateway.port()),
            ];
            for rule in &rules {
                if rule.contains("@host:") {
                    assert!(host_allows.contains(rule), "unexpected host allow {rule:?} in {rules:?}");
                }
            }
        }
    }

    /// Allowlist mode (#303) keeps the same infrastructure allows, adds the TLS-edge secret hosts
    /// on 443, and drops the `public` profile — the deny set follows, and nothing configured can
    /// precede it (that ordering is `egress.rs`'s own fuzz; here it is the boot's side of the deal).
    #[test]
    fn an_allowlist_colony_keeps_only_harness_ports_the_tls_edge_and_dns() {
        let gateway: std::net::SocketAddr = "127.0.0.1:52000".parse().unwrap();
        let resolved = crate::egress::Resolved {
            policy: crate::egress::EgressPolicy {
                mode: crate::egress::EgressMode::Allowlist,
                ..Default::default()
            },
            sources: crate::egress::Sources {
                mode: "global".into(),
                ..Default::default()
            },
        };
        let (profiles, rules, record) = colony_network(
            Some((vec!["allow@192.168.1.4:udp:41743".into()], 41740)),
            &providers::ColonyRoutes::default(),
            gateway,
            &resolved,
            &["api.anthropic.com".into(), "api.anthropic.com".into()],
        );
        assert!(profiles.is_empty(), "no profile allow in allowlist mode: {profiles:?}");
        assert_eq!(record.mode, crate::egress::EgressMode::Allowlist);
        // Deduplicated TLS edge, DNS first, harness ports (no gateway allow: no routes), then the
        // deny set.
        assert_eq!(rules[0], "allow@dns");
        assert_eq!(rules[1], "allow@192.168.1.4:udp:41743");
        assert_eq!(rules[2], "allow@host:tcp:41740");
        assert_eq!(rules[3], "allow@api.anthropic.com:tcp:443");
        assert!(rules[4].starts_with("deny@"), "the deny set follows the allows: {rules:?}");
    }

    #[test]
    fn a_stale_boot_only_reaps_a_colony_that_is_still_not_live() {
        use SessionStatus::*;
        // Stopped/Failed mid-boot: the stop's `msb rm` ran before `sandbox::boot`, so the
        // orphaned microVM is this stale task's to reap.
        for status in [Stopped, Failed] {
            assert!(stale_boot_needs_teardown(status), "{status:?} must be reaped");
        }
        // Back to Starting: a newer boot/resume claimed the colony under the same sandbox
        // name — tearing down here could kill its fresh VM.
        assert!(!stale_boot_needs_teardown(Starting));
        // Live now: a stale boot must never touch the live VM.
        for status in [Running, WaitingForAnswer, Idle] {
            assert!(!stale_boot_needs_teardown(status), "{status:?} must be left alone");
        }
        // Terminal without a VM, and queued/publishing which a boot can never stale into:
        // no teardown either way, but never a live VM at risk.
        for status in [Queued, Publishing, PrOpened, Merged, Closed, NoChanges] {
            assert!(!stale_boot_needs_teardown(status), "{status:?}");
        }
    }

    /// A pin is the operator's decision: an explicit preset and `custom` both come back unchanged,
    /// with nothing to explain, even when the repository carries a marker that would have detected.
    #[test]
    fn a_pinned_stack_skips_detection_and_logs_nothing() {
        let detected = crate::presets::Detected {
            stack: "node",
            marker: "package.json".to_string(),
        };
        for configured in ["rust", crate::presets::CUSTOM] {
            let (stack, message) = stack_choice(configured, Some(&detected));
            assert_eq!(stack, configured, "the pin decides, not the repository");
            assert!(message.is_none(), "{configured} has nothing to explain");
        }
    }

    /// `auto` over a repository with markers takes the detected stack, and the log line names both
    /// the file that decided and the image that will boot.
    #[test]
    fn auto_uses_the_detected_stack_and_names_the_marker_and_image() {
        let detected = crate::presets::Detected {
            stack: "rust",
            marker: "Cargo.toml".to_string(),
        };
        let (stack, message) = stack_choice(crate::presets::AUTO, Some(&detected));
        assert_eq!(stack, "rust");
        assert_eq!(
            message.as_deref(),
            Some("detected rust from Cargo.toml, using rust:1-bookworm"),
            "the log names the stack, the marker that chose it, and the image"
        );
    }

    /// A marker in a subdirectory keeps its repo-relative path in the log, so the log says which
    /// file decided.
    #[test]
    fn a_subdirectory_marker_names_the_file_that_decided() {
        let detected = crate::presets::Detected {
            stack: "node",
            marker: "web/package.json".to_string(),
        };
        let (stack, message) = stack_choice(crate::presets::AUTO, Some(&detected));
        assert_eq!(stack, "node");
        let message = message.expect("detection logs what it saw");
        assert!(
            message.contains("web/package.json"),
            "the log should carry the subdirectory path, got {message:?}"
        );
    }

    /// `auto` over a repository with no markers falls back, and says so rather than silently
    /// picking one.
    #[test]
    fn auto_with_no_marker_found_falls_back_and_says_so() {
        let (stack, message) = stack_choice(crate::presets::AUTO, None);
        assert_eq!(stack, crate::presets::AUTO_FALLBACK);
        assert_eq!(
            message.as_deref(),
            Some("no stack marker found in the repository; using the node stack"),
            "the fallback is logged, not silent"
        );
    }

    /// The node runtime mounts exactly when the agent's in-VM command starts with `node` — the
    /// claude-code module's `["node","runner.mjs"]` entry once `vm_command()` rewrites the script
    /// to its in-VM path — and the mount exposes the binary read-only at its in-VM path.
    #[test]
    fn node_runtime_mounts_only_for_node_agents() {
        let node_cmd = vec!["node".into(), "/opt/colonizer/agent/runner.mjs".into()];
        assert!(agent_needs_node(&node_cmd), "a node entrypoint needs the runtime");
        let mount = node_mount(PathBuf::from("/dist/bin/node-guest"));
        assert_eq!(mount.target, "/opt/node/bin/node");
        assert!(mount.read_only, "the colony must not rewrite its own runtime");

        assert!(
            !agent_needs_node(&["python3".into(), "runner.py".into()]),
            "a non-node agent boots exactly as before, with no new failure mode"
        );
        assert!(!agent_needs_node(&[]), "an empty command needs nothing mounted");
    }

    /// The boot script puts the node bin dir first and keeps the claude entry as-is.
    #[test]
    fn boot_script_puts_node_first() {
        assert!(
            BOOT_SCRIPT.contains(r#"export PATH="/opt/node/bin:/opt/claude/bin:$PATH""#),
            "node first, claude entry unchanged"
        );
    }

    /// The Jev compaction switch mounts the payload only with the staged files and a key to go with them.
    #[test]
    fn jev_compaction_mounts_only_with_a_payload_and_a_key() {
        let payload = Some(PathBuf::from("/dist/vendor/fast-jev-compaction"));
        let (source, key) =
            jev_compaction(payload.clone(), Some("ts-key".into())).expect("a payload and a key switch compaction on");
        assert_eq!(source, payload.clone().unwrap());
        assert_eq!(key, "ts-key");
        assert_eq!(
            jev_compaction(None, Some("ts-key".into())).unwrap_err(),
            "Jev compaction is switched on, but fast-jev-compaction isn't installed (scripts/install.sh stages it); running without it"
        );
        assert_eq!(
            jev_compaction(payload, None).unwrap_err(),
            "Jev compaction is switched on, but the mothership has no TypeSafe key (set JEV_API_KEY); running without it"
        );
    }
}
