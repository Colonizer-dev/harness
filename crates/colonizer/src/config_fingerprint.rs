//! Configuration fingerprint at boot time, capturing the resolved state of the harness and colony.
//!
//! What a colony booted under (#1352): one digest per component so a bench or evolve comparison can name what differs,
//! and one over all of them. Each component's digest is sha256 of its JSON value (keys sorted),
//! and the overall digest is sha256 of sorted component digests as `name=digest\n` lines.

use crate::util;
use anyhow::Result;
use ring::digest::{SHA256, digest};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// The complete configuration fingerprint recorded on a session.
/// One digest per component so comparisons can name what differs, and one over all of them.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ConfigFingerprint {
    pub digest: String,
    pub components: BTreeMap<String, ComponentDigest>,
}

/// A single component's digest and its resolved value.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ComponentDigest {
    pub digest: String,
    pub value: Value,
}

/// Input builder for config fingerprint computation.
/// Gathers resolved harness state: versions, agent module, plugins, model, policies, and egress rules.
pub(crate) struct Inputs<'a> {
    pub harness_version: &'a str,
    pub harness_commit: Option<&'a str>,
    pub agent_id: &'a str,
    pub agent_module_dir: &'a Path,
    pub plugins: &'a [(String, Option<String>)],
    pub model_tier: Option<&'a str>,
    pub model_name: Option<&'a str>,
    pub model_override: Option<&'a str>,
    pub autonomy: &'a Value,
    pub exec_policy_install: Option<&'a str>,
    pub exec_policy_org: Option<&'a str>,
    pub exec_policy_repo: Option<&'a str>,
    pub egress_resolved: &'a Value,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest_value = digest(&SHA256, bytes);
    let hex = util::hex(digest_value.as_ref());
    format!("sha256:{}", hex)
}

/// Hash text to "sha256:<hex>", or return null for missing values.
pub(crate) fn text_hash(text: Option<&str>) -> Option<String> {
    text.map(|t| sha256_hex(t.as_bytes()))
}

fn digest_value(value: &Value) -> Result<String> {
    let sorted = sort_value(value);
    let json_str = serde_json::to_string(&sorted)?;
    Ok(sha256_hex(json_str.as_bytes()))
}

fn sort_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted = Map::new();
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            for key in keys {
                if let Some(val) = map.get(&key) {
                    sorted.insert(key, sort_value(val));
                }
            }
            Value::Object(sorted)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sort_value).collect()),
        other => other.clone(),
    }
}

/// The same walk scripts/bench.mjs `moduleDirDigest` does, so evolve can recompute a module's digest from a commit plus a diff.
pub(crate) fn dir_digest(dir: &Path) -> Result<String> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    fn collect(dir: &Path, base: &Path, files: &mut Vec<(String, Vec<u8>)>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if matches!(name_str.as_ref(), ".DS_Store" | "__pycache__" | "node_modules" | ".git") {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                collect(&entry.path(), base, files)?;
            } else if file_type.is_file() {
                let path = entry.path();
                let rel = path.strip_prefix(base)?.to_string_lossy().to_string().replace("\\", "/");
                files.push((rel, fs::read(&path)?));
            }
        }
        Ok(())
    }
    collect(dir, dir, &mut files)?;
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut hasher = ring::digest::Context::new(&SHA256);
    for (path, content) in files {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(content.len().to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(&content);
    }
    let hex = util::hex(hasher.finish().as_ref());
    Ok(format!("sha256:{}", hex))
}

/// Compute the configuration fingerprint from resolved inputs.
/// Returns one digest per component so a bench or evolve comparison can name what differs,
/// and one over all of them.
pub(crate) fn compute(inputs: &Inputs) -> Result<ConfigFingerprint> {
    let mut components = BTreeMap::new();
    macro_rules! add {
        ($k:expr, $v:expr) => {{
            let v = $v;
            let d = digest_value(&v)?;
            components.insert($k.to_string(), ComponentDigest { digest: d, value: v });
        }};
    }

    add!(
        "harness",
        json!({
            "version": inputs.harness_version,
            "commit": inputs.harness_commit
        })
    );

    let module_digest = dir_digest(inputs.agent_module_dir)
        .map_err(|e| {
            tracing::warn!(path = %inputs.agent_module_dir.display(), error = %e, "failed to compute agent module digest");
            e
        })
        .ok();
    add!("agent", json!({ "id": inputs.agent_id, "module_digest": module_digest }));

    let mut plugins_sorted = inputs.plugins.to_vec();
    plugins_sorted.sort_by(|a, b| a.0.cmp(&b.0));
    add!(
        "plugins",
        Value::Array(plugins_sorted.iter().map(|(n, p)| json!({ "name": n, "pin": p })).collect())
    );

    add!(
        "model",
        json!({
            "tier": inputs.model_tier,
            "model": inputs.model_name,
            "override": inputs.model_override
        })
    );

    add!(
        "policy",
        json!({
            "autonomy": inputs.autonomy,
            "exec_policy": {
                "install": text_hash(inputs.exec_policy_install),
                "org": text_hash(inputs.exec_policy_org),
                "repo": text_hash(inputs.exec_policy_repo)
            }
        })
    );

    add!("egress", json!({ "resolved": inputs.egress_resolved }));

    let mut concat = String::new();
    for (name, comp) in &components {
        concat.push_str(name);
        concat.push('=');
        concat.push_str(&comp.digest);
        concat.push('\n');
    }

    Ok(ConfigFingerprint {
        digest: sha256_hex(concat.as_bytes()),
        components,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn dir_digest_fixture_constant() {
        let tmpdir = std::env::temp_dir().join(format!("colonizer-cfp-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmpdir).unwrap();
        fs::write(tmpdir.join("a.txt"), "hello\n").unwrap();
        fs::create_dir(tmpdir.join("sub")).unwrap();
        fs::write(tmpdir.join("sub/b.md"), "world").unwrap();
        fs::write(tmpdir.join(".DS_Store"), "x").unwrap();
        fs::create_dir(tmpdir.join("sub/__pycache__")).unwrap();
        fs::write(tmpdir.join("sub/__pycache__/c.pyc"), "y").unwrap();
        assert_eq!(
            dir_digest(&tmpdir).unwrap(),
            "sha256:ef29960db3e92cd0c60428da998e6cb617c4dc9f6a9b2777ac7dba134af0cc2f"
        );
        fs::remove_dir_all(&tmpdir).unwrap();
    }

    #[test]
    fn config_round_trip() {
        let fp = ConfigFingerprint {
            digest: "sha256:abc".to_string(),
            components: {
                let mut m = BTreeMap::new();
                m.insert(
                    "test".to_string(),
                    ComponentDigest {
                        digest: "sha256:def".to_string(),
                        value: json!({"key": "value"}),
                    },
                );
                m
            },
        };
        let json = serde_json::to_value(&fp).unwrap();
        let roundtrip: ConfigFingerprint = serde_json::from_value(json).unwrap();
        assert_eq!(fp, roundtrip);
    }

    #[test]
    fn session_deserializes_without_config() {
        let j = r#"{"id":"t","repo":"t","org":"t","issue_title":"","instructions":"","status":"stopped","branch":"","worktree":"","sandbox":"","agent":"","created_at":"1970-01-01T00:00:00Z","updated_at":"1970-01-01T00:00:00Z"}"#;
        let s: crate::sessions::Session = serde_json::from_str(j).unwrap();
        assert!(s.config.is_none());
    }

    #[test]
    fn component_digest_independence() {
        let tmpdir = std::env::temp_dir().join(format!("colonizer-cfp-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmpdir).unwrap();
        fs::write(tmpdir.join("test.txt"), "content").unwrap();

        let plugins = vec![("plugin1".to_string(), Some("v1.0".to_string()))];
        let autonomy = json!(false);
        let egress = json!({"enabled": false});

        let base_inputs = Inputs {
            harness_version: "1.0.0",
            harness_commit: Some("abc123"),
            agent_id: "test-agent",
            agent_module_dir: &tmpdir,
            plugins: &plugins,
            model_tier: Some("medium"),
            model_name: Some("claude-3-sonnet"),
            model_override: None,
            autonomy: &autonomy,
            exec_policy_install: None,
            exec_policy_org: None,
            exec_policy_repo: None,
            egress_resolved: &egress,
        };

        let base_fp = compute(&base_inputs).unwrap();

        // Change model_name and verify only model component digest changes
        let variant_inputs = Inputs {
            model_name: Some("claude-3-opus"),
            ..base_inputs
        };
        let variant_fp = compute(&variant_inputs).unwrap();

        // Overall digest should differ
        assert_ne!(
            base_fp.digest, variant_fp.digest,
            "overall digest should change when model_name changes"
        );

        // Only model component should differ
        let mut changed_components = 0;
        for (name, base_comp) in &base_fp.components {
            if let Some(variant_comp) = variant_fp.components.get(name)
                && base_comp.digest != variant_comp.digest
            {
                changed_components += 1;
                assert_eq!(name, "model", "only model component should have different digest");
            }
        }
        assert_eq!(changed_components, 1, "exactly one component digest should change");

        fs::remove_dir_all(&tmpdir).unwrap();
    }
}
