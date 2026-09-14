//! Process settings (environment) and module selection (persisted JSON in the config dir).

use crate::util::env_nonempty;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Settings {
    pub bind: String,
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    /// Short-path directory for unix sockets (sun_path is limited to 108 bytes).
    pub runtime_dir: PathBuf,
    /// Bundled app assets (`bin/`, `vendor/`, `modules/`, `web/`), if found.
    pub assets: Option<PathBuf>,
    pub msb: String,
    pub claude_bin: Option<String>,
    pub allowed_hosts: Vec<String>,
}

impl Settings {
    pub fn from_env() -> Result<Self> {
        let home = PathBuf::from(std::env::var("HOME").context("HOME is not set")?);
        let local_msb = home.join(".local/bin/msb");
        let uid = std::fs::metadata(&home).map(|m| m.uid()).unwrap_or(0);
        let runtime_dir = env_nonempty("XDG_RUNTIME_DIR")
            .map(|d| PathBuf::from(d).join("colonizer"))
            .unwrap_or_else(|| PathBuf::from(format!("/tmp/colonizer-{uid}")));
        let settings = Settings {
            bind: env_nonempty("COLONIZER_BIND").unwrap_or_else(|| "127.0.0.1:7878".into()),
            data_dir: env_nonempty("COLONIZER_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share/colonizer")),
            config_dir: env_nonempty("COLONIZER_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config/colonizer")),
            runtime_dir,
            assets: resolve_assets(),
            msb: env_nonempty("COLONIZER_MSB").unwrap_or_else(|| {
                if local_msb.exists() { local_msb.display().to_string() } else { "msb".into() }
            }),
            claude_bin: env_nonempty("COLONIZER_CLAUDE_BIN"),
            allowed_hosts: env_nonempty("COLONIZER_ALLOWED_HOSTS")
                .unwrap_or_default()
                .split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect(),
        };
        let data = settings.data_dir.display().to_string();
        if data.contains(':') || data.contains(',') {
            bail!("COLONIZER_DATA_DIR must not contain ':' or ',' (it is used in microVM mount specs)");
        }
        Ok(settings)
    }

    pub fn asset(&self, relative: &str) -> Result<PathBuf> {
        let root = self
            .assets
            .as_ref()
            .context("app assets not found: run scripts/install.sh (or set COLONIZER_HOME)")?;
        let path = root.join(relative);
        if !path.exists() {
            bail!("missing bundled asset {} (run scripts/install.sh)", path.display());
        }
        Ok(path)
    }
}

/// `COLONIZER_HOME`, an installed layout next to the binary, or `dist/` in a source checkout.
fn resolve_assets() -> Option<PathBuf> {
    if let Some(home) = env_nonempty("COLONIZER_HOME") {
        return Some(PathBuf::from(home));
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    for dir in exe.ancestors().skip(1) {
        if dir.join("vendor").is_dir() && dir.join("bin").is_dir() {
            return Some(dir.to_path_buf());
        }
        if dir.join("dist").join("vendor").is_dir() {
            return Some(dir.join("dist"));
        }
    }
    None
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModuleChoice {
    pub provider: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub settings: Map<String, Value>,
}

fn enabled_by_default() -> bool {
    true
}

impl ModuleChoice {
    fn new(provider: &str) -> Self {
        Self { provider: provider.into(), enabled: true, settings: Map::new() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModulesConfig {
    pub source: ModuleChoice,
    pub sandbox: ModuleChoice,
    pub mesh: ModuleChoice,
    pub agent: ModuleChoice,
    pub interfaces: ModuleChoice,
    pub publish: ModuleChoice,
    #[serde(default = "default_memory")]
    pub memory: ModuleChoice,
    #[serde(default = "default_watchdog")]
    pub watchdog: ModuleChoice,
}

fn default_memory() -> ModuleChoice {
    ModuleChoice::new("files")
}

fn default_watchdog() -> ModuleChoice {
    ModuleChoice::new("default")
}

impl Default for ModulesConfig {
    fn default() -> Self {
        Self {
            source: ModuleChoice::new("github"),
            sandbox: ModuleChoice::new("microsandbox"),
            mesh: ModuleChoice::new("headscale"),
            agent: ModuleChoice::new("claude-code"),
            interfaces: ModuleChoice::new("default"),
            publish: ModuleChoice::new("github-pr"),
            memory: default_memory(),
            watchdog: default_watchdog(),
        }
    }
}

impl ModulesConfig {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn get(&self, kind: &str) -> Option<&ModuleChoice> {
        match kind {
            "source" => Some(&self.source),
            "sandbox" => Some(&self.sandbox),
            "mesh" => Some(&self.mesh),
            "agent" => Some(&self.agent),
            "interfaces" => Some(&self.interfaces),
            "publish" => Some(&self.publish),
            "memory" => Some(&self.memory),
            "watchdog" => Some(&self.watchdog),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, kind: &str) -> Option<&mut ModuleChoice> {
        match kind {
            "source" => Some(&mut self.source),
            "sandbox" => Some(&mut self.sandbox),
            "mesh" => Some(&mut self.mesh),
            "agent" => Some(&mut self.agent),
            "interfaces" => Some(&mut self.interfaces),
            "publish" => Some(&mut self.publish),
            "memory" => Some(&mut self.memory),
            "watchdog" => Some(&mut self.watchdog),
            _ => None,
        }
    }

    pub fn mesh_enabled(&self) -> bool {
        self.mesh.enabled && self.mesh.provider == "headscale"
    }
}

/// Reads a setting, falling back to the schema default.
pub fn setting<'a>(choice: &'a ModuleChoice, schema: &'a Value, key: &str) -> Option<&'a Value> {
    choice.settings.get(key).or_else(|| schema["properties"][key].get("default"))
}

pub fn setting_str(choice: &ModuleChoice, schema: &Value, key: &str) -> String {
    setting(choice, schema, key).and_then(Value::as_str).unwrap_or_default().to_string()
}

pub fn setting_u64(choice: &ModuleChoice, schema: &Value, key: &str) -> u64 {
    setting(choice, schema, key).and_then(Value::as_u64).unwrap_or_default()
}
