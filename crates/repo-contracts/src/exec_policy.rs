//! The exec-policy save check against the runner's parser (issue #924): one fixture, driven here
//! against `exec_policy::validate` and in `modules/agents/claude-code/test/execpolicy.test.mjs`
//! against `parsePolicy`.

use colonizer_harness::contract::validate_exec_policy;
use serde_json::Value;

#[test]
fn the_mothership_accepts_exactly_the_policies_the_runner_keeps_whole() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../modules/agents/claude-code/test/fixtures/execpolicy-valid.json");
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() > 10, "the fixture lost its cases");
    for case in cases {
        let policy = case["policy"].as_str().unwrap();
        let valid = case["valid"].as_bool().unwrap();
        assert_eq!(
            validate_exec_policy(policy).is_ok(),
            valid,
            "{policy}: {:?}",
            validate_exec_policy(policy)
        );
    }
}
