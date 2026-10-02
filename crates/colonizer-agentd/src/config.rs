//! `/colonizer/session.json` (docs/protocol.md §1).

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Deserialize)]
pub struct SessionConfig {
    #[serde(default)]
    pub session_id: String,
    #[serde(default = "default_workspace")]
    pub workspace: PathBuf,
    #[serde(default = "default_listen")]
    pub listen: String,
    pub agent: AgentConfig,
    #[serde(default)]
    pub initial_prompt: Option<String>,
    /// Present only on a resume boot (issue #700): the services to bring back before the brief.
    #[serde(default)]
    pub restore: Option<Restore>,
}

/// `restore`: the host's record of what ran before a suspension, so the guest can relaunch it and
/// say what happened. Absent from every fresh boot's session.json, and optional field by field so
/// records the agent wrote under an earlier contract still parse.
#[derive(Debug, Deserialize)]
pub struct Restore {
    pub suspended: bool,
    #[serde(default)]
    pub services: Vec<ServiceSpec>,
}

/// One service to relaunch on resume — or, with `restart: false`, to report as lost. `cmd` runs
/// via `sh -c`; `ready` is a decimal TCP port or an http(s) URL; `env` lists variable NAMES to
/// pass from the colony env, never values. Serialize is how a `colonizer-svc start` record is
/// written: what the host folds into the next resume's `restore` block is exactly this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceSpec {
    pub name: String,
    pub cmd: String,
    /// Relative to the worktree (the runner's directory).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    pub timeout_secs: u64,
    pub restart: bool,
    /// `manifest` (the host's declaration), `registered` (`colonizer-svc`) or `background`.
    pub source: String,
}

pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 60;

impl Default for ServiceSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            cmd: String::new(),
            cwd: None,
            ready: None,
            env: Vec::new(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            restart: true,
            source: "manifest".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    #[serde(default)]
    pub module: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn default_workspace() -> PathBuf {
    "/workspace".into()
}

fn default_listen() -> String {
    "0.0.0.0:7070".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_is_absent_from_a_fresh_boot_and_defaults_on_a_resume() {
        let config: SessionConfig = serde_json::from_str(r#"{"agent": {"command": ["true"]}, "initial_prompt": "hi"}"#).unwrap();
        assert!(config.restore.is_none());
        assert_eq!(config.workspace, default_workspace());
        let config: SessionConfig = serde_json::from_str(
            r#"{"agent": {"command": ["true"]}, "restore": {"suspended": true, "services": [
                {"name": "web", "cmd": "npm run dev"}
            ]}}"#,
        )
        .unwrap();
        let restore = config.restore.unwrap();
        assert!(restore.suspended);
        let web = &restore.services[0];
        assert_eq!(web.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert!(web.restart);
        assert_eq!(web.source, "manifest");
        assert!(web.env.is_empty() && web.ready.is_none() && web.cwd.is_none());
    }
}
