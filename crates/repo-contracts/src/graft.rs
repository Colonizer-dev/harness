//! The staged graft plugin bundle (moved from `graft.rs`).

use colonizer_harness::contract::validate;
use std::path::Path;

#[test]
fn the_staged_plugin_is_a_valid_skillset() {
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/graft-bundle/plugin");
    validate(&plugin).unwrap();
    let skill = std::fs::read_to_string(plugin.join("skills/graft/SKILL.md")).unwrap();
    for rule in ["graft ask", "--source", "graft grep", "--depth all", "graft skeleton", "head"] {
        assert!(skill.contains(rule), "SKILL.md should teach {rule:?}");
    }
    let wrapper = std::fs::read_to_string(plugin.join("bin/graft")).unwrap();
    assert!(
        wrapper.contains("node/bin/node"),
        "the wrapper runs graft with the bundle's own node"
    );
    assert!(wrapper.contains("DO_NOT_TRACK=1"), "graft's telemetry stays closed");
    assert!(
        wrapper.contains("-u ANTHROPIC_API_KEY") && wrapper.contains("-u OPENAI_API_KEY"),
        "no model key reaches graft"
    );
}
