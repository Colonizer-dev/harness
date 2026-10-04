//! Preset detection against a shipped agent manifest (moved from `presets.rs`).

use colonizer_harness::contract::{detect, find};
use serde_json::Value;

#[test]
fn a_rust_repo_boots_the_rust_image_while_the_runner_still_needs_node() {
    // The mismatch issue #249 fixes, both halves in one place: a `Cargo.toml`
    // repo detects as `rust`, whose preset image is the Rust toolchain image
    // rather than the Node one — while the agent entry still starts with
    // `node` (`modules/agents/claude-code/module.json`), which only works
    // because every colony also ships the vendored Node runtime.
    let d = detect(&["Cargo.toml"]).expect("Cargo.toml is a marker");
    assert_eq!(d.stack, "rust");
    let rust = find("rust").expect("rust is a preset");
    let node = find("node").expect("node is a preset");
    assert_ne!(rust.image, node.image, "the rust stack must not boot the node image");
    assert!(rust.image.starts_with("rust:"), "unexpected rust image {}", rust.image);

    const MANIFEST: &str = include_str!("../../../modules/agents/claude-code/module.json");
    let manifest: Value = serde_json::from_str(MANIFEST).expect("the agent manifest parses");
    let entry: Vec<&str> = manifest["entry"]
        .as_array()
        .expect("the agent manifest names an entry command")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(entry.first(), Some(&"node"), "the runner still starts with `node`: {entry:?}");
}
