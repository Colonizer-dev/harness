//! The fleet-export schema against the constants the code writes (moved from `fleet_export.rs`).

use colonizer_harness::contract::{BUNDLE_FORMAT, BUNDLE_VERSION};
use serde_json::{Value, json};

#[test]
fn the_published_schema_pins_format_and_version() {
    let schema: Value =
        serde_json::from_str(include_str!("../../../docs/fleet-export.schema.json")).expect("docs/fleet-export.schema.json");
    assert_eq!(schema["properties"]["format"]["const"], json!(BUNDLE_FORMAT));
    assert_eq!(schema["properties"]["version"]["const"], json!(BUNDLE_VERSION));
}
