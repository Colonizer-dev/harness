//! `/legion/session.json` (docs/protocol.md §1).

use serde::Deserialize;
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
