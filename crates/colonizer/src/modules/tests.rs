use super::*;
use crate::config::ModulesConfig;
use std::sync::Arc;

#[test]
fn the_agent_entry_leaves_node_to_path() {
    // The runner command is `["node", "/opt/colonizer/agent/runner.mjs"]`:
    // `node` is not a file in the module directory, so `vm_command` leaves
    // it bare for the VM's `PATH` (`/opt/node/bin` first) to resolve, while
    // the runner script maps to its read-only mount. That split is the
    // premise issue #249's vendored Node runtime exists for.
    let dir = std::env::temp_dir().join(format!("colonizer-vm-command-{}", crate::util::short_id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("runner.mjs"), "// test fixture").unwrap();
    let module = AgentModule::test("claude-code")
        .dir(dir)
        .entry(vec!["node".into(), "runner.mjs".into()])
        .needs_claude(true);
    let command = module.vm_command();
    assert_eq!(command.first().map(String::as_str), Some("node"), "{command:?}");
    assert_eq!(
        command.get(1).map(String::as_str),
        Some("/opt/colonizer/agent/runner.mjs"),
        "{command:?}"
    );
    // End to end without KVM: that bare `node` is exactly what mounts the
    // vendored runtime (sessions::agent_needs_node over the resolved command).
    assert!(
        crate::sessions::agent_needs_node(&command),
        "a bare `node` entrypoint must mount the vendored runtime: {command:?}"
    );
    std::fs::remove_dir_all(&module.dir).ok();
}

#[test]
fn a_vendor_key_is_resolved_stored_first_then_env_and_never_over_a_taken_env() {
    // `Secret` carries a key value and so has no `Debug`; the push is compared as plain rows.
    fn rows(secrets: &[Secret]) -> Vec<(&str, &str, Vec<&str>)> {
        secrets
            .iter()
            .map(|s| (s.env.as_str(), s.value.as_str(), s.hosts.iter().map(String::as_str).collect()))
            .collect()
    }

    // Plain `fn` items, not closures: a closure literal fixes its argument's lifetime, which the
    // higher-ranked `Fn(&str)` the lookups are declared with needs.
    fn stored_openai(id: &str) -> Option<String> {
        (id == "openai").then(|| "sk-test-stored".to_string())
    }
    fn stored_none(_: &str) -> Option<String> {
        None
    }
    fn env_xai_blank(name: &str) -> Option<String> {
        (name == "XAI_API_KEY").then(|| "  ".to_string())
    }
    fn env_xai(name: &str) -> Option<String> {
        (name == "XAI_API_KEY").then(|| "sk-test-env".to_string())
    }
    fn env_none(_: &str) -> Option<String> {
        None
    }

    // The push order the colony sees: a stored gateway key wins over the mothership's own env,
    // a blank or missing stored key falls back to that env, and nothing configured means no
    // secret — never a boot failure. An env name a colony secret already grants is skipped, so
    // the operator's own value keeps precedence without a duplicate flag.
    // The push reads only the manifest's declarations, not the module's id or entry.
    let module = AgentModule::test("codex").vendor_secrets(vec![
        DeclaredSecret {
            env: vec!["CODEX_API_KEY".into()],
            hosts: vec!["api.openai.com".into()],
        },
        DeclaredSecret {
            env: vec!["XAI_API_KEY".into()],
            hosts: vec!["api.x.ai".into()],
        },
    ]);
    assert_eq!(
        rows(&vendor_boot_secrets(&module, &stored_openai, &env_xai_blank, &[])),
        vec![("CODEX_API_KEY", "sk-test-stored", vec!["api.openai.com"])],
    );

    // No stored openai key and a blank XAI env: the env fallback only fires on a value.
    assert_eq!(
        rows(&vendor_boot_secrets(&module, &stored_none, &env_xai, &[])),
        vec![("XAI_API_KEY", "sk-test-env", vec!["api.x.ai"])],
    );

    // Nothing configured anywhere, or the env name taken by a colony secret: silence.
    assert!(vendor_boot_secrets(&module, &stored_none, &env_none, &[]).is_empty());
    let taken = ["CODEX_API_KEY".to_string()];
    assert!(vendor_boot_secrets(&module, &stored_openai, &env_none, &taken).is_empty());
}

#[test]
fn a_broken_agent_manifest_is_reported_by_path_and_cause_instead_of_vanishing() {
    let root = std::env::temp_dir().join(format!("colonizer-discover-{}", crate::util::short_id()));
    let agents = root.join("modules/agents");
    for (dir, manifest) in [
        ("good", r#"{"id": "good", "entry": ["node", "runner.mjs"]}"#),
        ("broken-json", "{not json"),
        ("no-entry", r#"{"id": "no-entry", "entry": []}"#),
        ("no-id", r#"{"entry": ["node"]}"#),
    ] {
        std::fs::create_dir_all(agents.join(dir)).unwrap();
        std::fs::write(agents.join(dir).join("module.json"), manifest).unwrap();
    }
    // Not modules at all, so not problems either.
    std::fs::create_dir_all(agents.join("test")).unwrap();
    std::fs::write(agents.join("README.md"), "").unwrap();

    let (modules, problems) = discover_agents(Some(&root));
    assert_eq!(modules.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["good"]);
    let manifest = |dir: &str| agents.join(dir).join("module.json").display().to_string();
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert!(
        problems.contains(&format!(
            "{}: key must be a string at line 1 column 2",
            manifest("broken-json")
        )),
        "{problems:?}"
    );
    assert!(
        problems.contains(&format!(
            "{}: \"entry\" must be a non-empty array of strings",
            manifest("no-entry")
        )),
        "{problems:?}"
    );
    assert!(
        problems.contains(&format!("{}: missing \"id\"", manifest("no-id"))),
        "{problems:?}"
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn an_agent_setting_naming_an_unknown_skillset_is_refused_at_save_time() {
    let root = std::env::temp_dir().join(format!("colonizer-plugin-dirs-{}", crate::util::short_id()));
    let agent = AgentModule::test("claude-code")
        .dir(root.clone())
        .entry(vec!["node".into()])
        .needs_claude(true)
        .schema(json!({"type": "object", "properties": {"plugins": {"type": "string", "format": "plugin-dirs"}}}));
    let app = crate::tests::test_app_with_agents(&root, vec![agent], |_| {});
    // A skillset needs a manifest to pass validation (plugins::validate).
    std::fs::create_dir_all(app.cfg.data_dir.join("plugins/ecc")).unwrap();
    std::fs::write(app.cfg.data_dir.join("plugins/ecc/plugin.json"), r#"{"name": "ecc"}"#).unwrap();
    let save = |plugins: &str| {
        let mut settings = Map::new();
        settings.insert("plugins".into(), json!(plugins));
        let req = UpdateModule {
            provider: "claude-code".into(),
            enabled: true,
            settings,
            save_anyway: false,
            confirm_content: false,
        };
        update(State(app.clone()), Path("agent".into()), Json(req))
    };
    let err = save("ecc, superpower").await.unwrap_err();
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err.message(), "unknown skillset \"superpower\"; available: ecc");

    let saved = save(" ecc, ,").await.unwrap_or_else(|e| panic!("save refused: {:#}", e.1)).0;
    assert_eq!(saved["settings"]["plugins"], " ecc, ,");
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn enabling_autonomy_with_no_model_is_refused_even_with_save_anyway() {
    let root = std::env::temp_dir().join(format!("colonizer-autonomy-model-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    for provider in ["judge", "full_autonomy"] {
        for save_anyway in [false, true] {
            let req = UpdateModule {
                provider: provider.into(),
                enabled: true,
                settings: Map::from_iter([("model".into(), json!(""))]),
                save_anyway,
                confirm_content: false,
            };
            let err = update(State(app.clone()), Path("autonomy".into()), Json(req))
                .await
                .unwrap_err();
            assert_eq!(err.status(), StatusCode::BAD_REQUEST, "{provider} save_anyway={save_anyway}");
            assert!(
                err.message().starts_with("Autonomous mode needs a model"),
                "{provider} save_anyway={save_anyway}: {}",
                err.message()
            );
        }
    }
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_save_refuses_an_unknown_setting_by_name_but_keeps_stored_ones() {
    let root = std::env::temp_dir().join(format!("colonizer-unknown-key-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    let save = |settings: Map<String, Value>| {
        let req = UpdateModule {
            provider: "github".into(),
            enabled: true,
            settings,
            save_anyway: false,
            confirm_content: false,
        };
        update(State(app.clone()), Path("source".into()), Json(req))
    };
    let mut settings = Map::new();
    settings.insert("include_lables".into(), json!("ready"));
    let err = save(settings.clone()).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert!(
        err.message()
            .starts_with("`include_lables` is not a github setting; known settings: "),
        "{}",
        err.message()
    );

    // What a removed setting leaves behind rides along, whatever the schema now says.
    app.modules
        .write()
        .await
        .get_mut("source")
        .unwrap()
        .settings
        .insert("include_lables".into(), json!("ready"));
    let saved = save(settings).await.unwrap_or_else(|e| panic!("save refused: {:#}", e.1)).0;
    assert_eq!(saved["settings"]["include_lables"], "ready");
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn manifest_problems_found_at_boot_are_listed_on_the_agent_kind() {
    let root = std::env::temp_dir().join(format!("colonizer-manifest-errors-{}", crate::util::short_id()));
    let mut app = crate::tests::test_app(&root);
    let problem = "/app/modules/agents/broken/module.json: missing \"id\"".to_string();
    Arc::get_mut(&mut app).unwrap().agent_problems = vec![problem.clone()];
    let listed = list(State(app)).await.0;
    let agent = listed.iter().find(|k| k["kind"] == "agent").unwrap();
    assert_eq!(agent["manifest_errors"], json!([problem]));
    assert!(
        listed
            .iter()
            .filter(|k| k["kind"] != "agent")
            .all(|k| k.get("manifest_errors").is_none()),
        "only the agent kind has manifests"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn settings_validation_names_unknown_keys_enums_and_types() {
    let schema = providers("sandbox", &[]).remove(0).schema;
    let mut input = Map::new();
    input.insert("cpus".into(), json!(8));
    input.insert("unknwon".into(), json!("x"));
    let err = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap_err();
    assert!(
        err.starts_with("`unknwon` is not a sandbox setting; known settings: ") && err.contains("cpus") && err.contains("preset"),
        "{err}"
    );
    // A key already stored passes: the UI saves back everything modules.json holds, so refusing
    // those would brick every save after a provider switch or a removed setting.
    let mut stored = Map::new();
    stored.insert("unknwon".into(), json!("x"));
    let out = validate_settings("sandbox", &schema, &input, &stored).unwrap();
    assert_eq!(out.get("unknwon"), Some(&json!("x")), "the grandfathered key rides along");

    input.remove("unknwon").unwrap();
    input.insert("cpus".into(), json!(0));
    let err = validate_settings("sandbox", &schema, &input, &stored).unwrap_err();
    assert_eq!(err, "setting `cpus` must be between 1 and 64", "{err}");
    input.insert("cpus".into(), json!("eight"));
    // Being stored buys a key nothing once the schema declares it: `cpus` is checked like any
    // other, and only keys no schema has pass through untouched.
    stored.insert("cpus".into(), json!(4));
    let err = validate_settings("sandbox", &schema, &input, &stored).unwrap_err();
    assert_eq!(err, "setting `cpus` must be an integer", "{err}");
}

#[test]
fn an_enum_refusal_names_the_options() {
    let schema = providers("burn_down", &[]).remove(0).schema;
    let mut input = Map::new();
    input.insert("reset_weekday".into(), json!("Funday"));
    let err = validate_settings("burn_down", &schema, &input, &Map::new()).unwrap_err();
    assert_eq!(
        err, "setting `reset_weekday` must be one of Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday",
        "{err}"
    );
}

#[test]
fn the_sandbox_budget_defaults_to_off_and_rejects_negatives() {
    let schema = providers("sandbox", &[]).remove(0).schema;
    assert_eq!(
        schema["properties"]["budget_usd"]["default"],
        json!(0),
        "no budget unless the operator names one"
    );
    assert_eq!(
        schema["properties"]["budget_tokens"]["default"],
        json!(0),
        "no token budget unless the operator names one"
    );
    let mut input = Map::new();
    input.insert("budget_usd".into(), json!(-1));
    let err = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap_err();
    assert_eq!(err, "setting `budget_usd` must be at least 0", "{err}");
    input.remove("budget_usd");
    input.insert("budget_tokens".into(), json!(-1));
    assert!(validate_settings("sandbox", &schema, &input, &Map::new()).is_err());
    input.remove("budget_tokens");
    input.insert("budget_usd".into(), json!(12.5));
    assert_eq!(
        validate_settings("sandbox", &schema, &input, &Map::new())
            .unwrap()
            .get("budget_usd"),
        Some(&json!(12.5))
    );
}

#[test]
fn the_sandbox_host_disk_quota_is_a_size_and_malformed_ones_are_refused_at_save_time() {
    let schema = providers("sandbox", &[]).remove(0).schema;
    assert_eq!(
        schema["properties"]["host_disk"]["default"],
        json!("0"),
        "no quota unless the operator names one"
    );
    let mut input = Map::new();
    input.insert("host_disk".into(), json!("16G"));
    assert_eq!(
        validate_settings("sandbox", &schema, &input, &Map::new())
            .unwrap()
            .get("host_disk"),
        Some(&json!("16G"))
    );
    input.insert("host_disk".into(), json!(""));
    assert!(
        validate_settings("sandbox", &schema, &input, &Map::new()).is_ok(),
        "empty means unlimited, which is a size"
    );
    for bad in ["eight", "1.5G", "16 GB"] {
        input.insert("host_disk".into(), json!(bad));
        assert!(
            validate_settings("sandbox", &schema, &input, &Map::new()).is_err(),
            "{bad:?} must be refused while the operator is looking"
        );
    }
}

#[test]
fn the_sandbox_egress_lists_are_entry_lists_validated_at_save_time() {
    let schema = providers("sandbox", &[]).remove(0).schema;
    assert_eq!(
        schema["properties"]["egress"]["default"],
        json!("open"),
        "an install that never hears of egress boots as before"
    );
    let mut input = Map::new();
    input.insert("egress".into(), json!("fenced"));
    assert!(validate_settings("sandbox", &schema, &input, &Map::new()).is_err());
    input.insert("egress".into(), json!("open"));
    input.insert(
        "egress_allow".into(),
        json!("api.anthropic.com:443, 10.0.0.0/8, *.example.com"),
    );
    assert_eq!(
        validate_settings("sandbox", &schema, &input, &Map::new())
            .unwrap()
            .get("egress_allow"),
        input.get("egress_allow")
    );
    // A list a boot would have to drop is refused while the operator is looking, with the
    // parser's own word for why.
    input.insert("egress_allow".into(), json!("api.anthropic.com:443, host"));
    let err = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap_err();
    assert!(
        err.starts_with("setting `egress_allow`: `host` is not a host or host:port"),
        "{err}"
    );
}

#[test]
fn the_sandbox_path_lists_take_strings_and_refuse_unusable_paths_at_save_time() {
    let schema = providers("sandbox", &[]).remove(0).schema;
    for key in ["mask_paths", "protect_paths", "unmask_paths"] {
        assert_eq!(schema["properties"][key]["type"], "array", "{key} is an array setting");
        assert_eq!(schema["properties"][key]["default"], json!([]));
    }
    let mut input = Map::new();
    input.insert("mask_paths".into(), json!(["secrets/credentials.json", "vendor/keys/"]));
    input.insert("protect_paths".into(), json!(["tools/run.sh", ".airplane/"]));
    input.insert("unmask_paths".into(), json!([".envrc"]));
    let out = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap();
    assert_eq!(out.get("mask_paths"), input.get("mask_paths"));
    assert_eq!(out.get("protect_paths"), input.get("protect_paths"));

    // A non-array, or an array of non-strings, is the wrong type for the setting, and the
    // refusal says the type the schema asks for.
    for bad in [json!("secrets/credentials.json"), json!(["ok", 4])] {
        input.insert("mask_paths".into(), bad);
        let err = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap_err();
        assert_eq!(err, "setting `mask_paths` must be an array of strings", "{err}");
    }
    // And the entries themselves are checked with the boot's own gate: absolute paths,
    // traversal, the worktree root and — for the masked list only — the git dir.
    let refused = [
        ("mask_paths", "/etc/passwd"),
        ("mask_paths", "../outside"),
        ("mask_paths", "."),
        ("mask_paths", ".git/config"),
        ("protect_paths", "../../outside"),
        ("unmask_paths", "with,comma"),
    ];
    for (key, path) in refused {
        let mut one = Map::new();
        one.insert(key.to_string(), json!([path]));
        let err = validate_settings("sandbox", &schema, &one, &Map::new()).unwrap_err();
        assert!(
            err.starts_with(&format!("setting `{key}` has an unusable path")),
            "{path:?}: {err}"
        );
    }
    // Protecting the git dir is allowed: the read-only mount already covers it, but the
    // operator may underline it.
    let mut protect_git = Map::new();
    protect_git.insert("protect_paths".into(), json!([".git/config"]));
    assert!(validate_settings("sandbox", &schema, &protect_git, &Map::new()).is_ok());
}

#[test]
fn the_sandbox_free_disk_thresholds_are_sizes_validated_like_host_disk() {
    let schema = providers("sandbox", &[]).remove(0).schema;
    assert_eq!(
        schema["properties"]["warn_free_disk"]["default"],
        json!("10G"),
        "the cockpit warns below 10G unless the operator says otherwise"
    );
    assert_eq!(
        schema["properties"]["min_free_disk"]["default"],
        json!("5G"),
        "the queue holds below 5G unless the operator says otherwise"
    );
    let mut input = Map::new();
    input.insert("warn_free_disk".into(), json!("8G"));
    input.insert("min_free_disk".into(), json!("5G"));
    let out = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap();
    assert_eq!(out.get("warn_free_disk"), Some(&json!("8G")));
    assert_eq!(out.get("min_free_disk"), Some(&json!("5G")));
    input.insert("min_free_disk".into(), json!("0"));
    assert_eq!(
        validate_settings("sandbox", &schema, &input, &Map::new())
            .unwrap()
            .get("min_free_disk"),
        Some(&json!("0")),
        "0 turns the floor off"
    );
    for bad in ["eight", "1.5G", "16 GB"] {
        input.insert("warn_free_disk".into(), json!(bad));
        assert!(
            validate_settings("sandbox", &schema, &input, &Map::new()).is_err(),
            "{bad:?} must be refused while the operator is looking"
        );
    }
}

#[test]
fn the_held_colony_timeout_defaults_to_30_minutes_and_rejects_out_of_range() {
    let schema = providers("sandbox", &[]).remove(0).schema;
    assert_eq!(
        schema["properties"]["hold_timeout_minutes"]["default"],
        json!(30),
        "a held colony keeps its slot for half an hour unless the operator says otherwise"
    );
    let mut input = Map::new();
    for bad in [json!(0), json!(1441)] {
        input.insert("hold_timeout_minutes".into(), bad);
        let err = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap_err();
        assert_eq!(
            err, "setting `hold_timeout_minutes` must be between 1 and 1440",
            "the refusal names the bounds: {err}"
        );
    }
    for ok in [1, 30, 1440] {
        input.insert("hold_timeout_minutes".into(), json!(ok));
        assert_eq!(
            validate_settings("sandbox", &schema, &input, &Map::new())
                .unwrap()
                .get("hold_timeout_minutes"),
            Some(&json!(ok))
        );
    }
}

#[test]
fn schemas_are_normalized() {
    assert!(normalize_schema(&json!({"model": {"type": "string"}}))["properties"]["model"].is_object());
    assert!(normalize_schema(&Value::Null)["properties"].is_object());
}

#[test]
fn voice_is_a_kind_whose_services_follow_the_browser() {
    assert!(KINDS.contains(&"voice"));
    let ids: Vec<_> = providers("voice", &[]).into_iter().map(|p| p.id).collect();
    assert_eq!(ids.first().map(String::as_str), Some("browser"));
    assert!(ids.iter().any(|id| id == "openai_compatible"));
    assert!(schema_for("voice", "openai_compatible", &[])["properties"]["base_url"].is_object());
    assert!(
        schema_for("voice", "openai", &[])["properties"]["base_url"].is_null(),
        "a hosted service's URL is not a setting"
    );
    let mut modules = ModulesConfig::default();
    assert!(modules.get("voice").is_none(), "absent until saved: the browser");
    assert_eq!(modules.get_mut("voice").unwrap().provider, "browser");
}

#[test]
fn notify_is_a_kind_and_it_stays_disablable() {
    assert!(KINDS.contains(&"notify"));
    assert!(!is_required("notify"), "announcing colonies to the world is opt-in by design");
    assert!(is_required("source") && is_required("publish"));
}

#[test]
fn burn_down_is_a_kind_and_it_is_opt_in() {
    assert!(KINDS.contains(&"burn_down"));
    assert!(!is_required("burn_down"), "spending a plan is opt-in by design");
    // The settings schema carries its defaults, so the cockpit can render it automatically.
    let schema = providers("burn_down", &[]).remove(0).schema;
    for (key, default) in [
        ("reset_weekday", json!("Monday")),
        ("reset_time", json!("00:00")),
        ("lead_hours", json!(48)),
        ("reserve_pct", json!(5)),
        ("spend_usd_per_colony", json!(5)),
        ("max_live", json!(2)),
        ("repos", json!("")),
        ("instructions", json!("")),
    ] {
        assert_eq!(schema["properties"][key]["default"], default, "{key}");
    }
    assert_eq!(
        schema["properties"]["allowance_usd"].get("default"),
        None,
        "the allowance is the one setting with no default: burn-down must never invent a budget"
    );
    // An invalid weekday saved by hand is refused at save time by the enum check; see
    // `an_enum_refusal_names_the_options` for the exact refusal.
    let mut input = Map::new();
    input.insert("reset_weekday".into(), json!("Funday"));
    assert!(
        validate_settings(
            "burn_down",
            &providers("burn_down", &[]).remove(0).schema,
            &input,
            &Map::new()
        )
        .is_err()
    );
}

#[test]
fn egress_parsing_accepts_an_absent_section_and_names_what_it_refuses() {
    assert_eq!(parse_egress(&json!({})).unwrap(), None);
    // A repeated host keeps one union entry; a wildcard covers subdomains only.
    let section = json!({"api": ["api.anthropic.com", "api.anthropic.com"], "telemetry": ["*.sentry.io"]});
    let egress = parse_egress(&json!({"egress": section}))
        .unwrap()
        .expect("a valid section parses");
    assert_eq!(egress.hosts(), ["api.anthropic.com", "*.sentry.io"]);
    assert!(egress.covers("o447895.sentry.io") && !egress.covers("sentry.io"));
    for (section, expected) in [
        (
            json!({"api": ["https://x"]}),
            r#"egress.api[0]: "https://x" is not a bare hostname"#,
        ),
        (
            json!({"api": [], "foo": []}),
            r#"egress: unknown category "foo"; known: api, auth, telemetry, extra"#,
        ),
        (json!({"api": "x"}), "egress.api must be an array of hostnames"),
    ] {
        assert_eq!(parse_egress(&json!({"egress": section})).unwrap_err(), expected);
    }
}

/// An agent module carrying exactly these `requires`, for the preflight tests below.
fn agent_with(requires: Requires) -> AgentModule {
    AgentModule::test("grok-build").requires(requires)
}

#[test]
fn a_required_binary_no_stock_image_carries_is_refused_naming_the_binary_and_the_fix() {
    let agent = agent_with(Requires {
        binaries: vec!["grok".into()],
        pins: BTreeMap::from([(
            "grok".into(),
            Pin {
                version: "1.0.34".into(),
                install: Some("https://x.ai/cli/install.sh".into()),
            },
        )]),
        ..Default::default()
    });
    let image = crate::presets::pinned_image("node").unwrap();
    let err = check_requires(&agent, &image, &[]).unwrap_err();
    assert!(
        err.contains("agent module `grok-build` needs the `grok` binary (pinned 1.0.34)")
            && err.contains(&image)
            && err.contains("set the sandbox module's image to one with grok on PATH")
            && err.contains("install: https://x.ai/cli/install.sh"),
        "{err}"
    );
    // The bare tag is the same stock image, and a pin without an install command refuses just
    // the same, without pretending to know how to install the binary.
    assert!(check_requires(&agent, "node:24-bookworm", &[]).is_err());
    let unpinned_install = agent_with(Requires {
        binaries: vec!["hermes".into()],
        ..Default::default()
    });
    let err = check_requires(&unpinned_install, "rust:1-bookworm", &[]).unwrap_err();
    assert!(
        err.contains("needs the `hermes` binary, which") && !err.contains("install:") && !err.contains("pinned"),
        "{err}"
    );
}

#[test]
fn a_required_binary_a_custom_image_may_carry_is_trusted_until_the_vm_checks() {
    let agent = agent_with(Requires {
        binaries: vec!["grok".into()],
        ..Default::default()
    });
    assert!(
        check_requires(&agent, "ghcr.io/me/grok-toolchain:1", &[]).is_ok(),
        "a custom image is the operator's word; the runner's in-VM preflight still checks"
    );
}

#[test]
fn a_base_tool_every_stock_preset_carries_passes_the_check() {
    let wget = agent_with(Requires {
        binaries: vec!["wget".into()],
        ..Default::default()
    });
    assert!(
        check_requires(&wget, &crate::presets::pinned_image("node").unwrap(), &[]).is_ok(),
        "the Debian base tools ride every stock preset, whatever else it ships"
    );
    // The list is what the presets share, not what the widest one carries: `git` ships on the
    // node, python and go images but not the rust one, so it still refuses — without claiming
    // the configured image carries nothing like it.
    let git = agent_with(Requires {
        binaries: vec!["git".into()],
        ..Default::default()
    });
    let image = crate::presets::pinned_image("node").unwrap();
    let err = check_requires(&git, &image, &[]).unwrap_err();
    assert!(err.contains("do not all carry") && err.contains(&image), "{err}");
}

#[test]
fn a_runner_fetched_binary_passes_whatever_the_image() {
    let agent = agent_with(Requires {
        binaries: vec!["opencode".into()],
        fetched_by_runner: vec!["opencode".into()],
        ..Default::default()
    });
    assert!(check_requires(&agent, &crate::presets::pinned_image("node").unwrap(), &[]).is_ok());
}

#[test]
fn the_staged_claude_passes_and_a_mismatching_pin_is_refused_naming_both_versions() {
    let pinned = |version: &str| {
        agent_with(Requires {
            binaries: vec!["claude".into()],
            pins: BTreeMap::from([(
                "claude".into(),
                Pin {
                    version: version.into(),
                    install: None,
                },
            )]),
            ..Default::default()
        })
    };
    let staged = [StagedBinary {
        name: "claude".into(),
        version: Some("2.1.280".into()),
    }];
    let image = crate::presets::pinned_image("node").unwrap();
    // The shipped claude-code module pins nothing: the staged binary is what it needs.
    assert!(check_requires(&pinned("2.1.280"), &image, &staged).is_ok());
    assert!(
        check_requires(
            &agent_with(Requires {
                binaries: vec!["claude".into()],
                ..Default::default()
            }),
            &image,
            &staged
        )
        .is_ok()
    );
    let err = check_requires(&pinned("9.9.9"), &image, &staged).unwrap_err();
    assert!(
        err.contains("pins the `claude` binary at 9.9.9, but the harness stages 2.1.280"),
        "{err}"
    );
    // A staged version the harness does not know — the host's own install standing in for a
    // missing vendored build — cannot be pin-checked, so it is not.
    let unknown = [StagedBinary {
        name: "claude".into(),
        version: None,
    }];
    assert!(check_requires(&pinned("9.9.9"), &image, &unknown).is_ok());
}

#[test]
fn a_malformed_requires_section_is_a_manifest_problem_by_path_and_cause() {
    let root = std::env::temp_dir().join(format!("colonizer-requires-{}", crate::util::short_id()));
    let agents = root.join("modules/agents");
    for (dir, manifest) in [
        (
            "bad-binaries",
            r#"{"id": "x", "entry": ["node"], "requires": {"binaries": "claude"}}"#,
        ),
        ("bad-pins", r#"{"id": "x", "entry": ["node"], "requires": {"pins": []}}"#),
        (
            "pin-no-version",
            r#"{"id": "x", "entry": ["node"], "requires": {"pins": {"claude": {}}}}"#,
        ),
        (
            "bad-marker",
            r#"{"id": "x", "entry": ["node"], "requires": {"fetched_by_runner": "opencode"}}"#,
        ),
        ("not-an-object", r#"{"id": "x", "entry": ["node"], "requires": "claude"}"#),
    ] {
        std::fs::create_dir_all(agents.join(dir)).unwrap();
        std::fs::write(agents.join(dir).join("module.json"), manifest).unwrap();
    }
    let (modules, problems) = discover_agents(Some(&root));
    assert!(modules.is_empty(), "{:?}", modules.iter().map(|m| &m.id));
    assert_eq!(problems.len(), 5, "{problems:?}");
    let manifest = |dir: &str| agents.join(dir).join("module.json").display().to_string();
    assert!(
        problems.contains(&format!(
            "{}: requires.binaries must be an array of strings",
            manifest("bad-binaries")
        )),
        "{problems:?}"
    );
    assert!(
        problems.contains(&format!(
            "{}: requires.pins must be an object of package or binary name to pin",
            manifest("bad-pins")
        )),
        "{problems:?}"
    );
    assert!(
        problems.contains(&format!(
            "{}: requires.pins.claude must name a string \"version\"",
            manifest("pin-no-version")
        )),
        "{problems:?}"
    );
    assert!(
        problems.contains(&format!(
            "{}: requires.fetched_by_runner must be an array of strings",
            manifest("bad-marker")
        )),
        "{problems:?}"
    );
    assert!(
        problems.contains(&format!("{}: requires must be an object", manifest("not-an-object"))),
        "{problems:?}"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn the_claude_lock_version_reads_the_agent_rows_only() {
    let version = claude_lock_version(CLAUDE_LOCK).expect("the shipped lock pins a Claude Code build");
    assert!(!version.is_empty(), "{version}");
    const SAMPLE: &str = "\
# a comment naming the row that is not an entry
claude-code  2.1.280  linux-arm64  agent  aa  https://x/arm64
claude-code  2.1.280  linux-x64    agent  bb  https://x/x64
other        9.9.9    linux-x64    agent  cc  https://x/other
";
    assert_eq!(claude_lock_version(SAMPLE), Some("2.1.280"));
    assert_eq!(
        claude_lock_version("claude-code 2.1.280 linux-x64 binary aa https://x/x64"),
        None,
        "a non-agent kind never matches"
    );
    assert_eq!(claude_lock_version(""), None);
}

#[test]
fn the_harness_stages_claude_at_the_lock_version_only_when_the_guest_build_is_installed() {
    let mut cfg = crate::config::Settings {
        bind: "127.0.0.1:0".into(),
        data_dir: PathBuf::new(),
        config_dir: PathBuf::new(),
        runtime_dir: PathBuf::new(),
        assets: None,
        msb: "msb".into(),
        claude_bin: None,
        gateway_bind: "127.0.0.1:0".parse().unwrap(),
        allowed_hosts: Vec::new(),
        fleet_peers: Vec::new(),
        bench_pool: None,
    };
    let staged = harness_staged_binaries(&cfg);
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].name, "claude");
    assert_eq!(staged[0].version, None, "no vendored build, no version to pin-check");
    let dir = std::env::temp_dir().join(format!("colonizer-staged-claude-{}", crate::util::short_id()));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/claude-guest"), b"\x7fELF padding").unwrap();
    cfg.assets = Some(dir.clone());
    let staged = harness_staged_binaries(&cfg);
    assert_eq!(
        staged[0].version.as_deref(),
        claude_lock_version(CLAUDE_LOCK),
        "the vendored build is the lock's"
    );
    std::fs::remove_dir_all(dir).ok();
}
