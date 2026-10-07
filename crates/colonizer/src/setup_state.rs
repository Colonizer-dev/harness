//! Setup's "don't ask again" (issue #1200): the advisory rows of the Setup checklist (the Stack and the
//! live map) can be marked done for good. The answer is kept per host in `<config>/setup.json`, so it
//! survives a restart and is the same from every browser. Required rows (machine, GitHub, Claude) can
//! never be dismissed.
//!
//! `GET /api/setup` returns `{"dismissed": ["stack", ...]}`; `PUT /api/setup {"id": "stack",
//! "dismissed": true}` adds or removes one id. Both are owner-only to scoped API tokens.

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

use crate::{ApiResult, Shared, app::client_error, util};

/// The rows that may be dismissed: advisory ones only.
const DISMISSIBLE: &[&str] = &["map", "stack"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SetupState {
    #[serde(default)]
    pub dismissed: BTreeSet<String>,
}

impl SetupState {
    fn load(path: &Path) -> Self {
        let mut state: Self = std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        state.dismissed.retain(|id| DISMISSIBLE.contains(&id.as_str()));
        state
    }

    fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        util::write_private(path, &serde_json::to_vec_pretty(self)?)
    }

    /// Applies one change; false when `id` is not a dismissible row.
    fn set(&mut self, id: &str, dismissed: bool) -> bool {
        if !DISMISSIBLE.contains(&id) {
            return false;
        }
        if dismissed {
            self.dismissed.insert(id.to_string());
        } else {
            self.dismissed.remove(id);
        }
        true
    }
}

/// Serialises the read-modify-write of the one file.
static WRITE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn path(app: &Shared) -> std::path::PathBuf {
    app.cfg.config_dir.join("setup.json")
}

/// `GET /api/setup`
pub async fn status(State(app): State<Shared>) -> Json<SetupState> {
    Json(SetupState::load(&path(&app)))
}

#[derive(Deserialize)]
pub struct SetRequest {
    id: String,
    dismissed: bool,
}

/// `PUT /api/setup` — `{"id": "stack", "dismissed": true|false}`
pub async fn put(State(app): State<Shared>, Json(body): Json<SetRequest>) -> ApiResult<SetupState> {
    let _guard = WRITE.lock().await;
    let file = path(&app);
    let mut state = SetupState::load(&file);
    if !state.set(&body.id, body.dismissed) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "only the advisory rows (stack, map) can be dismissed",
        ));
    }
    state.save(&file)?;
    Ok(Json(state))
}

pub(crate) fn routes() -> axum::Router<Shared> {
    use axum::routing;
    axum::Router::new().route("/api/setup", routing::get(status).put(put))
}

/// This module's feature descriptor (`features.rs`). No `token_scope`: the routes stay owner-only.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "setup_state",
    routes,
    token_scope: None,
    activity: ACTIVITY,
    kinds: &[],
    start_tasks: None,
};

const ACTIVITY: &[crate::activity::Rule] = &[crate::activity::rule(
    "PUT",
    "/api/setup",
    "settings.save",
    crate::activity::Target::Named("setup checklist", "setup"),
)];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismissal_survives_a_reload_from_disk() {
        let dir = std::env::temp_dir().join(format!("setup-state-{}", std::process::id()));
        let file = dir.join("setup.json");
        let mut state = SetupState::load(&file);
        assert!(state.dismissed.is_empty());
        assert!(state.set("stack", true));
        state.save(&file).unwrap();
        assert!(SetupState::load(&file).dismissed.contains("stack"));
        let mut again = SetupState::load(&file);
        assert!(again.set("stack", false));
        again.save(&file).unwrap();
        assert!(SetupState::load(&file).dismissed.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn required_rows_cannot_be_dismissed() {
        let mut state = SetupState::default();
        for id in ["machine", "github", "claude", "launch", "nonsense"] {
            assert!(!state.set(id, true), "{id}");
        }
        assert!(state.dismissed.is_empty());
    }

    #[test]
    fn a_hand_edited_file_cannot_dismiss_a_required_row() {
        let dir = std::env::temp_dir().join(format!("setup-state-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("setup.json");
        std::fs::write(&file, br#"{"dismissed":["github","map"]}"#).unwrap();
        let state = SetupState::load(&file);
        assert_eq!(state.dismissed.into_iter().collect::<Vec<_>>(), ["map"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
