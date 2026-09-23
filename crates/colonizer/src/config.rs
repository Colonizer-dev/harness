//! Process settings (environment) and module selection (persisted JSON in the config dir).

use crate::util::{env_nonempty, is_elf};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    net::SocketAddr,
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
    /// Provider gateway listener; colonies reach it through `host.microsandbox.internal`. An
    /// IP:port socket address parsed once at startup ([`parse_gateway_bind`]), so the listener,
    /// the colony model routes and the network fence all use the same port.
    pub gateway_bind: SocketAddr,
    pub allowed_hosts: Vec<String>,
    /// Other mothership base URLs to poll for `GET /api/hosts` (issue #231's fleet view), reached
    /// over whatever private network the operator already has (their own tailnet/mesh, a VPN, a LAN).
    /// This host's own `COLONIZER_BIND` stays loopback-only by default regardless of this list —
    /// nothing here auto-exposes anything. For a peer to be pollable, its operator sets *that peer's*
    /// `COLONIZER_BIND` to a private interface IP of their own choosing (never `0.0.0.0`).
    pub fleet_peers: Vec<String>,
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
            gateway_bind: Self::parse_gateway_bind(env_nonempty("COLONIZER_GATEWAY_BIND"))?,
            allowed_hosts: env_nonempty("COLONIZER_ALLOWED_HOSTS")
                .unwrap_or_default()
                .split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect(),
            fleet_peers: env_nonempty("COLONIZER_FLEET_PEERS")
                .unwrap_or_default()
                .split(',')
                .map(|u| u.trim().to_string())
                .filter(|u| !u.is_empty())
                .collect(),
        };
        let data = settings.data_dir.display().to_string();
        if data.contains(':') || data.contains(',') {
            bail!("COLONIZER_DATA_DIR must not contain ':' or ',' (it is used in microVM mount specs)");
        }
        Ok(settings)
    }

    /// `COLONIZER_GATEWAY_BIND` as a socket address, parsed once because the listener, the colony
    /// model routes and the network fence must all agree on the port. A malformed value (including
    /// a hostname like `localhost:41750`, which no longer resolves here) refuses startup instead of
    /// silently falling back to 41750 — a fallback that would open whatever unrelated service
    /// holds that port to every colony (issue #406).
    fn parse_gateway_bind(raw: Option<String>) -> Result<SocketAddr> {
        let Some(raw) = raw else {
            return Ok(SocketAddr::from(([127, 0, 0, 1], 41750)));
        };
        match raw.parse() {
            Ok(addr) => Ok(addr),
            Err(_) => bail!(
                "COLONIZER_GATEWAY_BIND must be an IP:port socket address such as 127.0.0.1:41750 (a hostname is not enough), got {raw:?}"
            ),
        }
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

    /// A bundled binary that a colony `exec`s inside a Linux microVM: it must exist and be an ELF.
    /// The build scripts refuse a here build off Linux, so a non-ELF still here is a stale artefact
    /// from before that guard or a binary copied by hand — caught now, not by Linux's ENOEXEC with
    /// the VM already booting.
    pub fn linux_binary(&self, relative: &str) -> Result<PathBuf> {
        let path = self.asset(relative)?;
        if !is_elf(&path) {
            bail!(
                "{} is not an ELF binary, so the colony cannot exec it (a stale artefact from before the build scripts checked, or a hand-copied binary); run scripts/install.sh",
                path.display()
            );
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
        Self {
            provider: provider.into(),
            enabled: true,
            settings: Map::new(),
        }
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
    /// Absent in a modules.json written before autonomous mode existed, which reads as off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<ModuleChoice>,
    /// Absent until it is configured, like `autonomy`: telling the world outside this machine about
    /// your colonies is something to switch on, not a default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<ModuleChoice>,
    /// Absent until it is configured, like `autonomy` and `notify`: spending a weekly plan on
    /// burner colonies is a decision, not a default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burn_down: Option<ModuleChoice>,
    /// Speech-to-text for the composer. Absent reads as the browser's own recognition: sending audio
    /// to a service is something to connect, not a default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<ModuleChoice>,
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
            // Off until it is switched on: a judge answering for you is a decision, not a default.
            autonomy: None,
            // Off until it is configured: a webhook is a write to somewhere outside this machine.
            notify: None,
            // Off until it is configured: burning a plan is a decision, not a default.
            burn_down: None,
            voice: None,
        }
    }
}

impl ModulesConfig {
    /// Loads `modules.json` the way startup loads `sessions.json`: a missing file is the default
    /// (a first run), and a file that cannot be read or parsed is moved aside to
    /// `<name>.corrupt-<unix-timestamp>` — never overwritten — with the default and a sticky
    /// `LoadDamage` alert in its place. The old silent `unwrap_or_default` meant a damaged file's
    /// settings were quietly forgotten and the next save replaced the operator's bytes with
    /// defaults (#408). A file that cannot even be moved aside is a hard error: the next save
    /// would destroy it.
    pub fn load(path: &Path) -> Result<(Self, Option<crate::StorageAlert>)> {
        let data = match std::fs::read(path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Self::default(), None)),
            Err(e) => return Self::damaged(path, format!("could not be read ({e})")),
        };
        match serde_json::from_slice(&data) {
            Ok(config) => Ok((config, None)),
            Err(e) => Self::damaged(path, format!("could not be parsed ({e})")),
        }
    }

    /// The default config plus the alert that says where the damaged file's bytes went.
    fn damaged(path: &Path, reason: String) -> Result<(Self, Option<crate::StorageAlert>)> {
        let saved = crate::move_corrupt_aside(path)?;
        let message = format!(
            "{} {reason} and was saved as {}; module settings are back on their defaults until it is fixed or removed",
            path.display(),
            saved.display()
        );
        eprintln!("modules: {message}");
        Ok((
            Self::default(),
            Some(crate::StorageAlert {
                kind: crate::StorageAlertKind::LoadDamage,
                message,
                ts: chrono::Utc::now(),
                failures: 1,
                recovered_at: None,
            }),
        ))
    }

    pub async fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::util::write_atomic(path, &serde_json::to_vec_pretty(self)?).await
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
            "autonomy" => self.autonomy.as_ref(),
            "notify" => self.notify.as_ref(),
            "burn_down" => self.burn_down.as_ref(),
            "voice" => self.voice.as_ref(),
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
            // Absent until it is configured, so the entry is created on first save rather than
            // written into every modules.json that never asked for it.
            "autonomy" => Some(self.autonomy.get_or_insert_with(|| ModuleChoice::new("off"))),
            "notify" => Some(self.notify.get_or_insert_with(|| ModuleChoice::new("default"))),
            "burn_down" => Some(self.burn_down.get_or_insert_with(|| ModuleChoice::new("default"))),
            "voice" => Some(self.voice.get_or_insert_with(|| ModuleChoice::new(crate::voice::BROWSER))),
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

/// A [`ModuleChoice`] with a preset's defaults filled in underneath what the
/// user actually set.
///
/// Precedence is explicit setting, then preset, then schema default. An
/// existing `modules.json` therefore keeps every value it names, whichever
/// preset is selected — the preset only reaches keys nobody chose.
pub fn with_preset(choice: &ModuleChoice, preset_defaults: &Value) -> ModuleChoice {
    let mut merged = choice.clone();
    if let Some(defaults) = preset_defaults.as_object() {
        for (key, value) in defaults {
            merged.settings.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    merged
}

pub fn setting_str(choice: &ModuleChoice, schema: &Value, key: &str) -> String {
    setting(choice, schema, key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn setting_u64(choice: &ModuleChoice, schema: &Value, key: &str) -> u64 {
    setting(choice, schema, key).and_then(Value::as_u64).unwrap_or_default()
}

pub fn setting_f64(choice: &ModuleChoice, schema: &Value, key: &str) -> f64 {
    setting(choice, schema, key).and_then(Value::as_f64).unwrap_or_default()
}

/// Who a colony's commits and pull requests name as co-author.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CoAuthor {
    pub name: String,
    pub email: String,
}

impl CoAuthor {
    /// github.com/colonizer-settlers. GitHub only attributes a trailer whose address belongs to the
    /// account, so this must be the ID-prefixed noreply form; a bare
    /// `colonizer-settlers@users.noreply.github.com` shows as plain text.
    pub fn settlers() -> Self {
        Self {
            name: "Colonizer Settlers".into(),
            email: "331648616+colonizer-settlers@users.noreply.github.com".into(),
        }
    }

    /// The `Co-Authored-By` trailer line naming this identity.
    pub fn trailer(&self) -> String {
        format!("Co-Authored-By: {} <{}>", self.name, self.email)
    }

    /// The GitHub login behind a users.noreply.github.com address (`ID+login@…` or legacy
    /// `login@…`), for crediting the account by name where there is no co-author field; `None`
    /// for any other address.
    pub fn github_login(&self) -> Option<&str> {
        let (local, domain) = self.email.split_once('@')?;
        if !domain.eq_ignore_ascii_case("users.noreply.github.com") {
            return None;
        }
        let login = match local.split_once('+') {
            Some((id, login)) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => login,
            None => local,
            _ => return None,
        };
        if login.is_empty() || !login.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return None;
        }
        Some(login)
    }
}

/// `co_author` takes a bool or an identity: `true` is Colonizer Settlers, `false` turns the
/// trailer off, and a table names someone else.
#[derive(Deserialize)]
#[serde(untagged)]
enum CoAuthorOpt {
    On(bool),
    Identity(CoAuthor),
}

fn default_co_author() -> Option<CoAuthor> {
    Some(CoAuthor::settlers())
}

fn de_co_author<'de, D>(deserializer: D) -> Result<Option<CoAuthor>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match CoAuthorOpt::deserialize(deserializer)? {
        CoAuthorOpt::On(true) => Ok(Some(CoAuthor::settlers())),
        CoAuthorOpt::On(false) => Ok(None),
        CoAuthorOpt::Identity(identity) => Ok(Some(identity)),
    }
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
    /// Who the commit and the pull request body name as co-author (`Co-Authored-By` trailer):
    /// `true` (the default) is Colonizer Settlers, a table names someone else, `false` turns the
    /// trailer — and the findings credit — off.
    #[serde(default = "default_co_author", deserialize_with = "de_co_author")]
    pub co_author: Option<CoAuthor>,
}

impl Default for PublishConfig {
    fn default() -> Self {
        Self {
            co_author: default_co_author(),
        }
    }
}

impl FileConfig {
    /// Read where it is used rather than cached at startup, so editing the file doesn't need a restart.
    pub fn load(config_dir: &Path) -> Self {
        let path = config_dir.join("colonizer.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
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
    fn gateway_bind_defaults_unset_parses_an_ip_port_and_refuses_everything_else() {
        // Unset keeps the documented default; an explicit bind may name any IP, IPv6 included.
        assert_eq!(
            Settings::parse_gateway_bind(None).unwrap(),
            "127.0.0.1:41750".parse().unwrap()
        );
        assert_eq!(
            Settings::parse_gateway_bind(Some("192.168.1.9:52000".into())).unwrap(),
            "192.168.1.9:52000".parse().unwrap()
        );
        assert_eq!(
            Settings::parse_gateway_bind(Some("[::1]:41750".into())).unwrap(),
            "[::1]:41750".parse().unwrap()
        );
        // Anything else must refuse startup, naming the variable and the bad value: the old
        // fallback would have opened port 41750 to colonies whatever the operator meant (#406).
        for raw in [
            "41750",
            "127.0.0.1",
            "localhost:41750",
            "127.0.0.1:notaport",
            "127.0.0.1:70000",
        ] {
            let err = Settings::parse_gateway_bind(Some(raw.into())).unwrap_err().to_string();
            assert!(err.contains("COLONIZER_GATEWAY_BIND"), "{raw:?}: {err}");
            assert!(err.contains(raw), "the bad value {raw:?} should be named: {err}");
        }
    }

    #[test]
    fn colonizer_toml_defaults_to_settlers_and_can_turn_it_off_or_name_someone_else() {
        let settlers = Some(CoAuthor::settlers());
        assert_eq!(FileConfig::default().publish.co_author, settlers);
        assert_eq!(parse("").publish.co_author, settlers);
        assert_eq!(parse("[publish]\n").publish.co_author, settlers);
        assert_eq!(parse("[publish]\nco_author = true\n").publish.co_author, settlers);
        assert_eq!(parse("[publish]\nco_author = false\n").publish.co_author, None);
        // A key we don't know is not a reason to refuse the file.
        assert_eq!(
            parse("[publish]\nco_author = true\nsomething_else = 3\n").publish.co_author,
            settlers
        );
        let custom = CoAuthor {
            name: "Someone Else".into(),
            email: "someone@example.com".into(),
        };
        assert_eq!(
            parse("[publish]\nco_author = { name = \"Someone Else\", email = \"someone@example.com\" }\n")
                .publish
                .co_author,
            Some(custom.clone())
        );
        assert_eq!(
            parse("[publish.co_author]\nname = \"Someone Else\"\nemail = \"someone@example.com\"\n")
                .publish
                .co_author,
            Some(custom)
        );
    }

    #[test]
    fn the_default_co_author_is_the_id_prefixed_noreply_address() {
        let email = CoAuthor::settlers().email;
        let (id, rest) = email.split_once('+').expect("the ID-prefixed noreply form");
        assert_eq!(id.parse::<u64>().expect("a numeric GitHub user id"), 331648616);
        assert_eq!(rest, "colonizer-settlers@users.noreply.github.com");
    }

    #[test]
    fn github_login_reads_the_account_behind_a_noreply_address() {
        fn login(email: &str) -> Option<String> {
            CoAuthor {
                name: "x".into(),
                email: email.into(),
            }
            .github_login()
            .map(str::to_string)
        }
        assert_eq!(
            login("331648616+colonizer-settlers@users.noreply.github.com").as_deref(),
            Some("colonizer-settlers")
        );
        assert_eq!(
            login("colonizer-settlers@users.noreply.github.com").as_deref(),
            Some("colonizer-settlers"),
            "the legacy form without an id prefix still names the account"
        );
        assert_eq!(login("someone@example.com"), None);
        assert_eq!(login("not-an-email"), None);
        assert_eq!(login("abc+colonizer-settlers@users.noreply.github.com"), None);
        assert_eq!(login("+colonizer-settlers@users.noreply.github.com"), None);
        assert_eq!(login("331648616+@users.noreply.github.com"), None);
        assert_eq!(
            login("331648616+colonizer_settlers@users.noreply.github.com"),
            None,
            "a login GitHub would not accept is not credited"
        );
    }

    #[test]
    fn load_reads_colonizer_toml_from_the_config_dir() {
        let dir = std::env::temp_dir().join(format!("colonizer-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            FileConfig::load(&dir).publish.co_author,
            Some(CoAuthor::settlers()),
            "no file means defaults"
        );

        std::fs::write(dir.join("colonizer.toml"), "[publish]\nco_author = false\n").unwrap();
        assert_eq!(
            FileConfig::load(&dir).publish.co_author,
            None,
            "the file is read from config_dir/colonizer.toml"
        );

        std::fs::write(dir.join("colonizer.toml"), "[publish\nco_author = ").unwrap();
        assert_eq!(
            FileConfig::load(&dir).publish.co_author,
            Some(CoAuthor::settlers()),
            "a broken file falls back rather than failing a publish"
        );

        std::fs::write(dir.join("colonizer.toml"), "[publish]\nco_author = { name = \"x\" }\n").unwrap();
        assert_eq!(
            FileConfig::load(&dir).publish.co_author,
            Some(CoAuthor::settlers()),
            "a malformed co_author falls back too, so signing stays on"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    use serde_json::json;

    fn choice(settings: Value) -> ModuleChoice {
        ModuleChoice {
            provider: "microsandbox".into(),
            enabled: true,
            settings: settings.as_object().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn an_explicit_setting_beats_the_preset() {
        // Somebody who already pinned an image keeps it, whichever stack is
        // selected. This is the whole backwards-compatibility promise.
        let c = choice(json!({"image": "ghcr.io/me/my-toolchain:1"}));
        let merged = with_preset(&c, &crate::presets::defaults("rust"));
        assert_eq!(merged.settings["image"], "ghcr.io/me/my-toolchain:1");
        // Keys they did not set still come from the preset.
        assert_eq!(merged.settings["cpus"], 6);
    }

    #[test]
    fn the_preset_fills_in_what_was_never_set() {
        let merged = with_preset(&choice(json!({})), &crate::presets::defaults("python"));
        let image = merged.settings["image"].as_str().unwrap();
        assert!(
            image.starts_with("python:3.13-bookworm@sha256:"),
            "{image} is not the pinned python tag"
        );
        assert_eq!(merged.settings["memory"], "8G");
    }

    #[test]
    fn custom_leaves_the_choice_untouched() {
        let c = choice(json!({"image": "debian:bookworm"}));
        let merged = with_preset(&c, &crate::presets::defaults(crate::presets::CUSTOM));
        assert_eq!(merged.settings, c.settings, "custom must not inject anything");
    }

    #[test]
    fn setting_falls_back_from_choice_to_schema() {
        let schema = json!({"properties": {"memory": {"default": "8G"}}});
        assert_eq!(setting_str(&choice(json!({"memory": "16G"})), &schema, "memory"), "16G");
        assert_eq!(setting_str(&choice(json!({})), &schema, "memory"), "8G");
    }

    #[test]
    fn notify_is_absent_until_configured_like_autonomy() {
        let mut modules = ModulesConfig::default();
        assert!(modules.get("notify").is_none(), "off until it is configured");
        assert!(
            !serde_json::to_string(&modules).unwrap().contains("notify"),
            "not written into the modules.json of anyone who never asked for it"
        );
        modules.get_mut("notify").unwrap();
        assert!(modules.get("notify").is_some());
        assert!(
            serde_json::to_string(&modules).unwrap().contains("notify"),
            "created on first save"
        );
    }

    #[test]
    fn linux_binary_refuses_an_asset_the_colony_cannot_exec() {
        let dir = std::env::temp_dir().join(format!("colonizer-config-elf-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let cfg = Settings {
            bind: String::new(),
            data_dir: dir.clone(),
            config_dir: dir.clone(),
            runtime_dir: dir.clone(),
            assets: Some(dir.clone()),
            msb: String::new(),
            claude_bin: None,
            gateway_bind: "127.0.0.1:0".parse().unwrap(),
            allowed_hosts: Vec::new(),
            fleet_peers: Vec::new(),
        };

        let missing = cfg.linux_binary("bin/colonizer-agentd").unwrap_err().to_string();
        assert!(missing.contains("scripts/install.sh"), "{missing}");

        // What a Mac build with COLONIZER_BUILD_HERE=1 installs.
        std::fs::write(dir.join("bin/colonizer-agentd"), b"\xcf\xfa\xed\xfe").unwrap();
        let macho = cfg.linux_binary("bin/colonizer-agentd").unwrap_err().to_string();
        assert!(macho.contains("is not an ELF binary"), "{macho}");
        assert!(macho.contains("scripts/install.sh"), "{macho}");

        std::fs::write(dir.join("bin/colonizer-agentd"), b"\x7fELF padding").unwrap();
        assert_eq!(
            cfg.linux_binary("bin/colonizer-agentd").unwrap(),
            dir.join("bin/colonizer-agentd")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_damaged_modules_json_is_moved_aside_with_an_alert_rather_than_overwritten() {
        let dir = std::env::temp_dir().join(format!("colonizer-modules-damage-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("modules.json");

        let (modules, alert) = ModulesConfig::load(&path).unwrap();
        assert_eq!(
            serde_json::to_string(&modules).unwrap(),
            serde_json::to_string(&ModulesConfig::default()).unwrap(),
            "a missing file is a first run"
        );
        assert!(alert.is_none());

        std::fs::write(&path, "{\"source\":").unwrap();
        let (modules, alert) = ModulesConfig::load(&path).unwrap();
        assert_eq!(
            serde_json::to_string(&modules).unwrap(),
            serde_json::to_string(&ModulesConfig::default()).unwrap(),
            "a damaged file reads as the defaults"
        );
        let alert = alert.expect("damage is reported");
        assert_eq!(alert.kind, crate::StorageAlertKind::LoadDamage);
        assert!(alert.message.contains("modules.json"), "{}", alert.message);
        assert!(
            alert.message.contains(".corrupt-"),
            "the message says where the bytes went: {}",
            alert.message
        );
        let aside: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(aside.len(), 1, "only the moved-aside file remains: {aside:?}");
        assert!(aside[0].starts_with("modules.json.corrupt-"), "{aside:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join(&aside[0])).unwrap(),
            "{\"source\":",
            "the original bytes survive the move"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
