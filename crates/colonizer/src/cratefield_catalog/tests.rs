//! Tests for the Cratefield module catalogue.

use super::*;

#[test]
fn bundled_snapshot_has_10_modules() {
    let snapshot = &SNAPSHOT;
    assert_eq!(snapshot.modules.len(), 10, "bundled snapshot should have 10 modules");
}

#[test]
fn compute_status_published_when_registry_version_present() {
    let status = compute_status(Some("0.1.0"), Some("0.1.0"));
    assert!(matches!(status, ModuleStatus::Published));
}

#[test]
fn compute_status_unreleased_when_only_catalogue_version_present() {
    let status = compute_status(Some("0.1.0"), None);
    assert!(matches!(status, ModuleStatus::Unreleased));
}

#[test]
fn compute_status_planned_when_no_versions() {
    let status = compute_status(None, None);
    assert!(matches!(status, ModuleStatus::Planned));
}

#[test]
fn registry_source_selection_prefers_live() {
    let live = std::collections::HashMap::from([("test".to_string(), "0.1.0".to_string())]);
    let cached = std::collections::HashMap::from([("test".to_string(), "0.0.9".to_string())]);
    let (source, _) = select_registry_source(Some(&live), Some(&cached));
    assert!(matches!(source, Source::Live));
}

#[test]
fn registry_source_selection_falls_back_to_cache() {
    let cached = std::collections::HashMap::from([("test".to_string(), "0.0.9".to_string())]);
    let (source, _) = select_registry_source(None, Some(&cached));
    assert!(matches!(source, Source::Cache));
}

#[test]
fn registry_source_selection_falls_back_to_snapshot() {
    let (source, _) = select_registry_source(None, None);
    assert!(matches!(source, Source::Snapshot));
}

#[test]
fn module_links_serializes_crate_as_crate() {
    let links = make_links("test-crate");
    let json = serde_json::to_value(&links).unwrap();
    assert!(json.get("crate").is_some());
    assert!(json.get("crate_ref").is_none());
}

#[test]
fn small_catalogue_parses() {
    let json_str = r#"{
        "modules": [{
            "slug": "test",
            "name": "Test",
            "summary": "A test module",
            "depends_on": [],
            "releases": [{"version": "0.1.0"}],
            "detail": {
                "crate_name": "test-module",
                "needs": ["Database"],
                "optional": [],
                "tables": []
            }
        }]
    }"#;
    let cat: CatalogueJson = serde_json::from_str(json_str).expect("should parse");
    assert_eq!(cat.modules.len(), 1);
    assert_eq!(cat.modules[0].slug, "test");
}
