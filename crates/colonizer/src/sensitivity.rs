//! File-sensitivity classification (issue #472): which providers a subtask touching a path may
//! reach depends on how sensitive that path is, not on how cheap the provider is. Classification is
//! pure and defaults-driven, mirroring routing.rs's tier decision — a repo can extend the built-in
//! defaults with `.colonizer/sensitivity.toml`, but the defaults hold when it does not.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// How sensitive the paths a task touches are, loosest first — so that classifying several paths at
/// once can just take the strictest with `Iterator::max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sensitivity {
    Open,
    Standard,
    Custom,
    Vetted,
    Restricted,
}

impl Sensitivity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Sensitivity::Open => "open",
            Sensitivity::Standard => "standard",
            Sensitivity::Custom => "custom",
            Sensitivity::Vetted => "vetted",
            Sensitivity::Restricted => "restricted",
        }
    }

    /// Read a sensitivity off a session record: case-insensitive, tolerant of stray whitespace, and
    /// `None` for anything that is not a class — mirroring `Tier::parse`, since both travel the same
    /// way, as a bare word on a stored record.
    pub fn parse(s: &str) -> Option<Sensitivity> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Sensitivity::Open),
            "standard" => Some(Sensitivity::Standard),
            "custom" => Some(Sensitivity::Custom),
            "vetted" => Some(Sensitivity::Vetted),
            "restricted" => Some(Sensitivity::Restricted),
            _ => None,
        }
    }
}

/// How far an operator has vouched for a provider, loosest first: any configured connection, one
/// marked `vetted`, or one marked `trusted` — trusted implies vetted, the way the stronger promise
/// carries the weaker one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProviderMark {
    Any,
    Vetted,
    Trusted,
}

impl ProviderMark {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderMark::Any => "any",
            ProviderMark::Vetted => "vetted",
            ProviderMark::Trusted => "trusted",
        }
    }

    /// Read a mark off a setting, where it travels as a bare word: case-insensitive, tolerant of
    /// stray whitespace, and `None` for anything that is not a mark.
    pub fn parse(s: &str) -> Option<ProviderMark> {
        match s.trim().to_ascii_lowercase().as_str() {
            "any" => Some(ProviderMark::Any),
            "vetted" => Some(ProviderMark::Vetted),
            "trusted" => Some(ProviderMark::Trusted),
            _ => None,
        }
    }

    /// The mark of a provider record: `trusted` is the stronger promise and implies `vetted`.
    pub fn of(trusted: bool, vetted: bool) -> ProviderMark {
        if trusted {
            ProviderMark::Trusted
        } else if vetted {
            ProviderMark::Vetted
        } else {
            ProviderMark::Any
        }
    }
}

/// Repo-local overrides read from `.colonizer/sensitivity.toml`, layered on the built-in defaults.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SensitivityConfig {
    pub restricted: Vec<String>,
    pub open: Vec<String>,
    pub custom: Vec<String>,
    pub vetted: Vec<String>,
}

impl SensitivityConfig {
    /// Read where it is used rather than cached, so editing the file doesn't need a restart. A repo
    /// that names no file, or whose file will not parse, gets the built-in defaults only — a
    /// misconfigured file has never been a reason to widen what a colony may touch.
    pub fn load(repo_dir: &Path) -> Self {
        let path = repo_dir.join(".colonizer/sensitivity.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!("{}: could not be parsed ({e}); using the defaults", path.display());
            Self::default()
        })
    }
}

/// Paths that carry secrets or open the door to them: env files, private keys, cloud credentials,
/// infrastructure that provisions both, and the colony's own secrets file. A task touching these
/// may only run on a provider the operator has marked trusted.
const DEFAULT_RESTRICTED: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "credentials.json",
    "infra/",
    ".colonizer/secrets.toml",
];

/// Paths that are public by construction: published docs and vendored third-party code. Touching
/// them says nothing about what else a task may see, so they never raise a task's class.
const DEFAULT_OPEN: &[&str] = &["docs/", "*.md", "vendor/", "third_party/", "node_modules/"];

/// Whether one path hits one pattern. Patterns are one of: a directory (`infra/`), naming the
/// directory itself and anything under it at any depth; a filename suffix (`*.pem`) or prefix
/// (`.env.*`); or an exact name, hit by the filename alone, the whole path, or the path's tail
/// (`.colonizer/secrets.toml` matches a worktree-prefixed copy too). One wildcard position only —
/// these lists are read by people, and a glob engine is not worth its weight for that.
fn matches(path: &str, pattern: &str) -> bool {
    if let Some(dir) = pattern.strip_suffix('/') {
        return path == dir || path.starts_with(&format!("{dir}/")) || path.contains(&format!("/{dir}/"));
    }
    let filename = path.rsplit('/').next().unwrap_or(path);
    if let Some(tail) = pattern.strip_prefix('*') {
        return filename.ends_with(tail);
    }
    if let Some(head) = pattern.strip_suffix('*') {
        return filename.starts_with(head);
    }
    filename == pattern || path == pattern || path.ends_with(&format!("/{pattern}"))
}

/// Whether one path hits any of a repo's configured patterns for a class.
fn matches_any(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| matches(path, pattern))
}

/// Whether a path lands in a class: the repo's configured list first, then the built-in defaults —
/// a config extends the defaults rather than replacing them, so naming one extra restricted file
/// does not mean re-listing every `.env`.
fn in_class(path: &str, configured: &[String], defaults: &[&str]) -> bool {
    matches_any(path, configured) || defaults.iter().any(|pattern| matches(path, pattern))
}

/// The class of one path: restricted is checked first so an override in another list can never
/// loosen a secret, then the vetted and custom tiers (config only — nothing is in either until a
/// repo says so), then open, and everything unlisted is standard.
pub fn classify_path(path: &str, config: &SensitivityConfig) -> Sensitivity {
    if in_class(path, &config.restricted, DEFAULT_RESTRICTED) {
        return Sensitivity::Restricted;
    }
    if matches_any(path, &config.vetted) {
        return Sensitivity::Vetted;
    }
    if matches_any(path, &config.custom) {
        return Sensitivity::Custom;
    }
    if in_class(path, &config.open, DEFAULT_OPEN) {
        return Sensitivity::Open;
    }
    Sensitivity::Standard
}

/// The strictest class among every path a task names. A task naming no path classifies
/// `Standard` — the same access today's routing already gives it, neither widened nor narrowed by
/// a feature it never triggered.
pub fn classify_paths<'a>(paths: impl IntoIterator<Item = &'a str>, config: &SensitivityConfig) -> Sensitivity {
    paths
        .into_iter()
        .map(|path| classify_path(path, config))
        .max()
        .unwrap_or(Sensitivity::Standard)
}

/// An org's per-class minimum provider mark (issue #626), layered over the built-in defaults: each
/// class field names one of `any`, `vetted` or `trusted`, and `None` inherits that class's default.
/// `restricted_vendors` pins restricted work to providers whose recorded vendor is on the list;
/// `None` leaves the mark alone to decide.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SensitivityOverrides {
    pub open: Option<String>,
    pub standard: Option<String>,
    pub custom: Option<String>,
    pub vetted: Option<String>,
    pub restricted: Option<String>,
    pub restricted_vendors: Option<Vec<String>>,
}

impl SensitivityOverrides {
    /// The mark this org names for one class, `None` when it leaves the class at its default.
    fn minimum(&self, sensitivity: Sensitivity) -> Option<&str> {
        match sensitivity {
            Sensitivity::Open => self.open.as_deref(),
            Sensitivity::Standard => self.standard.as_deref(),
            Sensitivity::Custom => self.custom.as_deref(),
            Sensitivity::Vetted => self.vetted.as_deref(),
            Sensitivity::Restricted => self.restricted.as_deref(),
        }
    }
}

/// What an org may save in its sensitivity overrides (orgs.rs `validate` calls this): every mark it
/// names must be one of the three words, `restricted` can be loosened to vetted but never to `any`,
/// and a vendor list, when set, must actually name vendors — a blank entry could never match a
/// vendor, so saving one would write a rule that silently refuses everything. A blank mark inherits,
/// like a blank stack or egress mode.
pub fn validate_overrides(overrides: &SensitivityOverrides) -> Result<(), String> {
    for (class, mark) in [
        ("open", &overrides.open),
        ("standard", &overrides.standard),
        ("custom", &overrides.custom),
        ("vetted", &overrides.vetted),
        ("restricted", &overrides.restricted),
    ] {
        let Some(word) = mark.as_deref().filter(|word| !word.trim().is_empty()) else {
            continue;
        };
        let Some(parsed) = ProviderMark::parse(word) else {
            return Err(format!("sensitivity {class} must be one of any, vetted, trusted"));
        };
        if class == "restricted" && parsed == ProviderMark::Any {
            return Err("sensitivity restricted can be loosened to vetted, but never to any".into());
        }
    }
    if let Some(vendors) = &overrides.restricted_vendors
        && (vendors.is_empty() || vendors.iter().any(|vendor| vendor.trim().is_empty()))
    {
        return Err("sensitivity restricted_vendors must name at least one vendor, or be cleared to inherit".into());
    }
    Ok(())
}

/// The minimum mark one class demands: any provider for the classes that say nothing about trust, a
/// vetted provider for the vetted tier, a trusted one for restricted. The org's override moves the
/// bar either way — except for restricted, which an override may loosen to vetted but never below,
/// however the org spells it.
pub fn required_mark(sensitivity: Sensitivity, overrides: Option<&SensitivityOverrides>) -> ProviderMark {
    let default = match sensitivity {
        Sensitivity::Open | Sensitivity::Standard | Sensitivity::Custom => ProviderMark::Any,
        Sensitivity::Vetted => ProviderMark::Vetted,
        Sensitivity::Restricted => ProviderMark::Trusted,
    };
    let Some(word) = overrides.and_then(|overrides| overrides.minimum(sensitivity)) else {
        return default;
    };
    let Some(mark) = ProviderMark::parse(word) else {
        return default; // validation refuses an unparsable mark; if one is saved anyway, the default holds
    };
    if sensitivity == Sensitivity::Restricted {
        mark.max(ProviderMark::Vetted)
    } else {
        mark
    }
}

/// Whether a provider may carry a task of this sensitivity: its mark must meet the class's minimum
/// (the built-in defaults, moved by the org's overrides — see [`required_mark`]), and restricted
/// work further requires the provider's recorded vendor to be on the org's list when it pins one.
/// Vendors are matched case-insensitively after trimming, and a provider with no vendor recorded
/// fails the pin — "runs somewhere, unrecorded" is not on any list.
pub fn eligible(
    sensitivity: Sensitivity,
    mark: ProviderMark,
    vendor: Option<&str>,
    overrides: Option<&SensitivityOverrides>,
) -> bool {
    if mark < required_mark(sensitivity, overrides) {
        return false;
    }
    if sensitivity == Sensitivity::Restricted
        && let Some(vendors) = overrides.and_then(|overrides| overrides.restricted_vendors.as_ref())
    {
        let Some(vendor) = vendor else {
            return false;
        };
        if !vendors.iter().any(|named| named.trim().eq_ignore_ascii_case(vendor.trim())) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-sensitivity-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn secrets_env_and_infra_paths_classify_restricted_by_default_and_public_ones_open() {
        let config = SensitivityConfig::default();
        assert_eq!(classify_path(".env", &config), Sensitivity::Restricted);
        assert_eq!(classify_path("infra/deploy.yaml", &config), Sensitivity::Restricted);
        assert_eq!(classify_path("foo/.env.production", &config), Sensitivity::Restricted);
        assert_eq!(classify_path("server.pem", &config), Sensitivity::Restricted);
        assert_eq!(classify_path(".colonizer/secrets.toml", &config), Sensitivity::Restricted);

        assert_eq!(classify_path("docs/readme.md", &config), Sensitivity::Open);
        assert_eq!(classify_path("vendor/lib/x.rs", &config), Sensitivity::Open);
    }

    #[test]
    fn ordinary_app_code_classifies_standard_and_a_repo_config_can_restrict_more() {
        let config = SensitivityConfig::default();
        assert_eq!(classify_path("src/main.rs", &config), Sensitivity::Standard);

        let mut stricter = SensitivityConfig::default();
        stricter.restricted.push("research/plans.md".into());
        assert_eq!(classify_path("research/plans.md", &stricter), Sensitivity::Restricted);
        // The default restricted list still holds beside the repo's own entries.
        assert_eq!(classify_path(".env", &stricter), Sensitivity::Restricted);
    }

    #[test]
    fn the_strictest_class_wins_across_a_tasks_paths_and_no_paths_means_standard() {
        let config = SensitivityConfig::default();
        let mixed = classify_paths(["docs/readme.md", "infra/deploy.yaml"], &config);
        assert_eq!(mixed, Sensitivity::Restricted);
        assert_eq!(classify_paths([], &config), Sensitivity::Standard);
        assert_eq!(classify_paths(["src/main.rs"], &config), Sensitivity::Standard);
    }

    #[test]
    fn vetted_ranks_between_custom_and_restricted_and_parses_like_the_rest() {
        assert!(Sensitivity::Custom < Sensitivity::Vetted);
        assert!(Sensitivity::Vetted < Sensitivity::Restricted);
        assert_eq!(Sensitivity::parse(" Vetted "), Some(Sensitivity::Vetted));
        assert_eq!(Sensitivity::Vetted.as_str(), "vetted");
        assert_eq!(Sensitivity::parse("nope"), None);
    }

    #[test]
    fn a_repo_config_can_classify_paths_vetted_and_restricted_still_wins() {
        let mut config = SensitivityConfig::default();
        config.vetted.push("research/".into());
        config.restricted.push("research/secrets.md".into());
        assert_eq!(classify_path("research/plans.md", &config), Sensitivity::Vetted);
        assert_eq!(classify_path("research/secrets.md", &config), Sensitivity::Restricted);
        assert_eq!(classify_path("src/main.rs", &config), Sensitivity::Standard);
    }

    #[test]
    fn each_tier_demands_its_mark_and_an_org_override_moves_the_bar() {
        use ProviderMark::{Any, Trusted, Vetted};
        use Sensitivity::{Custom, Open, Restricted, Standard};
        type Case = (
            &'static str,
            Sensitivity,
            ProviderMark,
            Option<&'static str>,
            Option<SensitivityOverrides>,
            bool,
        );
        let tighten = SensitivityOverrides {
            standard: Some("vetted".into()),
            ..Default::default()
        };
        let loosen = SensitivityOverrides {
            restricted: Some("vetted".into()),
            ..Default::default()
        };
        let clamp_any = SensitivityOverrides {
            restricted: Some("any".into()),
            ..Default::default()
        };
        let pin = SensitivityOverrides {
            restricted_vendors: Some(vec!["Anthropic".into()]),
            ..Default::default()
        };
        let cases: &[Case] = &[
            // Defaults: the loose classes run on anything, vetted needs a vetted provider, restricted
            // a trusted one — and trusted clears every bar.
            ("defaults", Open, Any, None, None, true),
            ("defaults", Standard, Any, None, None, true),
            ("defaults", Custom, Any, None, None, true),
            ("defaults", Sensitivity::Vetted, Any, None, None, false),
            ("defaults", Sensitivity::Vetted, Vetted, None, None, true),
            ("defaults", Restricted, Vetted, None, None, false),
            ("defaults", Restricted, Trusted, None, None, true),
            // An org can tighten a loose class ...
            ("tightened", Standard, Any, None, Some(tighten.clone()), false),
            ("tightened", Standard, Vetted, None, Some(tighten.clone()), true),
            // ... loosen restricted to vetted but never below it, however the org spells it ...
            ("loosened", Restricted, Vetted, None, Some(loosen.clone()), true),
            ("loosened", Restricted, Any, None, Some(loosen), false),
            ("clamped", Restricted, Any, None, Some(clamp_any), false),
            // ... and pin restricted work to the vendors it names.
            ("vendors", Restricted, Trusted, Some("anthropic"), Some(pin.clone()), true),
            ("vendors", Restricted, Trusted, Some("DeepSeek"), Some(pin.clone()), false),
            ("vendors", Restricted, Trusted, None, Some(pin.clone()), false),
            ("vendors", Standard, Any, Some("deepseek"), Some(pin), true),
        ];
        for (group, sensitivity, mark, vendor, overrides, want) in cases {
            assert_eq!(
                eligible(*sensitivity, *mark, *vendor, overrides.as_ref()),
                *want,
                "{group}: {sensitivity:?} on a {mark:?} provider, vendor {vendor:?}, overrides {overrides:?}"
            );
        }
    }

    #[test]
    fn org_sensitivity_overrides_are_validated() {
        let ok = |overrides: SensitivityOverrides| validate_overrides(&overrides).is_ok();
        assert!(ok(SensitivityOverrides {
            standard: Some("vetted".into()),
            restricted: Some(" Trusted ".into()),
            restricted_vendors: Some(vec!["anthropic".into()]),
            ..Default::default()
        }));
        assert!(ok(SensitivityOverrides {
            // Blank inherits, like a blank stack; so does a mark the org leaves unset.
            standard: Some("  ".into()),
            ..Default::default()
        }));
        assert!(!ok(SensitivityOverrides {
            standard: Some("best".into()),
            ..Default::default()
        }));
        assert!(!ok(SensitivityOverrides {
            restricted: Some("any".into()),
            ..Default::default()
        }));
        assert!(!ok(SensitivityOverrides {
            restricted_vendors: Some(Vec::new()),
            ..Default::default()
        }));
        assert!(!ok(SensitivityOverrides {
            restricted_vendors: Some(vec!["anthropic".into(), "  ".into()]),
            ..Default::default()
        }));
    }

    #[test]
    fn load_returns_defaults_when_the_config_file_is_absent_or_malformed() {
        let missing = temp_dir("missing");
        let config = SensitivityConfig::load(&missing);
        assert_eq!(config.restricted, Vec::<String>::new());
        assert_eq!(config.open, Vec::<String>::new());
        assert_eq!(config.custom, Vec::<String>::new());
        assert_eq!(config.vetted, Vec::<String>::new());
        let _ = std::fs::remove_dir_all(missing);

        let corrupt = temp_dir("corrupt");
        std::fs::create_dir_all(corrupt.join(".colonizer")).unwrap();
        std::fs::write(corrupt.join(".colonizer/sensitivity.toml"), "restricted = not a list").unwrap();
        let config = SensitivityConfig::load(&corrupt);
        assert_eq!(config.restricted, Vec::<String>::new());
        assert_eq!(classify_path(".env", &config), Sensitivity::Restricted, "defaults still hold");
        let _ = std::fs::remove_dir_all(corrupt);
    }

    #[test]
    fn a_config_file_parses_and_classifies_through_the_loaded_lists() {
        let repo = temp_dir("parsed");
        std::fs::create_dir_all(repo.join(".colonizer")).unwrap();
        std::fs::write(
            repo.join(".colonizer/sensitivity.toml"),
            "restricted = [\"research/plans.md\"]\ncustom = [\"src/generated/\"]\n",
        )
        .unwrap();
        let config = SensitivityConfig::load(&repo);
        assert_eq!(classify_path("research/plans.md", &config), Sensitivity::Restricted);
        assert_eq!(classify_path("src/generated/api.rs", &config), Sensitivity::Custom);
        let _ = std::fs::remove_dir_all(repo);
    }
}
