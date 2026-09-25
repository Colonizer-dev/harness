//! File-sensitivity classification (issue #472): which providers a subtask touching a path may
//! reach depends on how sensitive that path is, not on how cheap the provider is. Classification is
//! pure and defaults-driven, mirroring routing.rs's tier decision — a repo can extend the built-in
//! defaults with `.colonizer/sensitivity.toml`, but the defaults hold when it does not.

use serde::Deserialize;
use std::path::Path;

/// How sensitive the paths a task touches are, loosest first — so that classifying several paths at
/// once can just take the strictest with `Iterator::max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sensitivity {
    Open,
    Standard,
    Custom,
    Restricted,
}

impl Sensitivity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Sensitivity::Open => "open",
            Sensitivity::Standard => "standard",
            Sensitivity::Custom => "custom",
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
            "restricted" => Some(Sensitivity::Restricted),
            _ => None,
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
/// loosen a secret, then custom (config only — nothing is custom until a repo says so), then open,
/// and everything unlisted is standard.
pub fn classify_path(path: &str, config: &SensitivityConfig) -> Sensitivity {
    if in_class(path, &config.restricted, DEFAULT_RESTRICTED) {
        return Sensitivity::Restricted;
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

/// Whether a provider may carry a task of this sensitivity. Only `Restricted` gates today, and it
/// gates on the operator's mark alone: `providers.json` carries no vendor-identity field to check
/// a first-party frontier against yet, so "trusted" is exactly what an operator has said it is.
/// `Open`, `Standard` and `Custom` place no extra restriction in this first slice; a "vetted" tier
/// and org-level overrides are follow-up work (issue #472).
pub fn eligible(sensitivity: Sensitivity, provider_trusted: bool) -> bool {
    sensitivity != Sensitivity::Restricted || provider_trusted
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
    fn only_a_restricted_task_gates_on_trust_and_every_other_class_is_eligible_either_way() {
        assert_eq!(Sensitivity::parse(" restricted "), Some(Sensitivity::Restricted));
        assert_eq!(Sensitivity::parse("restricted"), Some(Sensitivity::Restricted));
        assert_eq!(Sensitivity::parse("nope"), None);
        assert!(!eligible(Sensitivity::Restricted, false));
        assert!(eligible(Sensitivity::Restricted, true));
        assert!(eligible(Sensitivity::Open, false));
        assert!(eligible(Sensitivity::Standard, false));
        assert!(eligible(Sensitivity::Custom, false));
    }

    #[test]
    fn load_returns_defaults_when_the_config_file_is_absent_or_malformed() {
        let missing = temp_dir("missing");
        let config = SensitivityConfig::load(&missing);
        assert_eq!(config.restricted, Vec::<String>::new());
        assert_eq!(config.open, Vec::<String>::new());
        assert_eq!(config.custom, Vec::<String>::new());
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
