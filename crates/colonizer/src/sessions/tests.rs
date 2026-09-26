//! Fixtures the session tests share, here and in other modules (`sessions::tests::colony`).

use super::*;
use tokio::sync::RwLock;

pub(crate) fn colony(org: &str, status: SessionStatus) -> Session {
    Session {
        id: String::new(),
        repo: format!("{org}/repo"),
        org: org.into(),
        issue: None,
        issue_title: String::new(),
        instructions: String::new(),
        status,
        branch: String::new(),
        base: None,
        parent: None,
        stack: false,
        stack_fork: None,
        origin: None,
        launched_by_token: None,
        worktree: String::new(),
        git_admin_dir: None,
        sandbox: String::new(),
        mesh: None,
        local_port: None,
        agent: String::new(),
        autopilot: false,
        autofix: None,
        automerge: None,
        fix_for: None,
        pr_url: None,
        merged_at: None,
        pr_opened_at: None,
        changed_paths: Vec::new(),
        ci_state: None,
        summary: None,
        publish_stage: None,
        // A bare `publishing` fixture is a live-origin claim, so it holds its slot; tests for
        // a stopped-origin publish flip this off.
        publishing_holds_slot: status == SessionStatus::Publishing,
        needs_rebase: false,
        rebase_orphaned: false,
        queued_behind: None,
        claim_wait: false,
        verify: None,
        verification: None,
        error: None,
        cost_usd: None,
        model_usage: None,
        model_tier: None,
        model_override: None,
        subagent_model_override: None,
        claude_account: None,
        model_routing: None,
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
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}
/// The shape of `create`'s admission, shared by the concurrency tests below: check for room and push a
/// fresh colony in one step.
pub(crate) async fn admit_create(
    sessions: &RwLock<Vec<Session>>,
    org: &str,
    max_parallel: usize,
    org_limit: Option<u64>,
    id: String,
) {
    admit_create_in(sessions, &format!("{org}/repo"), max_parallel, org_limit, 32, id).await
}

/// `admit_create` for a colony of a named repository, under a per-repository limit as well.
pub(crate) async fn admit_create_in(
    sessions: &RwLock<Vec<Session>>,
    repo: &str,
    max_parallel: usize,
    org_limit: Option<u64>,
    repo_limit: u64,
    id: String,
) {
    let org = repo.split_once('/').map_or(repo, |(org, _)| org);
    with_slot(sessions, org, repo, max_parallel, org_limit, repo_limit, |sessions, room| {
        let mut s = colony(
            org,
            if room {
                SessionStatus::Starting
            } else {
                SessionStatus::Queued
            },
        );
        s.id = id;
        s.repo = repo.into();
        sessions.push(s);
    })
    .await
}

/// The shape of `resume`'s admission: re-check resumability and flip the colony under one lock.
pub(crate) async fn admit_resume(
    sessions: &RwLock<Vec<Session>>,
    org: &str,
    max_parallel: usize,
    org_limit: Option<u64>,
    id: &str,
) {
    let repo = format!("{org}/repo");
    with_slot(sessions, org, &repo, max_parallel, org_limit, 32, |sessions, room| {
        let Some(s) = sessions.iter_mut().find(|s| s.id == id) else {
            return;
        };
        if can_resume(s.status, s.cleaned_up, s.git_admin_dir.is_some()) {
            s.status = if room {
                SessionStatus::Starting
            } else {
                SessionStatus::Queued
            };
        }
    })
    .await
}

pub(crate) fn stopped_colony_with_worktree(org: &str, id: String) -> Session {
    let mut s = colony(org, SessionStatus::Stopped);
    s.id = id;
    s.git_admin_dir = Some("git".into());
    s
}

// -- storage failures -----------------------------------------------------------------------

pub(crate) use crate::tests::test_app;

/// A throwaway App with one colony in it, over a temp directory (as in memory.rs).
pub(crate) async fn app_with_colony(id: &str, status: SessionStatus) -> (Shared, PathBuf) {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = test_app(&root);
    let mut s = colony("acme", status);
    s.id = id.to_string();
    app.sessions.write().await.push(s);
    tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
    (app, root)
}

/// The smallest install `create` insists on, as in the org-known test above: an agent module
/// matching the configured provider and a guest binary that claims to be an ELF. Shared with
/// validation.rs, whose fix-colony test creates a session the same way.
pub(crate) fn app_that_can_create(root: &std::path::Path) -> Shared {
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
    crate::tests::test_app_with_agents(root, vec![agent], |cfg| cfg.assets = Some(assets))
}
