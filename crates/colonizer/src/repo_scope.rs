//! One reading of "which repositories": the entries every repository-list setting shares, and the
//! one place the `*` wildcard is resolved (issues #1212 and #1213).
//!
//! An entry is `*` (every repository of every visible workspace), `owner` (every repository of
//! that org) or `owner/name`. Hiding an org (`OrgSettings::hidden`) leaves it out of `*` at use
//! time, so hiding takes effect for every loop and setting at once; an entry that names the org
//! or the repository outright is the operator's explicit choice and still counts.

use crate::{App, orgs, util::valid_repo};
use std::collections::BTreeSet;

/// The "all repositories" wildcard.
pub const ALL: &str = "*";

/// Whether an entry is the wildcard.
pub fn is_all(entry: &str) -> bool {
    entry.trim() == ALL
}

/// An entry a repository-list setting accepts: `*`, `owner` or `owner/name`.
pub fn valid_entry(entry: &str) -> bool {
    let entry = entry.trim();
    is_all(entry) || valid_repo(entry) || (!entry.contains('/') && valid_repo(&format!("{entry}/x")))
}

/// Whether one entry names `repo`. `*` names everything; hiding is [`covers`]'s business.
pub fn entry_matches(entry: &str, repo: &str) -> bool {
    let entry = entry.trim();
    if is_all(entry) {
        return true;
    }
    if entry.contains('/') {
        return entry.eq_ignore_ascii_case(repo);
    }
    repo.split('/').next().is_some_and(|owner| owner.eq_ignore_ascii_case(entry))
}

/// Whether the entries cover `repo`, given the lowercased logins of the hidden orgs: a wildcard
/// never reaches into a hidden org, an explicit org or repository entry does.
pub fn covers(entries: &[String], repo: &str, hidden: &BTreeSet<String>) -> bool {
    let owner = repo.split('/').next().unwrap_or_default().to_ascii_lowercase();
    entries.iter().any(|entry| {
        if is_all(entry) {
            !hidden.contains(&owner)
        } else {
            entry_matches(entry, repo)
        }
    })
}

/// The entries with every `*` replaced by the visible orgs, deduplicated case-insensitively and in
/// order. With no wildcard the entries come back as they were.
pub fn expand_all(entries: &[String], visible_orgs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        let names: Vec<&String> = if is_all(entry) {
            visible_orgs.iter().collect()
        } else {
            vec![entry]
        };
        for name in names {
            if !out.iter().any(|seen| seen.eq_ignore_ascii_case(name)) {
                out.push(name.clone());
            }
        }
    }
    out
}

impl App {
    /// The lowercased logins of the orgs the operator hid.
    pub fn hidden_orgs(&self) -> BTreeSet<String> {
        hidden_of(&self.all_org_settings())
    }

    /// The orgs "all" means right now: the workspaces the mothership knows (known, with settings, or
    /// with colonies, and the signed-in login), minus the switched-off and the hidden.
    pub async fn visible_orgs(&self) -> Vec<String> {
        let saved = self.all_org_settings();
        let known = self.known_orgs().unwrap_or_default();
        let colony_orgs: Vec<String> = self.sessions.read().await.iter().map(|s| s.org.clone()).collect();
        let awaiting: BTreeSet<String> = self.new_orgs.read().await.keys().cloned().collect();
        let mut scope = crate::backlog::scope_orgs(
            known.keys().map(String::as_str),
            &saved,
            colony_orgs.iter().map(String::as_str),
            &awaiting,
        );
        if let Some(own) = crate::github::cached_login(self).await
            && saved.get(&own).is_none_or(|s| orgs::org_enabled(s) && !s.hidden)
        {
            scope.insert(own);
        }
        scope.into_iter().filter(|o| orgs::valid_org(o)).collect()
    }

    /// The entries with `*` resolved against the visible orgs, at use time.
    pub async fn resolve_scope(&self, entries: &[String]) -> Vec<String> {
        if !entries.iter().any(|e| is_all(e)) {
            return entries.to_vec();
        }
        expand_all(entries, &self.visible_orgs().await)
    }
}

/// The hidden orgs of a settings map, lowercased.
pub fn hidden_of(all: &std::collections::BTreeMap<String, orgs::OrgSettings>) -> BTreeSet<String> {
    all.iter()
        .filter(|(_, s)| s.hidden)
        .map(|(org, _)| org.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orgs::OrgSettings;
    use std::collections::BTreeMap;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn entries_are_a_wildcard_an_org_or_a_repository() {
        for ok in ["*", " * ", "acme", "acme/api", "Keep-Shipping/web"] {
            assert!(valid_entry(ok), "{ok:?}");
        }
        for bad in ["", "a b", "acme/", "/api", "a/b/c", "**", "acme/*"] {
            assert!(!valid_entry(bad), "{bad:?}");
        }
    }

    #[test]
    fn an_entry_matches_by_wildcard_org_or_name_ignoring_case() {
        assert!(entry_matches("*", "acme/api"));
        assert!(entry_matches("ACME", "acme/api"));
        assert!(entry_matches("acme/API", "Acme/api"));
        assert!(!entry_matches("acme", "acmecorp/api"));
        assert!(!entry_matches("acme/web", "acme/api"));
    }

    #[test]
    fn a_wildcard_skips_hidden_orgs_but_a_named_entry_does_not() {
        let hidden = BTreeSet::from(["qzx".to_string()]);
        assert!(covers(&list(&["*"]), "acme/api", &hidden));
        assert!(!covers(&list(&["*"]), "QZX/api", &hidden), "hiding leaves the org out of All");
        assert!(
            covers(&list(&["*", "qzx/api"]), "qzx/api", &hidden),
            "naming it outright is explicit"
        );
        assert!(covers(&list(&["qzx"]), "qzx/api", &hidden));
        assert!(!covers(&[], "acme/api", &hidden), "an empty list covers nothing");
    }

    #[test]
    fn expanding_replaces_the_wildcard_with_the_visible_orgs_once() {
        let visible = list(&["acme", "globex"]);
        assert_eq!(expand_all(&list(&["*"]), &visible), list(&["acme", "globex"]));
        assert_eq!(
            expand_all(&list(&["Acme/api", "*"]), &visible),
            list(&["Acme/api", "acme", "globex"])
        );
        assert_eq!(expand_all(&list(&["acme/api"]), &visible), list(&["acme/api"]));
        assert!(expand_all(&list(&["*"]), &[]).is_empty());
    }

    #[test]
    fn the_hidden_set_is_lowercased_and_only_holds_hidden_orgs() {
        let mut all = BTreeMap::new();
        all.insert(
            "QZX".to_string(),
            OrgSettings {
                hidden: true,
                ..Default::default()
            },
        );
        all.insert("acme".to_string(), OrgSettings::default());
        assert_eq!(hidden_of(&all), BTreeSet::from(["qzx".to_string()]));
    }

    #[test]
    fn every_repository_setting_takes_the_wildcard_and_an_org() {
        // Per-org lists: merge_prs and close_superseded_prs.
        let all = OrgSettings {
            merge_prs: list(&["*"]),
            close_superseded_prs: list(&["acme"]),
            ..Default::default()
        };
        assert!(orgs::validate(&all).is_ok());
        assert!(orgs::merges_prs(&all, "acme/api"));
        assert!(orgs::closes_superseded_prs(&all, "ACME/web"));
        assert!(
            !orgs::closes_superseded_prs(&all, "other/web"),
            "an org entry is that org only"
        );
        let bad = OrgSettings {
            merge_prs: list(&["acme/*"]),
            ..Default::default()
        };
        assert!(orgs::validate(&bad).is_err());

        // Merge train loop: allow and local_checks.
        let loop_settings = crate::merge_loop::Settings {
            allow: list(&["*"]),
            local_checks: list(&["acme"]),
            ..Default::default()
        };
        let normal = crate::merge_loop::normalize(loop_settings).unwrap();
        assert_eq!(normal.allow, list(&["*"]));
        assert_eq!(normal.local_checks, list(&["acme"]));

        // Supply-chain and TypeScript loops.
        let supply = crate::supply_chain_loop::Settings {
            allow: list(&["*"]),
            ..Default::default()
        }
        .validated()
        .unwrap();
        assert!(supply.covers("any/thing"));
        let ts = crate::ts_any_loop::Settings {
            allow: list(&["*", "acme/app/"]),
            ..Default::default()
        };
        assert!(ts.clone().validated().is_err(), "a trailing slash is still not a repository");
        let ts = crate::ts_any_loop::Settings {
            allow: list(&["*"]),
            ..Default::default()
        }
        .validated()
        .unwrap();
        assert!(ts.covers("any/thing"));
    }

    #[test]
    fn a_hidden_org_is_out_of_the_wildcard_for_its_own_settings() {
        let org = OrgSettings {
            hidden: true,
            merge_prs: list(&["*"]),
            close_superseded_prs: list(&["*", "acme/api"]),
            ..Default::default()
        };
        assert!(!orgs::merges_prs(&org, "acme/api"), "All skips a hidden org");
        assert!(
            orgs::closes_superseded_prs(&org, "acme/api"),
            "naming the repository is explicit"
        );
        assert!(!orgs::closes_superseded_prs(&org, "acme/web"));
        let shown = OrgSettings { hidden: false, ..org };
        assert!(orgs::merges_prs(&shown, "acme/api"), "toggling back restores it");
    }

    #[test]
    fn hidden_survives_a_save_that_does_not_name_it_and_is_the_flag_when_it_does() {
        let saved = OrgSettings {
            hidden: true,
            ..Default::default()
        };
        let json = serde_json::to_value(&saved).unwrap();
        assert_eq!(json["hidden"], true);
        assert!(serde_json::to_value(OrgSettings::default()).unwrap().get("hidden").is_none());
        let back: OrgSettings = serde_json::from_value(json).unwrap();
        assert!(back.hidden);
        let old: OrgSettings = serde_json::from_value(serde_json::json!({"enabled": false})).unwrap();
        assert!(!old.hidden, "an install from before the switch hides nothing");
    }

    #[test]
    fn a_hidden_org_is_out_of_the_backlog_scope_and_the_repository_pickers() {
        let mut saved = BTreeMap::new();
        saved.insert(
            "qzx".to_string(),
            OrgSettings {
                hidden: true,
                ..Default::default()
            },
        );
        saved.insert("acme".to_string(), OrgSettings::default());
        let awaiting = BTreeSet::new();
        let scope = crate::backlog::scope_orgs(["acme", "qzx"], &saved, ["qzx"], &awaiting);
        assert_eq!(scope, BTreeSet::from(["acme".to_string()]));

        let hidden = hidden_of(&saved);
        let row = |name: &str| serde_json::json!({"full_name": name});
        assert!(crate::github::repo_is_hidden(&row("QZX/old"), &hidden));
        assert!(!crate::github::repo_is_hidden(&row("acme/api"), &hidden));
    }

    #[tokio::test]
    async fn a_hidden_org_keeps_its_colonies_and_is_still_in_the_workspace_list_for_the_toggle() {
        let root = std::env::temp_dir().join(format!("colonizer-hidden-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let mut s = crate::sessions::tests::colony("qzx", crate::sessions::SessionStatus::Running);
        s.id = "c1".into();
        app.sessions.write().await.push(s);
        let mut all = BTreeMap::new();
        all.insert(
            "qzx".to_string(),
            OrgSettings {
                hidden: true,
                ..Default::default()
            },
        );
        app.save_org_settings(&all).await.unwrap();
        let axum::Json(listed) = orgs::list(axum::extract::State(app.clone())).await;
        let qzx = listed
            .iter()
            .find(|o| o["org"] == "qzx")
            .expect("hidden orgs stay listed so Settings can toggle them");
        assert_eq!(qzx["settings"]["hidden"], true);
        assert_eq!(qzx["colonies"]["live"], 1, "its running colony is untouched");
        assert!(!app.visible_orgs().await.contains(&"qzx".to_string()));
        assert_eq!(app.sessions.read().await.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn resolving_uses_the_visible_orgs_at_use_time_and_toggling_back_restores_them() {
        let root = std::env::temp_dir().join(format!("colonizer-scope-{}", crate::util::short_id()));
        std::fs::create_dir_all(&root).unwrap();
        let app = crate::tests::test_app(&root);
        let settings = |hidden: bool| {
            let mut all = BTreeMap::new();
            all.insert("acme".to_string(), OrgSettings::default());
            all.insert(
                "qzx".to_string(),
                OrgSettings {
                    hidden,
                    ..Default::default()
                },
            );
            all
        };
        app.save_org_settings(&settings(true)).await.unwrap();
        assert_eq!(app.resolve_scope(&list(&["*"])).await, list(&["acme"]));
        assert_eq!(
            app.resolve_scope(&list(&["qzx"])).await,
            list(&["qzx"]),
            "no wildcard, nothing resolved"
        );
        app.save_org_settings(&settings(false)).await.unwrap();
        assert_eq!(app.resolve_scope(&list(&["*"])).await, list(&["acme", "qzx"]));
        let _ = std::fs::remove_dir_all(&root);
    }
}
