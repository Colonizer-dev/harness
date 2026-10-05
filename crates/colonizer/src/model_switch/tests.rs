use super::*;
use crate::sessions::tests::colony;

fn test_root(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("colonizer-model-switch-{name}-{}", uuid::Uuid::new_v4()))
}

fn role() -> Value {
    json!({"type": "string", "default": ""})
}

/// claude-code with its six roles, as its module.json declares them.
fn claude_code_schema() -> Value {
    json!({"type": "object", "properties": {
        "model_high": {"type": "string", "title": "Model for large tasks", "default": ""},
        "model": {"type": "string", "title": "Orchestrator model", "default": ""},
        "plugins": {"type": "string", "default": ""},
        "summary_model": role(), "background_model": role(), "subagent_model": role(), "model_low": role(),
    }})
}

fn agent(id: &str, schema: Value, needs_claude: bool) -> crate::modules::AgentModule {
    crate::modules::AgentModule::test(id)
        .entry(vec!["node".into()])
        .needs_claude(needs_claude)
        .schema(schema)
}

/// An install on claude-code (main `bailian/qwen3.8-max`), with codex (one role, `model`) and
/// opencode (`model`, `small_model`) installed, `bailian` and `zai` providers, and three orgs:
/// `acme` overriding the subagent, `beta` on codex, `gamma` with nothing of its own.
async fn install(name: &str) -> (std::path::PathBuf, Shared) {
    let root = test_root(name);
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/providers.json"),
        serde_json::to_vec(&json!([
            {"id": "bailian", "name": "Bailian", "base_url": "http://127.0.0.1:1", "auth": "none", "models": ["qwen3.8-max"]},
            {"id": "zai", "name": "Z.AI", "base_url": "http://127.0.0.1:1", "auth": "none", "models": ["glm-5"]},
        ]))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("config/orgs.json"),
        serde_json::to_vec(&json!({
            "acme": {"agent": {"subagent_model": "zai/glm-5"}},
            "beta": {"agent": {"module": "codex", "model": "zai/glm-5"}},
            "gamma": {"max_parallel": 3},
        }))
        .unwrap(),
    )
    .unwrap();
    let agents = vec![
        agent("claude-code", claude_code_schema(), false),
        agent("codex", json!({"type": "object", "properties": {"model": role()}}), false),
        agent(
            "opencode",
            json!({"type": "object", "properties": {"model": role(), "small_model": role()}}),
            false,
        ),
        // Needs a Claude login this install does not have: it can't launch.
        agent("hermes", json!({"type": "object", "properties": {"model": role()}}), true),
    ];
    let app = crate::tests::test_app_with_agents(&root, agents, |_| {});
    {
        let mut modules = app.modules.write().await;
        modules.agent.settings = serde_json::from_value(json!({
            "model": "bailian/qwen3.8-max",
            "background_model": "haiku",
        }))
        .unwrap();
    }
    (root, app)
}

/// Past the parallel limit, so a resume queues instead of spawning a boot (as the quota card's
/// tests do).
async fn fill_the_parallel_limit(app: &Shared) {
    let max = orgs::global_max_parallel(&app.modules.read().await.clone()) as usize;
    let mut sessions = app.sessions.write().await;
    for i in 0..max {
        let mut filler = colony("filler", SessionStatus::Idle);
        filler.id = format!("filler-{i}");
        // On another module, so no switch here reaches them.
        filler.agent = "opencode".into();
        sessions.push(filler);
    }
}

async fn add_parked(app: &Shared, id: &str, org: &str) {
    let mut s = colony(org, SessionStatus::Parked);
    s.id = id.into();
    s.git_admin_dir = Some("git".into());
    app.sessions.write().await.push(s);
    std::fs::create_dir_all(app.session_dir(id)).unwrap();
}

async fn act(app: &Shared, body: Value) -> Result<Value, crate::AppError> {
    let req: SwitchRequest = serde_json::from_value(body).unwrap();
    switch(State(app.clone()), Json(req)).await.map(|Json(v)| v)
}

fn row<'a>(roles: &'a Value, role: &str) -> &'a Value {
    roles
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["role"] == role)
        .unwrap_or_else(|| panic!("no {role} row in {roles}"))
}

#[test]
fn a_modules_roles_are_its_model_settings_orchestrator_first() {
    let roles: Vec<String> = module_roles(&claude_code_schema()).into_iter().map(|(r, _)| r).collect();
    assert_eq!(
        roles,
        [
            "model",
            "subagent_model",
            "background_model",
            "summary_model",
            "model_low",
            "model_high"
        ],
        "plugins is not a role, and the order is the role order, not the schema's"
    );
    assert_eq!(
        module_roles(&claude_code_schema())[0].1,
        "Orchestrator model",
        "the schema's title"
    );
    assert!(module_roles(&json!({"type": "object"})).is_empty());
}

/// Install vs org: an org override wins and says so; the install's settings apply only to the
/// install's own module; anything else is the module's default.
#[test]
fn each_role_resolves_with_its_source() {
    let mut modules = ModulesConfig::default();
    modules.agent.settings = serde_json::from_value(json!({"model": "opus", "background_model": "haiku"})).unwrap();
    let schema = claude_code_schema();
    let install = Value::Array(resolve_roles(&modules, None, "claude-code", &schema));
    let main = row(&install, "model");
    assert_eq!(
        (main["value"].as_str(), main["source"].as_str()),
        (Some("opus"), Some("install"))
    );
    let sub = row(&install, "subagent_model");
    assert_eq!((sub["value"].as_str(), sub["source"].as_str()), (Some(""), Some("default")));
    assert_eq!(row(&install, "summary_model")["org_settable"], false);

    let org: OrgSettings = serde_json::from_value(json!({"agent": {"subagent_model": "zai/glm-5"}})).unwrap();
    let rows = Value::Array(resolve_roles(&modules, Some(&org), "claude-code", &schema));
    assert_eq!(row(&rows, "subagent_model")["source"], "org");
    assert_eq!(row(&rows, "subagent_model")["value"], "zai/glm-5");
    assert_eq!(row(&rows, "model")["source"], "install", "inherited from the install");
    assert_eq!(row(&rows, "model")["org_settable"], true);

    // An org on another module never gets the install's (claude-code) settings.
    let codex = json!({"type": "object", "properties": {"model": {"type": "string", "default": "gpt-5.5"}}});
    let rows = Value::Array(resolve_roles(&modules, Some(&OrgSettings::default()), "codex", &codex));
    assert_eq!(
        (row(&rows, "model")["value"].as_str(), row(&rows, "model")["source"].as_str()),
        (Some("gpt-5.5"), Some("default"))
    );
}

#[tokio::test]
async fn assignments_list_the_install_every_org_the_modules_and_the_models() {
    let (root, app) = install("assignments").await;
    app.gateway.mark_quota_exhausted(
        "zai",
        Some("Oct 6, 09:00 UTC".into()),
        Some(chrono::Utc::now().timestamp() + 3600),
    );
    let Json(view) = assignments(State(app.clone())).await.unwrap();

    assert_eq!(view["install"]["module"], "claude-code");
    assert_eq!(row(&view["install"]["roles"], "model")["value"], "bailian/qwen3.8-max");
    assert_eq!(row(&view["install"]["roles"], "model")["source"], "install");

    let org = |name: &str| {
        view["orgs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["org"] == name)
            .unwrap_or_else(|| panic!("no {name} in {}", view["orgs"]))
            .clone()
    };
    let acme = org("acme");
    assert_eq!(
        (acme["module"].as_str(), acme["module_source"].as_str()),
        (Some("claude-code"), Some("install"))
    );
    assert_eq!(row(&acme["roles"], "subagent_model")["source"], "org");
    assert_eq!(row(&acme["roles"], "model")["source"], "install");
    let beta = org("beta");
    assert_eq!(
        (beta["module"].as_str(), beta["module_source"].as_str()),
        (Some("codex"), Some("org"))
    );
    assert_eq!(beta["roles"].as_array().unwrap().len(), 1, "codex declares one role");
    assert_eq!(row(&org("gamma")["roles"], "background_model")["value"], "haiku");

    let modules = view["modules"].as_array().unwrap();
    let hermes = modules.iter().find(|m| m["id"] == "hermes").unwrap();
    assert!(hermes["blocked"].as_str().unwrap().contains("log in with Claude"), "{hermes}");
    assert!(modules.iter().find(|m| m["id"] == "codex").unwrap()["blocked"].is_null());

    let models = view["models"].as_array().unwrap();
    let glm = models.iter().find(|m| m["id"] == "zai/glm-5").unwrap();
    assert_eq!(
        (glm["out_of_quota"].as_bool(), glm["reset_at"].as_str()),
        (Some(true), Some("Oct 6, 09:00 UTC"))
    );
    let qwen = models.iter().find(|m| m["id"] == "bailian/qwen3.8-max").unwrap();
    assert_eq!(
        (qwen["out_of_quota"].as_bool(), qwen["healthy"].as_bool()),
        (Some(false), Some(true))
    );
    assert!(models.iter().any(|m| m["id"] == "sonnet"), "Claude's models are offered");
    let _ = std::fs::remove_dir_all(root);
}

/// Every refusal comes before anything is written: a model not on offer, one out of quota, a
/// role the module does not have, an install-only role per org, a module that can't launch.
#[tokio::test]
async fn a_disallowed_switch_changes_nothing() {
    let (root, app) = install("refused").await;
    app.gateway.mark_quota_exhausted("zai", None, None);
    let orgs_before = std::fs::read(root.join("config/orgs.json")).unwrap();
    let settings_before = app.modules.read().await.agent.settings.clone();
    for (body, says) in [
        (
            json!({"scope": "install", "roles": {"model": "nope/nothing"}}),
            "not a model on offer",
        ),
        (
            json!({"scope": "install", "roles": {"subagent_model": "zai/glm-5"}}),
            "out of quota",
        ),
        (
            json!({"scope": "install", "roles": {"small_model": "sonnet"}}),
            "not a model role of claude-code",
        ),
        (
            json!({"scope": "org", "org": "acme", "roles": {"summary_model": "sonnet"}}),
            "install-wide only",
        ),
        (json!({"scope": "org", "org": "gamma", "module": "hermes"}), "can't launch"),
        (
            json!({"scope": "org", "org": "gamma", "module": "nope"}),
            "unknown agent module",
        ),
        // Several roles: the bad one refuses the whole switch, the good one included.
        (
            json!({"scope": "install", "roles": {"model": "sonnet", "model_high": "nope/nothing"}}),
            "model_high",
        ),
        (json!({"scope": "install", "roles": {}}), "nothing to switch"),
        (
            json!({"scope": "install", "roles": {"model": "sonnet"}, "apply": "later"}),
            "apply",
        ),
    ] {
        let err = act(&app, body.clone()).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST, "{body}");
        assert!(err.message().contains(says), "{body}: {}", err.message());
    }
    assert_eq!(
        std::fs::read(root.join("config/orgs.json")).unwrap(),
        orgs_before,
        "no org moved"
    );
    assert_eq!(
        app.modules.read().await.agent.settings,
        settings_before,
        "the install is untouched"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// `new`: the settings move through the Settings handlers, and no colony is touched.
#[tokio::test]
async fn a_new_colonies_switch_saves_the_settings_and_leaves_the_colonies() {
    let (root, app) = install("new").await;
    add_parked(&app, "p1", "gamma").await;
    let reply = act(
        &app,
        json!({"scope": "install", "roles": {"model": "zai/glm-5", "background_model": null}}),
    )
    .await
    .unwrap();
    let agent = app.modules.read().await.agent.settings.clone();
    assert_eq!(agent["model"], "zai/glm-5");
    assert!(!agent.contains_key("background_model"), "cleared to the module's default");
    let saved: Value = serde_json::from_slice(&std::fs::read(root.join("config/modules.json")).unwrap()).unwrap();
    assert_eq!(
        saved["agent"]["settings"]["model"], "zai/glm-5",
        "saved through the modules API"
    );
    let changes = reply["changes"].as_array().unwrap();
    let model = changes.iter().find(|c| c["key"] == "model").unwrap();
    assert_eq!(
        (model["was"].as_str(), model["now"].as_str()),
        (Some("bailian/qwen3.8-max"), Some("zai/glm-5"))
    );
    assert_eq!(reply["affected"], json!([]));
    let p1 = app.session("p1").await.unwrap();
    assert_eq!(p1.status, SessionStatus::Parked, "not restarted");
    assert!(p1.model_override.is_none());

    // Per org, and back: "Use install default" clears the override.
    act(&app, json!({"scope": "org", "org": "acme", "roles": {"model": "sonnet"}}))
        .await
        .unwrap();
    let acme = app.org_settings("acme").agent.unwrap();
    assert_eq!(acme.model.as_deref(), Some("sonnet"));
    assert_eq!(acme.subagent_model.as_deref(), Some("zai/glm-5"), "the other override stays");
    act(&app, json!({"scope": "org", "org": "acme", "roles": {"model": null}}))
        .await
        .unwrap();
    assert_eq!(app.org_settings("acme").agent.unwrap().model, None);
    assert_eq!(
        app.org_settings("gamma").max_parallel,
        Some(3),
        "an org's other settings are kept"
    );

    // An org's module: picked, then back to the install's.
    act(
        &app,
        json!({"scope": "org", "org": "gamma", "module": "opencode", "roles": {"model": "sonnet"}}),
    )
    .await
    .unwrap();
    let gamma = app.org_settings("gamma").agent.unwrap();
    assert_eq!(
        (gamma.module.as_deref(), gamma.model.as_deref()),
        (Some("opencode"), Some("sonnet"))
    );
    act(&app, json!({"scope": "org", "org": "gamma", "module": ""}))
        .await
        .unwrap();
    assert_eq!(app.org_settings("gamma").agent.unwrap().module, None);
    let _ = std::fs::remove_dir_all(root);
}

/// `running`: a dry run counts the colonies first and changes nothing; the switch then points
/// their overrides at the new model and restarts them through the quota card's path. Colonies
/// on another module, outside the org, or whose org overrides the role stay.
#[tokio::test]
async fn a_running_switch_moves_and_restarts_the_scopes_colonies() {
    let (root, app) = install("running").await;
    add_parked(&app, "g1", "gamma").await;
    add_parked(&app, "a1", "acme").await;
    add_parked(&app, "b1", "beta").await;
    app.update_session("b1", |s| s.agent = "codex".into()).await;
    let mut pinned = colony("gamma", SessionStatus::Parked);
    pinned.id = "g2".into();
    pinned.git_admin_dir = Some("git".into());
    pinned.model_override = Some("zai/glm-5".into());
    app.sessions.write().await.push(pinned);
    std::fs::create_dir_all(app.session_dir("g2")).unwrap();
    fill_the_parallel_limit(&app).await;

    let body = json!({"scope": "install", "roles": {"model": "sonnet", "subagent_model": "haiku"}, "apply": "running"});
    let mut dry = body.clone();
    dry["dry_run"] = json!(true);
    let plan = act(&app, dry).await.unwrap();
    let mut affected: Vec<&str> = plan["affected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    affected.sort();
    // a1: its org overrides the subagent, but the orchestrator still moves. b1: on codex.
    assert_eq!(affected, ["a1", "g1", "g2"], "{plan}");
    assert_eq!(
        app.modules.read().await.agent.settings["model"],
        "bailian/qwen3.8-max",
        "a dry run writes nothing"
    );
    assert_eq!(app.session("g1").await.unwrap().status, SessionStatus::Parked);

    let reply = act(&app, body).await.unwrap();
    let mut restarted: Vec<&str> = reply["colonies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    restarted.sort();
    assert_eq!(restarted, ["a1", "g1", "g2"], "{reply}");
    let g1 = app.session("g1").await.unwrap();
    assert_eq!(g1.model_override.as_deref(), Some("sonnet"));
    assert_eq!(g1.subagent_model_override.as_deref(), Some("haiku"));
    assert_eq!(g1.status, SessionStatus::Queued, "restarted: queued to boot on the new model");
    assert_eq!(
        app.session("g2").await.unwrap().model_override.as_deref(),
        Some("sonnet"),
        "its launch pick moves too"
    );
    let a1 = app.session("a1").await.unwrap();
    assert_eq!(a1.model_override.as_deref(), Some("sonnet"));
    assert_eq!(a1.subagent_model_override, None, "acme overrides the subagent itself");
    let b1 = app.session("b1").await.unwrap();
    assert_eq!(
        (b1.status, b1.model_override),
        (SessionStatus::Parked, None),
        "another module stays"
    );
    assert!(
        reply["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["scope"] == "colony" && c["target"] == "g1" && c["was"] == "bailian/qwen3.8-max"),
        "{reply}"
    );

    // Per org: only that org's colonies.
    let reply = act(
        &app,
        json!({"scope": "org", "org": "beta", "roles": {"model": "sonnet"}, "apply": "running"}),
    )
    .await
    .unwrap();
    assert_eq!(reply["colonies"], json!(["b1"]), "{reply}");
    assert_eq!(app.session("b1").await.unwrap().model_override.as_deref(), Some("sonnet"));
    let _ = std::fs::remove_dir_all(root);
}

/// Both routes are owner-only: a scoped API token, at any scope, gets 403.
#[tokio::test]
async fn scoped_tokens_are_refused() {
    let (root, app) = install("tokens").await;
    for scope in ["read", "operate", "launch"] {
        let made = app
            .api_tokens
            .create(crate::api_tokens::NewToken {
                name: format!("ci-{scope}"),
                scope: scope.into(),
                orgs: Vec::new(),
                repos: Vec::new(),
                max_concurrent: None,
                budget_usd_per_day: None,
            })
            .await
            .unwrap();
        let token = app.api_tokens.authenticate(&made.token).await.unwrap();
        for (method, path) in [
            (axum::http::Method::GET, "/api/models/assignments"),
            (axum::http::Method::POST, "/api/models/switch"),
        ] {
            assert!(
                matches!(
                    crate::api_tokens::authorize(&app, &token, &method, path).await,
                    Err(crate::api_tokens::Deny::Forbidden(_))
                ),
                "{scope} {method} {path}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(root);
}
