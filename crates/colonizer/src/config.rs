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
    /// Provider gateway listener; colonies reach it through `host.microsandbox.internal`.
    pub gateway_bind: String,
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
        let assets = resolve_assets();
        let settings = Settings {
            bind: env_nonempty("COLONIZER_BIND").unwrap_or_else(|| "127.0.0.1:7878".into()),
            data_dir: env_nonempty("COLONIZER_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share/colonizer")),
            config_dir: env_nonempty("COLONIZER_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config/colonizer")),
            runtime_dir,
            msb: env_nonempty("COLONIZER_MSB").unwrap_or_else(|| {
                // Vendored with the app, then a host install, then whatever is on PATH.
                match assets.as_ref().map(|dir| dir.join("vendor/microsandbox/bin/msb")) {
                    Some(path) if path.exists() => path.display().to_string(),
                    _ if local_msb.exists() => local_msb.display().to_string(),
                    _ => "msb".into(),
                }
            }),
            assets,
            claude_bin: env_nonempty("COLONIZER_CLAUDE_BIN"),
            gateway_bind: env_nonempty("COLONIZER_GATEWAY_BIND").unwrap_or_else(|| "127.0.0.1:41750".into()),
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

/// `colonizer.toml`: hand-edited settings with no place in the UI. Colonizer never writes this file, so
/// a missing file, a missing key or a key we don't know are all the same thing — the default.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct FileConfig {
    pub publish: PublishConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct PublishConfig {
    /// Signs the commit a colony's work is published as with `Co-Authored-By: Colonizer`.
    pub co_author: bool,
}

impl Default for PublishConfig {
    fn default() -> Self {
        Self { co_author: true }
    }
}

impl FileConfig {
    /// Read where it is used rather than cached at startup, so editing the file doesn't need a restart.
    pub fn load(config_dir: &Path) -> Self {
        let path = config_dir.join("colonizer.toml");
        let Ok(text) = std::fs::read_to_string(&path) else { return Self::default() };
        toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!("{}: {e}; using defaults", path.display());
            Self::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> FileConfig {
        toml::from_str(text).expect("valid toml")
    }

    #[test]
    fn colonizer_toml_defaults_to_signing_and_can_turn_it_off() {
        assert!(FileConfig::default().publish.co_author);
        assert!(parse("").publish.co_author);
        assert!(parse("[publish]\n").publish.co_author);
        assert!(!parse("[publish]\nco_author = false\n").publish.co_author);
        // A key we don't know is not a reason to refuse the file.
        assert!(parse("[publish]\nco_author = true\nsomething_else = 3\n").publish.co_author);
    }

    #[test]
    fn load_reads_colonizer_toml_from_the_config_dir() {
        let dir = std::env::temp_dir().join(format!("colonizer-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(FileConfig::load(&dir).publish.co_author, "no file means defaults");

        std::fs::write(dir.join("colonizer.toml"), "[publish]\nco_author = false\n").unwrap();
        assert!(!FileConfig::load(&dir).publish.co_author, "the file is read from config_dir/colonizer.toml");

        std::fs::write(dir.join("colonizer.toml"), "[publish\nco_author = ").unwrap();
        assert!(FileConfig::load(&dir).publish.co_author, "a broken file falls back rather than failing a publish");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
