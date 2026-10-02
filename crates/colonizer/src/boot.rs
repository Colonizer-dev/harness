//! Colony boot: the async sequence that turns a queued colony into a running microVM — resolve
//! the base and issue, prepare the worktree, assemble prompt, mounts and secrets, size and start
//! the sandbox, wait for the agent daemon — and the failure path that reaps an orphaned microVM.

use crate::{
    App, CLAUDE_API_HOST, Shared,
    config::{ModulesConfig, setting, setting_str, setting_u64},
    diagnosis, egress,
    events::start_link,
    github,
    lifecycle::teardown_vm,
    memory,
    modules::schema_for,
    orgs, providers, resolve_guest_claude_bin,
    sandbox::{self, BootSpec, Mount, Secret},
    sessions::{
        AGENTD_NOT_READY, AGENTD_PORT, MeshInfo, Session, SessionLogger, SessionStatus, agent_env, agent_needs_node, agentd_http,
        apply_exec_policy, colony_image, findings_enabled,
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

/// What a resumed colony is told about its previous run (issue #213): a short digest of the last
/// events of the log the resume just rotated aside — the same one-line digests the cockpit's
/// diagnosis reads, capped at twenty lines and 64 KiB of source, so it rides in the prompt without
/// weighing it down. `None` when there is nothing to tell (no archived log, or one that digests to
/// nothing but deltas), which is the fresh-boot shape and asks for no block at all.
async fn resume_digest(dir: &std::path::Path) -> Option<String> {
    // The resume rotated `events.jsonl` into the highest slot before this boot started, so the
    // highest-numbered archive is the run that just ended.
    let mut highest: Option<u64> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        if let Some(n) = name
            .to_str()
            .and_then(|n| n.strip_prefix("events-"))
            .and_then(|n| n.strip_suffix(".jsonl"))
            .and_then(|n| n.parse::<u64>().ok())
        {
            highest = Some(highest.map_or(n, |m: u64| m.max(n)));
        }
    }
    let archive = dir.join(format!("events-{}.jsonl", highest?));
    let recent = diagnosis::recent_events(&diagnosis::tail_events_within(&archive, 64 * 1024).await)?;
    let mut block = String::from(
        "\n## Where your previous run left off\n\n\
         This colony was resumed after being parked or stopped. The last events of that run, \
         oldest first — pick up where it left off:\n",
    );
    for e in &recent {
        block.push_str(&format!("- #{} {}: {}\n", e.seq, e.kind, e.summary));
    }
    Some(block)
}

/// Boots one colony. `resume` says the boot brings a kept worktree back rather than creating one;
/// such a boot is an authorized effect (issue #98): the caller mints a fresh `Resume` grant at the
/// real approval (the operator's press, the queue's admission, the quota recovery) and this is the
/// one place every resume boot passes through, so the check lives here and denies — with the
/// reason — any resume it was not handed.
pub(crate) async fn boot(app: Shared, id: String, resume: bool, grant: Option<crate::authority::Grant>) {
    if resume
        && let Some(s) = app.session(&id).await
        && let Err(deny) = crate::authority::authorize_opt(
            grant.as_ref(),
            &crate::authority::Effect::Resume,
            &crate::lifecycle::resume_candidate(&s),
            crate::authority::now_unix(),
        )
    {
        let message = format!("resume refused: not authorized ({})", deny.reason);
        app.session_log(&id, "error", format!("session failed to start: {message}"))
            .await;
        let mut attention = None;
        app.update_session(&id, |s| {
            if s.status != SessionStatus::Starting {
                return false;
            }
            s.status = SessionStatus::Failed;
            s.error = Some(truncate(&message, 2000));
            attention = s.clear_attention();
            true
        })
        .await;
        app.note_cleared_attention(&id, attention).await;
        // Failed without ever opening a pull request: it frees the issue for a retry, on GitHub as
        // well as locally, the same as a boot that fails after starting.
        if let Some(s) = app.session(&id).await {
            crate::claims::spawn_release_if_needed(app.clone(), &s);
        }
        return;
    }
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

/// Where a colony keeps the issue it was started on, inside its session directory: written at the
/// first boot that fetched it, and read back by every resume instead of asking GitHub again.
pub(crate) const ISSUE_FILE: &str = "issue.json";

/// How long a resume waits on GitHub for an issue it has no stored copy of at all.
const RESUME_ISSUE_LIMIT: Duration = Duration::from_secs(30);

/// How long a resume waits on the best-effort refresh of its base branch.
const BASE_REFRESH_LIMIT: Duration = Duration::from_secs(60);

/// How many lines of one event log are searched for the colony's first brief. The runner sends it
/// as the first message, so it sits near the top.
const BRIEF_SEARCH_LINES: usize = 200;

/// The issue a boot works on. A fresh boot calls `fetch` (which carries the retry budget and the
/// access wording) and stores what comes back in [`ISSUE_FILE`]. A resume never needs GitHub: it
/// reads the stored issue, else the one recovered from its first brief (`vm/session.json`, then
/// the event logs), and only a colony with neither asks `fetch` once — whose failure is a warning,
/// not a failed resume, since the worktree and branch are what the colony's work lives in.
async fn resolve_issue<F, Fut>(
    dir: &std::path::Path,
    s: &Session,
    resume: bool,
    log: &SessionLogger,
    fetch: F,
) -> Result<Option<Value>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let Some(number) = s.issue else { return Ok(None) };
    if !resume {
        let issue = fetch().await?;
        store_issue(dir, &issue, log).await;
        return Ok(Some(issue));
    }
    if let Some(issue) = stored_issue(dir) {
        log.info(format!(
            "resuming on the stored copy of issue #{number}; GitHub is not asked again"
        ))
        .await;
        return Ok(Some(issue));
    }
    if let Some(issue) = issue_from_brief(dir) {
        log.info(format!("resuming on issue #{number} as this colony's first brief carried it"))
            .await;
        store_issue(dir, &issue, log).await;
        return Ok(Some(issue));
    }
    match fetch().await {
        Ok(issue) => {
            store_issue(dir, &issue, log).await;
            Ok(Some(issue))
        }
        Err(e) => {
            log.warn(format!(
                "resumed offline: issue #{number} is not stored and GitHub could not be read ({}); resuming on its \
                 title and the kept worktree",
                truncate(&format!("{e:#}"), 300)
            ))
            .await;
            Ok(Some(
                json!({"number": number, "title": s.issue_title, "body": "", "labels": [], "comments": []}),
            ))
        }
    }
}

/// The stored issue, when there is a readable one.
fn stored_issue(dir: &std::path::Path) -> Option<Value> {
    let bytes = std::fs::read(dir.join(ISSUE_FILE)).ok()?;
    let issue: Value = serde_json::from_slice(&bytes).ok()?;
    issue.get("title")?.as_str()?;
    Some(issue)
}

/// Stores the issue a boot works on. Best effort: a colony that cannot write it still boots, and
/// its resume recovers the issue from its brief instead.
async fn store_issue(dir: &std::path::Path, issue: &Value, log: &SessionLogger) {
    let written = std::fs::create_dir_all(dir)
        .map_err(anyhow::Error::from)
        .and_then(|()| Ok(serde_json::to_vec_pretty(issue)?))
        .and_then(|bytes| write_private(&dir.join(ISSUE_FILE), &bytes));
    if let Err(e) = written {
        log.warn(format!("could not store the issue for a later resume: {e:#}")).await;
    }
}

/// The issue as this colony's first brief carried it, for a colony started before issues were
/// stored: the `<issue>` block of the last `vm/session.json`, else of the first message in the
/// event logs, oldest run first.
fn issue_from_brief(dir: &std::path::Path) -> Option<Value> {
    let from_session = std::fs::read(dir.join("vm").join("session.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| v["initial_prompt"].as_str().and_then(parse_issue_block));
    if from_session.is_some() {
        return from_session;
    }
    let mut archives: Vec<u64> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_prefix("events-")?
                .strip_suffix(".jsonl")?
                .parse()
                .ok()
        })
        .collect();
    archives.sort_unstable();
    let mut logs: Vec<PathBuf> = archives.into_iter().map(|n| dir.join(format!("events-{n}.jsonl"))).collect();
    logs.push(dir.join("events.jsonl"));
    logs.iter()
        .find_map(|path| first_brief(path).as_deref().and_then(parse_issue_block))
}

/// The runner's echo of its first message (`user_message` with id `initial`) in one event log.
fn first_brief(path: &std::path::Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    std::io::BufReader::new(file)
        .lines()
        .take(BRIEF_SEARCH_LINES)
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .find(|e| e["type"] == "user_message" && e["id"] == "initial")
        .and_then(|e| e["text"].as_str().map(String::from))
}

/// Reads back the `<issue>` block `github::build_prompt` writes: its header lines, then the body
/// (comments included, as the prompt carried them).
fn parse_issue_block(prompt: &str) -> Option<Value> {
    let start = prompt.find("<issue>\n")? + "<issue>\n".len();
    let rest = &prompt[start..];
    let end = rest
        .find("\n</issue>\n\nThe issue text above")
        .or_else(|| rest.rfind("\n</issue>"))?;
    let block = &rest[..end];
    let (head, body) = block.split_once("\n\n").unwrap_or((block, ""));
    let mut issue = json!({"title": "", "body": "", "labels": [], "comments": []});
    for line in head.lines() {
        if let Some(title) = line.strip_prefix("Title: ") {
            issue["title"] = json!(title.trim());
        } else if let Some(url) = line.strip_prefix("URL: ") {
            issue["url"] = json!(url.trim());
        } else if let Some(author) = line.strip_prefix("Author: @") {
            issue["author"] = json!({"login": author.trim()});
        } else if let Some(labels) = line.strip_prefix("Labels: ") {
            let names: Vec<Value> = labels
                .split(", ")
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| json!({"name": l}))
                .collect();
            issue["labels"] = Value::Array(names);
        }
    }
    let body = body.trim();
    if body != "(no description)" {
        issue["body"] = json!(body);
    }
    issue["title"].as_str().filter(|t| !t.is_empty())?;
    Some(issue)
}

/// The default branch as the local mirror records it (a bare clone's `HEAD`), so a resume need
/// not ask GitHub.
async fn local_default_branch(app: &App, bare: &std::path::Path) -> Option<String> {
    let out = crate::util::exec(app.git(bare).args(["symbolic-ref", "--short", "HEAD"]))
        .await
        .ok()?;
    let branch = out.trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

/// Best-effort refresh of a resumed colony's base in the local mirror. The colony's own work is on
/// its kept branch, so a remote that is unreachable or refuses costs only freshness: the resume
/// carries on with the mirror as it is and says so. With external effects off it is not tried.
async fn refresh_base(app: &Shared, repo: &str, bare: &std::path::Path, base: &str, log: &SessionLogger) {
    if crate::authority::external_writes_blocked() {
        log.info("resumed offline: external effects are off, so the base was not refreshed")
            .await;
        return;
    }
    let lock = app.repo_lock(repo).await;
    let _guard = lock.lock().await;
    let refspec = format!("+refs/heads/{base}:refs/remotes/origin/{base}");
    let fetched = crate::util::exec_within(
        BASE_REFRESH_LIMIT,
        app.git(bare).args(["fetch", "--quiet", "origin", refspec.as_str()]),
    )
    .await;
    if let Err(e) = fetched {
        log.warn(format!(
            "resumed offline: base not refreshed, carrying on with origin/{base} as the local mirror has it ({})",
            truncate(&format!("{e:#}"), 300)
        ))
        .await;
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

/// The most a resume's service readiness waits may extend the boot health-wait by (issue #700): a
/// manifest may name an absurd `timeout_secs`, and the boot must neither hang on it nor overflow
/// the deadline arithmetic. The guest honours the spec's timeout either way; this only bounds the
/// mothership's patience with it.
const MAX_RESTORE_WAIT_SECS: u64 = 600;

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

    let dir = app.session_dir(id);
    let bare = app.bare_repo(&s.repo);
    // A fresh colony reads its issue from GitHub, riding out blips on the boot retry budget; a
    // resumed one already has it — stored at its first boot, or recovered from its first brief —
    // and does not ask GitHub again, so a GitHub that refuses or is unreachable cannot fail it.
    let issue = resolve_issue(&dir, &s, resume, &log, || async {
        let number = s.issue.unwrap_or_default();
        if resume {
            // One bounded attempt, only for a colony that has nothing stored at all.
            return tokio::time::timeout(RESUME_ISSUE_LIMIT, github::fetch_issue(app, &s.repo, number))
                .await
                .context("GitHub did not answer in time")?;
        }
        let label = format!("fetching issue {}#{number}", s.repo);
        log.info(label.clone()).await;
        let fetched = github::with_boot_retry(&label, Some(&log), boot_started_at, || {
            github::fetch_issue(app, &s.repo, number)
        })
        .await;
        match fetched {
            Ok(issue) => Ok(issue),
            Err(e) => Err(github::access_error(app, &s.repo, e).await),
        }
    })
    .await?;
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
        // A resumed colony that never recorded its base (an older record) reads the default branch
        // off the local mirror rather than asking GitHub; only a mirror that cannot say asks.
        stack::BootBase::Default if resume => match local_default_branch(app, &bare).await {
            Some(base) => base,
            None => default_base(app, &s.repo, &log, boot_started_at).await?,
        },
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

    let wt = PathBuf::from(&s.worktree);
    let admin = if resume {
        // The worktree and branch outlive the microVM, so a resumed colony picks them up as they are.
        log.info(format!("resuming on the kept worktree, branch {}", s.branch)).await;
        let admin = PathBuf::from(s.git_admin_dir.as_deref().context("this colony has no worktree to resume")?);
        refresh_base(app, &s.repo, &bare, &base, &log).await;
        admin
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
    // The module's `requires` preflight (issue #633), re-run on the image this boot actually
    // resolved — the same value the runner brief and the sandbox spec get — so a resume or restart
    // onto a changed image refuses before any VM work rather than dying in the runner's in-VM
    // preflight. The launch-time check held the configured stack; here the detected one.
    if let Err(problem) = crate::modules::check_requires(
        &agent,
        &colony_image(&app.agents, &modules, &stack),
        &crate::modules::harness_staged_binaries(&app.cfg),
    ) {
        anyhow::bail!(problem);
    }

    mark_phase(app, id, &mut timing, "git").await;

    let vm_dir = dir.join("vm");
    let out_dir = dir.join("out");
    // Cloned out of the lock before the awaits below: `touched_files` shells out to git per
    // sibling, and the sessions guard must not be held across that.
    let colonies = app.sessions.read().await.clone();
    let touched = github::touched_files(app, &colonies, &s).await;
    let siblings = github::siblings_of(&colonies, &s, &touched);
    // The colony's instructions carry an external-input marking when a scoped API token launched it
    // (issue #508): the record keeps the token's id, the prompt names it, and a revoked token falls
    // back to its id so the marking never disappears.
    let external_token = match s.launched_by_token.as_deref() {
        None => None,
        Some(id) => Some(app.api_tokens.name_of(id).await.unwrap_or_else(|| id.to_string())),
    };
    let mut prompt = github::build_prompt(
        &s,
        issue.as_ref(),
        &base,
        resume,
        &siblings,
        stacked_on.as_deref(),
        external_token.as_deref(),
    );
    // Colony secrets in scope: named in the prompt (never their values) and handed to msb below,
    // which substitutes each one only on TLS to its hosts.
    let colony_secrets = crate::colony_secrets::for_colony(&app.cfg.config_dir, &s.repo);
    prompt.push_str(&crate::colony_secrets::prompt_block(
        &colony_secrets.iter().map(|(meta, _)| meta).collect::<Vec<_>>(),
    ));
    // The previous run's story (issue #213): a resumed colony's own brief carries a digest of the
    // event log the resume just rotated aside, so the agent knows what it was doing when the park
    // or stop interrupted it instead of reading its work cold.
    if resume && let Some(story) = resume_digest(&dir).await {
        prompt.push_str(&story);
    }
    write_private(&vm_dir.join("token"), random_token().as_bytes())?;
    // The colony's own agent module's settings (issue #201): an org may run its colonies on a
    // module other than the install's, whose settings are not this module's to read.
    let mut agent_choice = orgs::effective_agent_for(&modules, &org_settings, &agent.id);
    // A mapping colony draws with archify whatever its org has switched on (maps.rs) — by hand or
    // from a refresh loop.
    if s.origin.as_deref().is_some_and(crate::maps::is_map_origin) {
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
    // The exec policy is the operator's rule about commands, not a model setting: it follows the
    // colony to this module pick, or the boot refuses when the pick cannot apply it.
    apply_exec_policy(&agent, &modules.agent, &app.agents, &wt, &mut runner_env)?;
    // Per-task model routing (routing.rs): the tier comes from the issue in front of the colony
    // unless the operator named one at launch, and the tier's model replaces the module's own when
    // that tier has one. Read off the effective settings, so an org override is honoured.
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
    // Jev (jev.rs): an optional external classifier's second opinion, fetched here in the async boot
    // path — never inside `routing::decide`, which stays synchronous and pure. Off by default, and a
    // silent no-op without both a setting and a `JEV_API_KEY` secret. Shadow mode records it for
    // comparison; act mode (issue #583) lets a confident opinion pick the tier, never below the
    // floor `decide` derives from `sensitive` — whether this org's gateway demands more than any
    // provider for the task's class (sensitivity.rs `required_mark`).
    let flag = |key: &str| setting(&agent_choice, &agent.schema, key).and_then(Value::as_bool);
    let jev_mode = crate::routing::JevMode::from_settings(
        flag("jev_routing_act").unwrap_or(false),
        flag("jev_shadow_mode").unwrap_or(false),
    );
    let route_settings = crate::routing::RoutingSettings {
        enabled: flag("route_per_task").unwrap_or(true),
        chosen: s.model_tier.as_deref().and_then(crate::routing::Tier::parse),
        jev_mode,
        jev_act_confidence: setting(&agent_choice, &agent.schema, "jev_routing_act_confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.8),
        sensitive: crate::sensitivity::required_mark(sensitivity, org_settings.sensitivity.as_ref())
            > crate::sensitivity::ProviderMark::Any,
    };
    task_signals.jev = crate::jev::shadow_opinion(jev_mode.asks(), &s.issue_title, &task_labels, &task_signals).await;
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
    // exactly as before. Only a low pick by the rule or by Jev in act mode is gated: medium is the
    // module's model and high an escalation, neither with a context-reload tradeoff, and an
    // operator's explicit tier is an instruction a cost estimate must never second-guess. The gate
    // only ever keeps the colony on the module's model, so it can never undercut the floor.
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
                    && matches!(
                        tier_decision.source,
                        crate::routing::Source::Rule | crate::routing::Source::Jev
                    );
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
    // Conditional instructions (issue #473): the agent's instruction rules can be conditioned on the
    // task's labels, which only the mothership knows at boot; they travel as a comma-separated list.
    if !task_labels.is_empty() {
        runner_env.insert("COLONIZER_TASK_LABELS".into(), Value::String(task_labels.join(",")));
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
            "sensitivity: this task's paths classify restricted; the gateway will refuse any provider that does not meet this org's restricted requirement (marked trusted in providers.json, unless the org's override says otherwise)".to_string(),
        )
        .await;
    }
    // An opinion that disagrees with the rule is worth a low-key note for later promotion analysis;
    // it never blocks boot or looks like an error. In act mode the reason above already says so
    // when Jev picked the tier.
    if tier_decision.jev_agrees() == Some(false) && tier_decision.source != crate::routing::Source::Jev {
        log.info(format!(
            "jev {} mode: the second opinion says {} where the rule says {}",
            jev_mode.as_str(),
            tier_decision.jev.as_ref().map(|jev| jev.tier.as_str()).unwrap_or("?"),
            tier_decision.rule.as_str()
        ))
        .await;
    }
    let record = json!({
        // Which decision this is and how Jev took part (issue #583), so a report can compare
        // Jev-acted decisions against rule ones by outcome without re-deriving either.
        "point": crate::routing::DECISION_POINT,
        "jev_mode": jev_mode,
        "jev_agrees": tier_decision.jev_agrees(),
        "floor": tier_decision.floor,
        "tier": tier_decision.tier,
        "rule": tier_decision.rule,
        "source": tier_decision.source,
        "score": tier_decision.score,
        "reason": tier_decision.reason,
        "model": if model_changed { json!(effective_model) } else { Value::Null },
        // The harness the decision was made for, so a routing record can be joined against the
        // spend journal's per-harness rows (issue #296).
        "agent": agent.id.clone(),
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
    let routing = providers::colony_routes(app, &gateway_token);
    if !routing.routes.is_empty() {
        runner_env.insert(
            "COLONIZER_MODEL_ROUTES".into(),
            Value::String(serde_json::to_string(&routing.routes)?),
        );
    }
    // A hand-edited providers.json row the PUT would have refused fails the launch here, with the
    // file, provider and row named (#295).
    if let Some(message) = providers::config_error(&routing.providers) {
        bail!("{message}");
    }
    // A `<provider>/` prefix nobody configured is a typo'd route, not a Claude model: the runner would
    // only warn and send those requests to Anthropic (router.mjs), so the boot refuses instead — here,
    // after tier substitution, so only the models this colony will actually run are checked.
    if let Some(message) = routing.unusable_route(&agent.id, &runner_env) {
        bail!("{message}");
    }
    let used = routing.used(&runner_env);
    // Recorded on the session because the gateway needs it long after boot: every proxied call is
    // checked against these sets (issue #409), so a colony's token opens only the providers — and
    // only the models — its model settings route to. Before the token is written (issue #681): the
    // record is what the token's access is derived from, so no token can exist ahead of it.
    app.update_session(id, |x| {
        x.allowed_providers = Some(used.iter().map(|p| p.id.clone()).collect());
        x.allowed_models = Some(routing.used_models(&runner_env));
    })
    .await;
    write_private(&app.gateway_token_file(id), gateway_token.as_bytes())?;
    // The tool policy the colony is being launched under, recorded for the colony report (#295): the
    // connection level per used provider, then the harness level the agent module configured.
    for line in providers::connection_disabled_tool_lines(&used) {
        app.session_log(id, "info", line).await;
    }
    match providers::harness_disabled_tool_lines(&agent, &runner_env) {
        Ok(lines) => {
            for line in lines {
                app.session_log(id, "info", line).await;
            }
        }
        Err(message) => bail!("{message}"),
    }
    let probes = futures_util::future::join_all(used.iter().map(|p| crate::gateway::probe_cached(app, p))).await;
    for (provider, health) in used.iter().zip(probes) {
        if health["reachable"] != true {
            // The cached probe only feeds a warning; a refusal is decided on a fresh one, so an
            // endpoint that came back within the probe TTL is not refused on a stale answer (#295).
            let health = if provider.fallback_model.is_some() {
                health.clone()
            } else {
                crate::gateway::probe(app, provider).await
            };
            if health["reachable"] == true {
                app.session_log(
                    id,
                    "info",
                    format!(
                        "model provider {} was unreachable on the cached probe but answered a fresh one; its requests will go through",
                        provider.id
                    ),
                )
                .await;
                continue;
            }
            let then = match &provider.fallback_model {
                Some(model) => format!("its requests will fall back to {model}"),
                // Without a fallback every request through this connection can only fail, so the
                // launch is refused with the fix instead of warned about (#295).
                None => bail!(
                    "{}",
                    providers::unreachable_route(&agent.id, provider, &health, &runner_env).unwrap_or_else(|| {
                        format!("model provider {} is unreachable and has no fallback model", provider.id)
                    })
                ),
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
    // deja recall (issue #495): the URL and this colony's gateway token, so the colony's MCP tool
    // can ask its own org's transcript index read-only. Set only when deja is effective-on for the
    // colony's org; the tool reports its absence as "unavailable" rather than erroring.
    if !s.org.is_empty() && orgs::effective_deja_enabled(&modules, &org_settings) {
        runner_env.insert(
            "COLONIZER_RECALL_URL".into(),
            Value::String(format!(
                "http://host.microsandbox.internal:{}/recall",
                app.cfg.gateway_bind.port()
            )),
        );
        runner_env.insert("COLONIZER_RECALL_TOKEN".into(), Value::String(gateway_token.clone()));
    }
    // What the colony can and cannot run is part of the agent's brief (runner.mjs), so it names the
    // image this colony actually boots — the resolved stack's, not the configured one — or an agent
    // in a repository detected as Rust would brief itself for a Node machine.
    runner_env.insert(
        "COLONIZER_IMAGE".into(),
        Value::String(colony_image(&app.agents, &modules, &stack)),
    );
    // A colony coming back to deliver a held answer (issue #562) resumes the agent session it
    // reported, and the answer is the first thing the resumed runner is told (the `initial_prompt`
    // below). Without a session id — never reported, or the module stopped declaring resumability
    // since — the answer still rides the prompt, so it is delivered either way.
    if s.pending_answer.is_some()
        && let Some(session_id) = &s.agent_session
    {
        runner_env.insert("COLONIZER_RESUME_SESSION".into(), Value::String(session_id.clone()));
    }

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
    // The agent's session transcripts (issue #562): a resumable module gets a writable host
    // directory over the in-VM path its runner keeps transcripts at, so a suspension can stop the
    // microVM without losing the conversation, and the resumed boot picks the same session up. The
    // host directory sits with the colony's other host-side state, so deleting the colony deletes
    // the transcripts with it.
    if let Some(resume_dir) = &agent.resume_dir {
        let host = dir.join("transcripts");
        std::fs::create_dir_all(&host)?;
        mounts.push(Mount {
            source: host,
            target: resume_dir.clone(),
            read_only: false,
        });
    }
    // Services that come back after a resume (issue #700): the guest's writers (`colonizer-svc`,
    // a Claude Code background-Bash hook) record every service they start as one JSON file in this
    // directory, which lives in the host session dir — so the records outlive the microVM exactly
    // like the transcripts above — and is mounted back writable at the path the env var names.
    // Created on every boot, so the writers always have it; read on a resume, when the `restore`
    // key of session.json below is built from it.
    let services_dir = dir.join(crate::services::DIR_NAME);
    std::fs::create_dir_all(&services_dir)?;
    runner_env.insert(
        crate::services::ENV_VAR.into(),
        Value::String(crate::services::GUEST_DIR.into()),
    );
    // GUEST_DIR sits inside the read-only /colonizer mount, where the guest cannot create a mount
    // point: without this directory in vm_dir the microVM fails to boot (agentd never serves).
    vm_mount_point(&vm_dir, crate::services::GUEST_DIR)?;
    mounts.push(Mount {
        source: services_dir.clone(),
        target: crate::services::GUEST_DIR.into(),
        read_only: false,
    });
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
    //
    // What the runner is first told (issue #562): a resumed session that comes back to deliver a
    // held answer gets just the answer — its transcript already carries the task brief — and a
    // colony without a session id to resume gets the answer appended, so the answer never rides on
    // the resume working.
    let initial_prompt = match (&s.pending_answer, &s.agent_session) {
        (Some(pa), Some(_)) => pa.prompt.clone(),
        (Some(pa), None) => format!("{prompt}\n\n{}", pa.prompt),
        (None, _) => prompt,
    };
    // What a resume brings back (issue #700): the manifest's declared services plus the records
    // the previous run registered, scrubbed of the colony's secret values before any of it reaches
    // session.json. The guest relaunches the restartable ones, waits each out to readiness or its
    // timeout, and opens the resumed turn saying what came back and what was lost. Warnings are
    // logged, never a failed boot; only a resume boot carries the key.
    let mut restore = None;
    // The guest starts no runner — and serves no HTTP — until every restartable service answered
    // or timed out (issue #700), so the health wait below must outlast the longest readiness wait
    // it was handed: a manifest timeout beyond the base deadline would otherwise fail the boot
    // while the guest was still waiting a service out. Only specs with a probe wait anything, and
    // one without `timeout_secs` gets the guest's default. The cap keeps a manifest naming an
    // absurd `timeout_secs` from hanging the boot — or overflowing the deadline arithmetic below.
    let mut restore_wait = Duration::ZERO;
    if resume {
        let secrets: Vec<String> = colony_secrets.iter().map(|(_, value)| value.clone()).collect();
        let (specs, warnings) = crate::services::resume_specs(&wt, &services_dir, &secrets);
        for warning in warnings {
            log.warn(format!("services: {warning}")).await;
        }
        restore_wait = specs
            .iter()
            .filter(|spec| spec.restart && spec.ready.is_some())
            .map(|spec| spec.timeout_secs.unwrap_or(crate::services::DEFAULT_TIMEOUT_SECS))
            .max()
            .map_or(Duration::ZERO, |secs| Duration::from_secs(secs.min(MAX_RESTORE_WAIT_SECS)));
        restore = Some(crate::services::restore_json(s.was_suspended, &specs));
    }
    let mut session_json = json!({
        "session_id": id,
        "workspace": "/workspace",
        "listen": format!("0.0.0.0:{AGENTD_PORT}"),
        "agent": {"module": agent.id, "command": agent.vm_command(), "env": runner_env},
        "initial_prompt": initial_prompt,
    });
    if let Some(restore) = restore {
        session_json["restore"] = restore;
    }
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
    // Vendor keys (issue #629): a module whose manifest declares a vendor host gets the gateway
    // provider's stored key for that vendor — else the mothership's own env — pushed under the env
    // names and hosts the manifest names. Nothing configured is silent: the module runs without a
    // key, which is not a boot failure. A colony secret naming the same env keeps its own value.
    let taken = colony_secrets.iter().map(|(meta, _)| meta.env.clone()).collect::<Vec<_>>();
    secrets.extend(crate::modules::vendor_boot_secrets(
        &agent,
        &|id| app.provider_key(id),
        &|name| std::env::var(name).ok(),
        &taken,
    ));
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
    // Path policy (docs/path-policy.md, issue #300): credential-shaped files in the worktree are
    // masked out of the colony's view and its agent-facing config pinned read-only. The host plans
    // the enforcement — binds, plus an empty placeholder for every listed path the checkout does
    // not have, since the guest's bind needs a target — then writes the intended placeholder list
    // BEFORE creating anything: a crash in between would leave an empty file git could stage with
    // no record that it was ours (issue #300 review). Publish later removes the still-empty ones,
    // so nothing placeholder-shaped lands in a pull request. All before the VM starts, on purpose:
    // the guest enforces the policy as it boots, before the agent can run a command. The org's
    // overrides (#649) join the module's lists, tightening them; the written policy file records
    // the whole effective list.
    let org_path_policy = org_settings.path_policy.clone().unwrap_or_default();
    let policy = crate::path_policy::from_settings(&sandbox_settings, &sandbox_schema, &org_path_policy);
    let previous = crate::path_policy::read_list(
        &std::fs::read_to_string(vm_dir.join(crate::path_policy::PLACEHOLDERS_FILE)).unwrap_or_default(),
    );
    let planned = crate::path_policy::plan(&wt, &policy, &previous)?;
    // Only targets guaranteed to exist get a bind: a skipped entry has no bind line, or the guest
    // mount would fail and brick the boot.
    crate::path_policy::write_list(&vm_dir.join(crate::path_policy::POLICY_FILE), &planned.binds)?;
    crate::path_policy::write_list(
        &vm_dir.join(crate::path_policy::PLACEHOLDERS_FILE),
        &planned.placeholder_names(),
    )?;
    crate::path_policy::apply(&wt, &planned)?;
    log.info(crate::path_policy::summary(&policy, &planned)).await;
    if let Some(note) = crate::path_policy::opt_outs(&policy) {
        app.session_log(id, "warn", note).await;
    }
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
        if let Err(e) = app.execution.pull(&spec.image).await {
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
    app.execution.boot(&spec).await?;
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
    // The base covers the guest's own start plus one health attempt left in flight when agentd
    // starts serving; a resume's readiness waits ride on top of it (issue #700, `restore_wait`).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90) + restore_wait;
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
    // The runner is up, so a held answer is as delivered as it gets (issue #562): say so, once, and
    // only then take it off the record. A boot that failed above never reaches this, and the answer
    // stays for the next resume; a stop cleared it under its own claim.
    let delivered = app.update_session(id, |x| x.pending_answer.take().is_some()).await;
    if let Some((s, true)) = delivered {
        app.session_log(
            id,
            "info",
            "held answer delivered: the agent resumes its session with it".into(),
        )
        .await;
        crate::activity::record_restored(app, &s).await;
    }
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

/// Creates, in the host dir mounted read-only at `/colonizer`, the mount point for a nested mount
/// at `target`: the guest cannot create one under a read-only mount, and a missing mount point
/// fails the microVM's boot. A target outside `/colonizer` needs nothing and is left alone.
fn vm_mount_point(vm_dir: &std::path::Path, target: &str) -> std::io::Result<()> {
    match target.strip_prefix("/colonizer/") {
        Some(rel) if !rel.is_empty() => std::fs::create_dir_all(vm_dir.join(rel)),
        _ => Ok(()),
    }
}

const BOOT_SCRIPT: &str = r#"#!/bin/sh
# Generated by colonizer. Runs as the microVM's main process.
set -u
mkdir -p /var/lib/colonizer
# Git metadata is mounted read-only; give git a private, writable index.
if [ -f "${GIT_DIR:-}/index" ]; then cp "$GIT_DIR/index" "$GIT_INDEX_FILE"; fi
export PATH="/opt/node/bin:/opt/claude/bin:/opt/colonizer/bin:$PATH"
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
# Path policy (docs/path-policy.md): the host lists what must be hidden or read-only inside the
# worktree. A masked file is covered by a bind of /dev/null (reads see nothing, writes land in
# the void), a masked directory by an empty read-only tmpfs (a tiny explicit size: size=0 means
# unlimited), a protected path by a read-only bind of itself. Fail closed: a policy the guest
# cannot enforce — a missing file, a symlink where a real path was expected (what the mount would
# follow is not the checkout's file), a kind the host does not write, a missing list — must not
# boot into a colony that assumes it was. The variables default the real paths but let the loop
# run as-is in a mount namespace against a stand-in workspace, which is how it was verified.
ws="${COLONIZER_WORKSPACE:-/workspace}"
policy="${COLONIZER_PATH_POLICY:-/colonizer/path-policy}"
[ -f "$policy" ] || { echo "colonizer: path policy: $policy is missing" >&2; exit 1; }
while read -r kind rel || [ -n "${rel:-}" ]; do
  [ -n "${kind:-}" ] || continue
  [ -n "${rel:-}" ] || { echo "colonizer: path policy: empty path" >&2; exit 1; }
  target="$ws/$rel"
  # The host resolves symlinks to their in-worktree target before writing this list, so a link
  # here means the checkout changed under the policy — refuse rather than follow it.
  [ -L "$target" ] && { echo "colonizer: path policy: $rel is a symlink" >&2; exit 1; }
  [ -e "$target" ] || { echo "colonizer: path policy: $rel is missing" >&2; exit 1; }
  case "$kind" in
    mask-file)
      mount --bind /dev/null "$target" || { echo "colonizer: path policy: cannot mask $rel" >&2; exit 1; }
      ;;
    mask-dir)
      mount -t tmpfs -o ro,size=4k,mode=0555 colonizer-mask "$target" || { echo "colonizer: path policy: cannot mask $rel" >&2; exit 1; }
      ;;
    protect)
      mount --bind "$target" "$target" && mount -o remount,bind,ro "$target" || {
        echo "colonizer: path policy: cannot protect $rel" >&2
        exit 1
      }
      ;;
    *)
      echo "colonizer: path policy: unknown kind $kind" >&2
      exit 1
      ;;
  esac
done <"$policy"

# Kernel-interface hygiene, after tailscale (it may need sysctls) and before the agent runs;
# the agent's seccomp filter denies unshare/setns, so these masks cannot be undone. Every step
# is best-effort: a missing path is skipped, a failed mount logs one line and boot continues.
echo 1 > /proc/sys/kernel/dmesg_restrict 2>/dev/null || echo "colonizer: kernel.dmesg_restrict stays open" >&2
echo 2 > /proc/sys/kernel/kptr_restrict 2>/dev/null || echo "colonizer: kernel.kptr_restrict stays open" >&2
mount -o remount,hidepid=invisible /proc 2>/dev/null \
  || mount -o remount,hidepid=2 /proc 2>/dev/null \
  || echo "colonizer: /proc stays world-readable" >&2
for f in kcore kallsyms keys timer_list sched_debug sysrq-trigger cmdline latency_stats modules config.gz kpageflags kpagecount kpagecgroup; do
  [ -e "/proc/$f" ] && { mount --bind /dev/null "/proc/$f" 2>/dev/null || echo "colonizer: could not mask /proc/$f" >&2; }
done
for d in /sys/kernel/debug /sys/kernel/tracing /sys/kernel/security /sys/fs/bpf /sys/firmware /proc/acpi /proc/scsi /proc/asound; do
  [ -d "$d" ] && { mount -t tmpfs -o ro,size=0 colonizer-mask "$d" 2>/dev/null || echo "colonizer: could not mask $d" >&2; }
done
mount --bind /proc/sys /proc/sys 2>/dev/null \
  && mount -o remount,bind,ro /proc/sys 2>/dev/null \
  || echo "colonizer: /proc/sys stays writable" >&2
mount -o remount,ro /sys 2>/dev/null || echo "colonizer: /sys stays writable" >&2
# The agent-facing service registry (issue #700) is the agentd binary under its argv0 name. Only
# the binary's own bind is read-only (msb mounts a single file, not its directory); the directory
# is the VM's writable overlay root, so the symlink sticks. `set -u` is not `set -e`: a failed link
# says so here, and agentd logs a warn event when it finds the link missing.
ln -sf /opt/colonizer/bin/colonizer-agentd /opt/colonizer/bin/colonizer-svc \
  || echo "colonizer: could not link colonizer-svc; the agent must run \`colonizer-agentd svc\` instead" >&2
exec /opt/colonizer/bin/colonizer-agentd --config /colonizer/session.json --token-file /colonizer/token --seal-token --state-dir /var/lib/colonizer
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest a cold resume rides in the prompt comes from the run the resume just rotated
    /// aside — the highest-numbered archive, not the first — and is absent for a colony with no
    /// previous run to tell about.
    #[tokio::test]
    async fn the_resume_digest_tells_the_latest_archived_run_and_skips_a_fresh_colony() {
        let dir = std::env::temp_dir().join(format!("colonizer-digest-{}", crate::util::short_id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(resume_digest(&dir).await.is_none(), "no archive, no story");
        let line = |seq: u64, text: &str| {
            json!({"seq": seq, "ts": "2026-01-01T00:00:00Z", "type": "user_message", "text": text}).to_string()
        };
        std::fs::write(
            dir.join("events-1.jsonl"),
            format!("{}\n{}\n", line(1, "first run began"), line(2, "first run ended")),
        )
        .unwrap();
        let newer = format!(
            "{}\n{}\n{}\n",
            json!({"seq": 1, "type": "assistant_text_delta", "text": "noise"}), // digests to nothing
            line(2, "ran the tests"),
            line(3, "found the failure"),
        );
        std::fs::write(dir.join("events-2.jsonl"), newer).unwrap();
        std::fs::write(dir.join("events.jsonl"), line(1, "the new run, not the old one")).unwrap();
        let story = resume_digest(&dir).await.expect("an archived run has a story");
        assert!(story.contains("#2 user_message: ran the tests") && story.contains("#3 user_message: found the failure"));
        assert!(!story.contains("first run"), "the older archive is not the story");
        assert!(!story.contains("the new run"), "the live log is not the story");
        assert!(!story.contains("assistant_text_delta"), "delta noise is digested away");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A resumable colony on issue #7, in a throwaway App, with its session directory made.
    fn resumable(tag: &str) -> (std::path::PathBuf, Shared, Session, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("colonizer-resume-{tag}-{}", crate::util::short_id()));
        let _ = std::fs::remove_dir_all(&root);
        let app = crate::app::tests::test_app(&root);
        let mut s = crate::sessions::tests::colony("acme", SessionStatus::Starting);
        s.id = format!("res{tag}");
        s.issue = Some(7);
        s.issue_title = "Fix the flaky login".into();
        s.branch = "colonizer/issue-7".into();
        let dir = app.session_dir(&s.id);
        std::fs::create_dir_all(dir.join("vm")).unwrap();
        (root, app, s, dir)
    }

    fn github_issue() -> Value {
        json!({
            "number": 7,
            "title": "Fix the flaky login",
            "body": "The login test fails one run in ten.",
            "labels": [{"name": "bug"}, {"name": "ready"}],
            "comments": [],
            "url": "https://github.com/acme/repo/issues/7",
            "author": {"login": "octo"},
        })
    }

    /// What a colony's session log said at `level`, oldest first.
    async fn said(app: &Shared, id: &str, level: &str) -> Vec<String> {
        let rt = app.runtime(id).await;
        let logs = rt.logs.lock().await;
        logs.iter()
            .filter(|e| e["level"] == level)
            .filter_map(|e| e["message"].as_str().map(String::from))
            .collect()
    }

    /// The observed outage: GitHub refusing (403, account suspended) or unreachable. A resume with
    /// a stored issue never asks, so neither can fail it, and the issue it boots on is the stored one.
    #[tokio::test]
    async fn a_resume_boots_on_the_stored_issue_whatever_github_says() {
        for (tag, refusal) in [
            ("403", "gh: Sorry. Your account was suspended. (HTTP 403)"),
            ("net", "error connecting to api.github.com: dial tcp: network is unreachable"),
        ] {
            let (root, app, s, dir) = resumable(tag);
            let worktree = root.join("worktree");
            std::fs::create_dir_all(&worktree).unwrap();
            std::fs::write(worktree.join("half-done.rs"), "// unfinished work").unwrap();
            std::fs::write(dir.join(ISSUE_FILE), github_issue().to_string()).unwrap();
            let asked = std::cell::Cell::new(0);
            let log = app.logger(&s.id);
            let issue = resolve_issue(&dir, &s, true, &log, || async {
                asked.set(asked.get() + 1);
                Err(anyhow::anyhow!(refusal))
            })
            .await
            .expect("a resume with a stored issue does not fail on GitHub");
            assert_eq!(issue, Some(github_issue()));
            assert_eq!(asked.get(), 0, "GitHub is not asked again on resume");
            assert!(worktree.join("half-done.rs").exists(), "the worktree is left as it was");
            assert!(said(&app, &s.id, "error").await.is_empty());
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// A colony started before issues were stored recovers its issue from its first brief:
    /// `vm/session.json` first, else the runner's echo of it in the oldest event log. Either way the
    /// recovered issue is stored, so the next resume reads it directly.
    #[tokio::test]
    async fn an_old_colony_recovers_its_issue_from_its_brief() {
        let (root, app, mut s, dir) = resumable("brief");
        s.base = Some("main".into());
        let prompt = github::build_prompt(&s, Some(&github_issue()), "main", false, &[], None, None);
        let failing = || async { Err::<Value, _>(anyhow::anyhow!("gh: Sorry. Your account was suspended. (HTTP 403)")) };
        let log = app.logger(&s.id);

        std::fs::write(
            dir.join("vm/session.json"),
            json!({"session_id": s.id, "initial_prompt": prompt}).to_string(),
        )
        .unwrap();
        let issue = resolve_issue(&dir, &s, true, &log, failing).await.unwrap().unwrap();
        assert_eq!(issue["title"], "Fix the flaky login");
        assert_eq!(issue["body"], "The login test fails one run in ten.");
        assert_eq!(issue["labels"], json!([{"name": "bug"}, {"name": "ready"}]));
        assert_eq!(issue["author"]["login"], "octo");
        assert!(dir.join(ISSUE_FILE).exists(), "the recovered issue is stored");

        // The last session.json carried only a held answer; the first run's event log has the brief.
        std::fs::remove_file(dir.join(ISSUE_FILE)).unwrap();
        std::fs::write(
            dir.join("vm/session.json"),
            json!({"initial_prompt": "The maintainer answered: yes"}).to_string(),
        )
        .unwrap();
        let echo = json!({"seq": 1, "type": "user_message", "id": "initial", "text": prompt});
        std::fs::write(
            dir.join("events-1.jsonl"),
            format!("{}\n{echo}\n", json!({"seq": 0, "type": "status", "state": "running"})),
        )
        .unwrap();
        let issue = resolve_issue(&dir, &s, true, &log, failing).await.unwrap().unwrap();
        assert_eq!(issue["body"], "The login test fails one run in ten.");
        // And it resumes with the same brief as before: the prompt rebuilt from the recovered
        // issue carries the same issue block.
        let rebuilt = github::build_prompt(&s, Some(&issue), "main", true, &[], None, None);
        assert!(rebuilt.contains("Title: Fix the flaky login") && rebuilt.contains("Labels: bug, ready"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Nothing stored anywhere and GitHub down: the resume still goes ahead, on the issue's title
    /// and the kept worktree, with a warning rather than a failure.
    #[tokio::test]
    async fn a_resume_with_nothing_stored_and_github_down_warns_and_carries_on() {
        let (root, app, s, dir) = resumable("bare");
        let log = app.logger(&s.id);
        let issue = resolve_issue(&dir, &s, true, &log, || async {
            Err(anyhow::anyhow!("error connecting to api.github.com"))
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(issue["title"], "Fix the flaky login");
        let warned = said(&app, &s.id, "warn").await;
        assert!(warned.iter().any(|w| w.starts_with("resumed offline")), "{warned:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The path when GitHub works: a fresh boot fetches the issue and stores it, and the resume
    /// boots on exactly what was fetched.
    #[tokio::test]
    async fn a_fresh_boot_stores_the_issue_its_resume_reads_back() {
        let (root, app, s, dir) = resumable("fresh");
        let log = app.logger(&s.id);
        let fresh = resolve_issue(&dir, &s, false, &log, || async { Ok(github_issue()) })
            .await
            .unwrap();
        assert_eq!(fresh, Some(github_issue()));
        let resumed = resolve_issue(&dir, &s, true, &log, || async { Ok(json!({"title": "changed since"})) })
            .await
            .unwrap();
        assert_eq!(resumed, fresh, "the resume boots on the stored issue");
        // A colony with no issue asks nobody either way.
        let mut chat = s.clone();
        chat.issue = None;
        let none = resolve_issue(&dir, &chat, false, &log, || async { Err(anyhow::anyhow!("never asked")) }).await;
        assert!(none.unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A fresh launch still needs GitHub, but a suspended account is named as one — not sent off to
    /// reconnect — and the failure reaches the boot.
    #[tokio::test]
    async fn a_fresh_launch_refused_by_a_suspended_account_says_so() {
        let (root, app, s, dir) = resumable("susp");
        let log = app.logger(&s.id);
        let raw = "`gh issue view 7 -R acme/repo` failed (exit status: 1): gh: Sorry. Your account was suspended. (HTTP 403)";
        let err = resolve_issue(&dir, &s, false, &log, || async {
            Err(github::access_error(&app, "acme/repo", anyhow::anyhow!(raw)).await)
        })
        .await
        .expect_err("a fresh launch cannot start without its issue");
        let message = format!("{err:#}");
        assert!(message.contains("suspended the account"), "{message}");
        assert!(!message.contains("Reconnect GitHub"), "{message}");
        assert!(!dir.join(ISSUE_FILE).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The base refresh on resume is best effort: a remote that cannot be reached is a warning and
    /// the mirror stays as it was; with external effects off it is not tried at all. The mirror's
    /// own `HEAD` answers the default branch without GitHub.
    #[tokio::test]
    async fn a_base_refresh_that_fails_on_resume_is_a_warning() {
        let (root, app, s, _dir) = resumable("fetch");
        let bare = root.join("mirror.git");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("--git-dir")
                .arg(&bare)
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        std::fs::create_dir_all(&bare).unwrap();
        git(&["init", "--quiet", "--bare", "--initial-branch=trunk"]);
        let gone = root.join("no-such-remote.git");
        git(&["remote", "add", "origin", gone.to_str().unwrap()]);
        let log = app.logger(&s.id);

        refresh_base(&app, &s.repo, &bare, "trunk", &log).await;
        let warned = said(&app, &s.id, "warn").await;
        assert!(
            warned.iter().any(|w| w.starts_with("resumed offline: base not refreshed")),
            "{warned:?}"
        );
        assert!(said(&app, &s.id, "error").await.is_empty());
        assert_eq!(local_default_branch(&app, &bare).await.as_deref(), Some("trunk"));

        {
            let _offline = crate::authority::test_block_external_writes();
            refresh_base(&app, &s.repo, &bare, "trunk", &log).await;
        }
        let noted = said(&app, &s.id, "info").await;
        assert!(noted.iter().any(|i| i.contains("external effects are off")), "{noted:?}");
        assert_eq!(said(&app, &s.id, "warn").await.len(), 1, "no second fetch was tried");
        let _ = std::fs::remove_dir_all(&root);
    }

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

    /// Regression (PR #770's colony-e2e): the services mount targets a path inside the read-only
    /// `/colonizer` mount, so its mount point must be created host-side or the microVM fails to boot.
    #[test]
    fn the_services_mount_point_exists_inside_the_vm_dir() {
        let vm_dir = std::env::temp_dir().join(format!("colonizer-mountpoint-{}", crate::util::short_id()));
        std::fs::create_dir_all(&vm_dir).unwrap();
        vm_mount_point(&vm_dir, crate::services::GUEST_DIR).unwrap();
        assert!(vm_dir.join(crate::services::DIR_NAME).is_dir());
        vm_mount_point(&vm_dir, "/workspace").unwrap();
        assert!(
            !vm_dir.join("workspace").exists(),
            "targets outside /colonizer are left alone"
        );
        std::fs::remove_dir_all(&vm_dir).unwrap();
    }

    /// The boot script puts the node bin dir first and keeps the claude entry as-is.
    #[test]
    fn boot_script_puts_node_first() {
        assert!(
            BOOT_SCRIPT.contains(r#"export PATH="/opt/node/bin:/opt/claude/bin:/opt/colonizer/bin:$PATH""#),
            "node first, claude entry unchanged"
        );
    }

    /// The guest's service CLI is the agentd binary under its argv0 name (issue #700): linked onto
    /// the exported PATH before the agent runs, so the runner's first shell already has it.
    #[test]
    fn boot_script_puts_the_service_cli_on_the_path() {
        let link = BOOT_SCRIPT
            .find("ln -sf /opt/colonizer/bin/colonizer-agentd /opt/colonizer/bin/colonizer-svc")
            .expect("the colonizer-svc symlink");
        let path = BOOT_SCRIPT.find(r#"export PATH="#).expect("the PATH export");
        let exec = BOOT_SCRIPT
            .find("exec /opt/colonizer/bin/colonizer-agentd")
            .expect("the exec of agentd");
        assert!(
            path < link && link < exec,
            "the link sits between the PATH export ({path}) and the exec ({exec})"
        );
        // `set -u` is not `set -e`: a failed link must say so, not vanish.
        let fallback = BOOT_SCRIPT[link..exec].find("|| echo \"colonizer: could not link colonizer-svc");
        assert!(fallback.is_some(), "a failed colonizer-svc link is loud");
    }

    /// The boot script's own link lines, run by `sh` against a stand-in bin dir: the link lands
    /// where the PATH export looks, and a dir the guest cannot write (here: absent) makes a loud
    /// line on stderr while the boot carries on to exec agentd (`set -u`, not `set -e`).
    #[cfg(unix)]
    #[test]
    fn boot_script_links_colonizer_svc_and_says_so_when_it_cannot() {
        let start = BOOT_SCRIPT.find("ln -sf /opt/colonizer/bin/colonizer-agentd").unwrap();
        let end = BOOT_SCRIPT.find("exec /opt/colonizer/bin/colonizer-agentd").unwrap();
        let lines = &BOOT_SCRIPT[start..end];
        let root = std::env::temp_dir().join(format!("colonizer-svc-link-{}", crate::util::short_id()));
        let run = |bin: &std::path::Path| {
            let script = format!(
                "set -u\n{}echo booted",
                lines.replace("/opt/colonizer/bin", &bin.display().to_string())
            );
            std::process::Command::new("sh").arg("-c").arg(script).output().unwrap()
        };
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("colonizer-agentd"), b"").unwrap();
        let ok = run(&bin);
        assert_eq!(String::from_utf8_lossy(&ok.stdout).trim(), "booted");
        assert!(ok.stderr.is_empty(), "{}", String::from_utf8_lossy(&ok.stderr));
        assert_eq!(
            std::fs::read_link(bin.join("colonizer-svc")).unwrap(),
            bin.join("colonizer-agentd")
        );
        let failed = run(&root.join("absent"));
        assert_eq!(
            String::from_utf8_lossy(&failed.stdout).trim(),
            "booted",
            "the boot carries on"
        );
        let stderr = String::from_utf8_lossy(&failed.stderr);
        assert!(stderr.contains("colonizer: could not link colonizer-svc"), "{stderr}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The path policy is enforced in the guest before the agent starts, and every kind the host
    /// writes has a branch — a policy line the guest did not act on would be silent non-enforcement.
    #[test]
    fn boot_script_enforces_the_path_policy_before_the_agent_starts() {
        for kind in ["mask-file", "mask-dir", "protect"] {
            assert!(BOOT_SCRIPT.contains(kind), "the guest handles {kind}");
        }
        let policy = BOOT_SCRIPT.find("/colonizer/path-policy").expect("the policy file is read");
        let exec = BOOT_SCRIPT.find("exec /opt/colonizer/bin/colonizer-agentd").unwrap();
        assert!(policy < exec, "the binds happen before the agent can run a command");
        // Fail closed: an unenforceable bind stops the boot rather than starting without it — a
        // missing target, a symlink the mount would follow, a kind the host does not write, and a
        // lost policy file are all loud stops, not silent starts.
        assert!(BOOT_SCRIPT.contains("cannot mask"), "mask failures are loud");
        assert!(BOOT_SCRIPT.contains("cannot protect"), "protect failures are loud");
        assert!(BOOT_SCRIPT.contains("unknown kind"), "an unknown kind is loud");
        assert!(BOOT_SCRIPT.contains("is missing"), "a missing target is loud");
        assert!(BOOT_SCRIPT.contains("is a symlink"), "a symlink target is loud");
        // And the whole list, not just its lines: a lost path-policy file stops the boot before
        // the loop would silently enforce nothing.
        let guard = BOOT_SCRIPT
            .find("is missing\" >&2; exit 1; }")
            .expect("a lost policy file is loud");
        assert!(
            guard < BOOT_SCRIPT.find("while read").unwrap(),
            "the missing-list check precedes the loop"
        );
    }

    /// Kernel-interface hardening runs after tailscale setup (which may need sysctls) and before
    /// agentd is exec'd — the agent's seccomp filter denies unshare, so masks set here stick.
    #[test]
    fn boot_script_hardens_kernel_interfaces_before_the_agent_runs() {
        let mesh = BOOT_SCRIPT
            .find(r#"|| echo "colonizer: joining the mesh failed" >&2"#)
            .expect("the script joins the mesh");
        let hardening = BOOT_SCRIPT.find("dmesg_restrict").expect("the hardening step is present");
        let exec = BOOT_SCRIPT
            .find("exec /opt/colonizer/bin/colonizer-agentd")
            .expect("the script execs agentd");
        assert!(
            mesh < hardening && hardening < exec,
            "hardening sits between tailscale setup ({mesh}) and the exec of agentd ({exec}), got {hardening}"
        );
        for marker in [
            "hidepid=invisible",                                // stronger hidepid, ...
            "hidepid=2",                                        // ... with the older-kernel fallback
            "for f in kcore kallsyms",                          // the masked files, bind-over loop
            r#"[ -e "/proc/$f" ] && { mount --bind /dev/null"#, // existing files only
            "/sys/fs/bpf",                                      // sensitive dirs masked ro
            "remount,bind,ro /proc/sys",                        // sysctls read-only, after the writes
            "remount,ro /sys",
        ] {
            assert!(BOOT_SCRIPT.contains(marker), "hardening must contain {marker:?}");
        }
    }

    /// The exec passes `--seal-token` (issue #640): agentd covers /colonizer/token with a read-only
    /// bind of /dev/null as soon as it has read it, so the runner child it spawns cannot read the
    /// bearer token and reach the unfiltered /v1/pty shell with it.
    #[test]
    fn boot_script_passes_seal_token_to_agentd() {
        let exec = BOOT_SCRIPT
            .find("exec /opt/colonizer/bin/colonizer-agentd")
            .expect("the script execs agentd");
        let line = BOOT_SCRIPT[exec..].lines().next().expect("the exec line");
        assert!(line.contains("--seal-token"), "the exec must pass --seal-token: {line}");
        assert!(
            line.contains("--token-file /colonizer/token"),
            "the sealed path is still the token the daemon reads: {line}"
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
