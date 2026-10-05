//! The shipped agent manifests against discovery, presets and the docs (moved from `modules.rs`).

use colonizer_harness::contract::{
    DeclaredSecret, Requires, check_requires, discover_agents, parse_requires, pinned_image, read_agent, short_id,
};
use serde_json::{Value, json};
use std::path::Path;

#[test]
fn the_pi_manifest_is_discovered_as_an_agent_that_needs_no_claude() {
    // `modules/agents/pi/module.json` ships beside claude-code's and must ride the
    // same discovery: still a `node` runner to mount, but an agent that reaches
    // models only through the provider gateway — no `claude` binary and no Claude
    // login — so it cannot need Claude, and its one model setting is the whole split.
    const MANIFEST: &str = include_str!("../../../modules/agents/pi/module.json");
    let root = std::env::temp_dir().join(format!("colonizer-pi-manifest-{}", short_id()));
    let dir = root.join("modules/agents/pi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("module.json"), MANIFEST).unwrap();
    std::fs::write(dir.join("runner.mjs"), "// test fixture").unwrap();
    let (modules, problems) = discover_agents(Some(&root));
    assert!(problems.is_empty(), "{problems:?}");
    let pi = modules.iter().find(|m| m.id == "pi").expect("the pi manifest is discovered");
    assert!(!pi.needs_claude, "pi holds no claude binary and no Claude credential");
    assert!(!pi.loop_tools, "pi serves no colonizer MCP server, so no loop tools");
    assert_eq!(pi.vm_command(), ["node", "/opt/colonizer/agent/runner.mjs"]);
    assert_eq!(pi.schema["properties"]["model"]["env"], "COLONIZER_MODEL");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn the_loop_tools_flag_is_parsed_from_the_manifest() {
    // The loop tools (`loop_next`, `loop_stop`) are a per-module capability: the manifest
    // declares them, and a loop's brief only names them when the module does (issue #643).
    let dir = std::env::temp_dir().join(format!("colonizer-loop-tools-{}", short_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("module.json");
    let write = |manifest: &str| std::fs::write(&path, manifest).unwrap();

    write(include_str!("../../../modules/agents/claude-code/module.json"));
    assert!(read_agent(&path).unwrap().loop_tools, "claude-code serves the loop tools");
    write(r#"{"id": "x", "entry": ["node", "runner.mjs"], "loop_tools": true}"#);
    assert!(read_agent(&path).unwrap().loop_tools);
    // Anything but a boolean is a manifest problem, not a silent default.
    write(r#"{"id": "x", "entry": ["node", "runner.mjs"], "loop_tools": "yes"}"#);
    assert_eq!(read_agent(&path).unwrap_err(), "\"loop_tools\" must be a boolean");
    // And absent stays the default: no declaration, no loop tools.
    write(r#"{"id": "x", "entry": ["node", "runner.mjs"]}"#);
    assert!(!read_agent(&path).unwrap().loop_tools);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn manifests_that_declare_a_vendor_host_expose_its_secret_for_the_boot_push() {
    // The runner-wire agents (issue #629) each name one vendor host in their manifest's
    // `secrets`: that declaration is the whole trigger for pushing a gateway key, so
    // discovery must distil exactly the vendor-facing entries from whatever else the
    // manifest grants. An agent without a vendor host stays empty.
    const CODEX: &str = include_str!("../../../modules/agents/codex/module.json");
    const GROK: &str = include_str!("../../../modules/agents/grok-build/module.json");
    const PI: &str = include_str!("../../../modules/agents/pi/module.json");
    let root = std::env::temp_dir().join(format!("colonizer-vendor-manifests-{}", short_id()));
    for (dir, manifest) in [("codex", CODEX), ("grok-build", GROK), ("pi", PI)] {
        let dir = root.join("modules/agents").join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("module.json"), manifest).unwrap();
        std::fs::write(dir.join("runner.mjs"), "// test fixture").unwrap();
    }
    let (modules, problems) = discover_agents(Some(&root));
    assert!(problems.is_empty(), "{problems:?}");
    let secrets = |id: &str| {
        modules
            .iter()
            .find(|m| m.id == id)
            .expect("the manifest is discovered")
            .vendor_secrets
            .clone()
    };
    assert_eq!(
        secrets("codex"),
        vec![DeclaredSecret {
            env: vec!["CODEX_API_KEY".into()],
            hosts: vec!["api.openai.com".into()],
        }],
        "{:?}",
        secrets("codex")
    );
    assert_eq!(
        secrets("grok-build"),
        vec![DeclaredSecret {
            env: vec!["XAI_API_KEY".into()],
            hosts: vec!["api.x.ai".into()],
        }],
        "{:?}",
        secrets("grok-build")
    );
    assert!(secrets("pi").is_empty(), "pi declares no vendor host");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn every_shipped_agent_module_declares_an_egress_that_covers_its_secret_hosts() {
    // CI enforcement (#304): a shipped module without the declaration is a named failure, and
    // one whose secrets reach hosts it does not declare is refused by read_agent itself.
    let agents = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/agents");
    let mut checked = 0;
    for entry in std::fs::read_dir(&agents).unwrap().flatten() {
        let dir = entry.path();
        if !dir.join("module.json").is_file() {
            continue;
        }
        let id = dir.file_name().unwrap().to_string_lossy();
        let egress = read_agent(&dir.join("module.json"))
            .unwrap_or_else(|e| panic!("modules/agents/{id}/module.json: {e}"))
            .egress
            .unwrap_or_else(|| panic!("modules/agents/{id}/module.json: missing egress declaration"));
        assert!(egress.api.iter().all(|host| egress.covers(host)) && !egress.hosts().iter().any(String::is_empty));
        checked += 1;
    }
    assert!(checked >= 6, "expected the six shipped agent modules, walked {checked}");
}

#[test]
fn every_shipped_agent_module_appears_in_the_provider_compatibility_table() {
    // CI enforcement (#304): a shipped runner missing from the docs is a named failure, so the
    // connection → backends table in docs/providers.md cannot silently rot when an agent module
    // is added.
    const TABLE: &str = include_str!("../../../docs/providers.md");
    let agents = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/agents");
    let mut listed = 0;
    for entry in std::fs::read_dir(&agents).unwrap().flatten() {
        let dir = entry.path();
        if !dir.join("module.json").is_file() {
            continue;
        }
        let id = dir.file_name().unwrap().to_string_lossy();
        let row = format!("| `{id}` |");
        assert!(
            TABLE.contains(&row),
            "docs/providers.md: no compatibility-table row for `{id}`; add the runner to the connection → backends table (a line starting with \"{row}\")"
        );
        listed += 1;
    }
    assert!(listed >= 6, "expected the six shipped agent modules, walked {listed}");
}

#[test]
fn the_requires_section_parses_binaries_pins_and_the_runner_fetched_marker() {
    // The shipped grok-build manifest: one required binary, pinned with an install command and
    // a source_rev the preflight has no use for.
    const GROK: &str = include_str!("../../../modules/agents/grok-build/module.json");
    let grok: Value = serde_json::from_str(GROK).unwrap();
    let requires = parse_requires(&grok).unwrap();
    assert_eq!(requires.binaries, ["grok"]);
    let pin = requires.pins.get("grok").expect("the grok pin parses");
    assert_eq!(pin.version, "1.0.34");
    assert_eq!(pin.install.as_deref(), Some("https://x.ai/cli/install.sh"));
    // acp pins a package name that is not a required binary: carried as declared, matched to
    // no binary by the preflight. The gemini preset's CLI is fetched by the runner, so the
    // fetched marker must list it and the preflight must leave it alone.
    const ACP: &str = include_str!("../../../modules/agents/acp/module.json");
    let acp: Value = serde_json::from_str(ACP).unwrap();
    let requires = parse_requires(&acp).unwrap();
    assert_eq!(requires.binaries, ["gemini"]);
    assert_eq!(requires.pins.keys().next().map(String::as_str), Some("@google/gemini-cli"));
    assert!(!requires.pins.contains_key("gemini"), "{:?}", requires.pins);
    assert_eq!(requires.fetched_by_runner, ["gemini"]);
    // opencode fetches its own binary at runtime, so the preflight must leave it alone.
    const OPENCODE: &str = include_str!("../../../modules/agents/opencode/module.json");
    let opencode: Value = serde_json::from_str(OPENCODE).unwrap();
    let requires = parse_requires(&opencode).unwrap();
    assert_eq!(requires.fetched_by_runner, ["opencode"]);
    assert_eq!(requires.binaries, ["opencode"]);
    // No section at all parses as none; pi declares none of the checked keys.
    assert_eq!(parse_requires(&json!({})).unwrap(), Requires::default());
    const PI: &str = include_str!("../../../modules/agents/pi/module.json");
    let pi: Value = serde_json::from_str(PI).unwrap();
    assert_eq!(parse_requires(&pi).unwrap(), Requires::default());
}

/// Issue #602: the Hermes runner builds its pinned hermes-agent on first boot (stage.mjs), so the
/// harness must launch a Hermes colony on every stock preset instead of refusing it for want of a
/// `hermes` binary in the image.
#[test]
fn the_hermes_module_launches_on_every_stock_preset() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/agents/hermes/module.json");
    let hermes = read_agent(&path).unwrap();
    assert_eq!(hermes.requires.binaries, ["hermes"]);
    assert_eq!(hermes.requires.fetched_by_runner, ["hermes"]);
    for preset in ["node", "python", "rust", "go"] {
        let image = pinned_image(preset).unwrap();
        assert_eq!(check_requires(&hermes, &image, &[]), Ok(()), "{preset}: {image}");
    }
}

/// The boot reads `relaunch_subagents` with `unwrap_or(true)`, and boot.rs's own test reads a schema
/// with that default: the shipped manifest must say the same (moved from `boot.rs`, #756).
#[test]
fn the_claude_code_manifest_ships_relaunch_subagents_on() {
    let manifest: Value = serde_json::from_str(include_str!("../../../modules/agents/claude-code/module.json")).unwrap();
    assert_eq!(
        manifest["settings"]["properties"]["relaunch_subagents"]["type"],
        json!("boolean")
    );
    assert_eq!(
        manifest["settings"]["properties"]["relaunch_subagents"]["default"],
        json!(true)
    );
}
