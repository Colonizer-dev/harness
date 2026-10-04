//! The shipped agent manifests and the exec-policy rule (moved from `sessions/agentd.rs`).

use colonizer_harness::contract::applies_exec_policy;
use serde_json::Value;

#[test]
fn exactly_claude_code_and_acp_apply_the_exec_policy() {
    // The rule is the manifest, not the runner on disk: a module applies the exec policy iff its
    // settings schema declares an `exec_policy` property. A new agent module has to take a side
    // — apply the policy, or be named here as one a set policy refuses at boot.
    let agents = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/agents");
    let mut applies = Vec::new();
    let mut refuses = Vec::new();
    for entry in std::fs::read_dir(&agents).unwrap().flatten() {
        let dir = entry.path();
        if !dir.join("module.json").is_file() {
            continue;
        }
        let id = dir.file_name().unwrap().to_string_lossy().into_owned();
        let manifest: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("module.json")).unwrap())
            .unwrap_or_else(|e| panic!("modules/agents/{id}/module.json: {e}"));
        if applies_exec_policy(&manifest["settings"]) {
            applies.push(id);
        } else {
            refuses.push(id);
        }
    }
    applies.sort();
    refuses.sort();
    assert_eq!(applies, ["acp", "claude-code"]);
    assert_eq!(refuses, ["codex", "grok-build", "hermes", "opencode", "pi"]);
}
