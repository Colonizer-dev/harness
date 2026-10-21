//! Which models the owner has enabled for each agent module. The cockpit's agent/model menu lists
//! only these; Settings → Models edits them with checkboxes. Stored apart from `orgs.json` and
//! `modules.json` in `<config>/enabled-models.json`: an install-wide list per module, and an org's
//! own list per module, which replaces the install's for that org.
//!
//! When a module has no saved list, the effective list is the models its roles are assigned to
//! (see `model_switch`), not every model on offer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// The longest model id the store accepts; a provider's `<provider>/<model>` is far shorter.
const MAX_ID: usize = 200;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Store {
    /// Install-wide lists, per agent module id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub install: BTreeMap<String, Vec<String>>,
    /// An org's own lists, per org and per agent module id; each replaces the install's for that org.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub orgs: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

pub fn file(config_dir: &Path) -> PathBuf {
    config_dir.join("enabled-models.json")
}

/// The stored lists. A file that cannot be read is an error, never a silent reset: a save that
/// followed a failed read would wipe every saved list.
pub fn load(config_dir: &Path) -> Result<Store> {
    crate::util::read_json_or_default(&file(config_dir))
}

pub fn save(config_dir: &Path, store: &Store) -> Result<()> {
    let path = file(config_dir);
    let data = serde_json::to_vec_pretty(store).context("serialize enabled models")?;
    let tmp = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, data).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// The list in force for `module` at `org` (`None` for the install): the org's own list when it
/// has one for the module, else the install's, else `None` — unset, so the caller falls back to
/// the models the roles are assigned to.
pub fn saved_list<'a>(store: &'a Store, org: Option<&str>, module: &str) -> Option<&'a Vec<String>> {
    if let Some(org) = org
        && let Some(list) = store.orgs.get(org).and_then(|per_module| per_module.get(module))
    {
        return Some(list);
    }
    store.install.get(module)
}

/// Clean a requested list: each id trimmed, non-empty, at most [`MAX_ID`] characters, with no
/// whitespace inside, duplicates dropped in order. A bad id is refused by naming it.
pub fn validate(models: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in models {
        let id = raw.trim();
        if id.is_empty() {
            bail!("a model id is empty");
        }
        if id.len() > MAX_ID || id.chars().any(char::is_whitespace) {
            bail!("{id:?} is not a model id (no spaces, at most {MAX_ID} characters)");
        }
        if !out.iter().any(|m| m == id) {
            out.push(id.to_string());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let mut s = Store::default();
        s.install.insert("pi".into(), vec!["claude-opus-5-5".into()]);
        s.orgs
            .entry("acme".into())
            .or_default()
            .insert("pi".into(), vec!["deepseek/deepseek-chat".into()]);
        s
    }

    #[test]
    fn an_org_list_replaces_the_install_list_for_that_org_only() {
        let s = store();
        assert_eq!(
            saved_list(&s, Some("acme"), "pi").unwrap(),
            &vec!["deepseek/deepseek-chat".to_string()]
        );
        // Another org reads the install's list, and an unlisted module reads nothing saved.
        assert_eq!(
            saved_list(&s, Some("other"), "pi").unwrap(),
            &vec!["claude-opus-5-5".to_string()]
        );
        assert!(saved_list(&s, None, "opencode").is_none());
    }

    #[test]
    fn ids_are_trimmed_deduplicated_and_checked() {
        assert_eq!(
            validate(&[" claude-opus-5-5 ".into(), "claude-opus-5-5".into(), "x/y".into()]).unwrap(),
            vec!["claude-opus-5-5".to_string(), "x/y".to_string()]
        );
        assert!(validate(&["".into()]).unwrap_err().to_string().contains("empty"));
        assert!(validate(&["a b".into()]).unwrap_err().to_string().contains("not a model id"));
        assert!(validate(&["x".repeat(MAX_ID + 1)]).is_err());
    }

    #[test]
    fn the_store_round_trips_through_its_file() {
        let dir = std::env::temp_dir().join(format!("colonizer-enabled-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load(&dir).unwrap(), Store::default(), "a missing file is no lists saved");
        save(&dir, &store()).unwrap();
        assert_eq!(load(&dir).unwrap(), store());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
