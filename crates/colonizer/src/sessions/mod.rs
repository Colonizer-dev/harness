//! Interactive sessions: one worktree + microVM + agent per task, bridged to browsers.
//!
//! Harness ⇄ VM traffic goes to `colonizer-agentd` over the private mesh (or a loopback port when the
//! mesh module is disabled). Agent events are persisted per session and fanned out to every open
//! browser; browser commands are forwarded to the agent.

use crate::{
    ApiResult, App, Shared, client_error,
    config::{ModulesConfig, setting, setting_str},
    diagnosis, github,
    modules::{AgentModule, schema_for},
    orgs,
    protocol::{Origin, QuestionRisk},
    restack, spend,
    stack::Stacked,
    store::SessionStore,
    util::{append_line, read_trimmed, short_id, truncate, valid_repo},
    watchdog::Activity,
};
// The boot sequence itself lives in boot.rs; re-exported here because lifecycle and queue reach
// `boot` through their `sessions::*` glob imports.
pub(crate) use crate::boot::boot;
#[allow(unused_imports)]
use crate::{events::*, lifecycle::*, publish::*, queue::*};
use anyhow::{Context, Result, bail};
use axum::{
    Json,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse as _, Response},
};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Mutex, broadcast, mpsc, watch},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{self, client::IntoClientRequest},
};

mod agentd;
mod api;
mod attention;
mod launch;
mod model;
mod persist;
mod runtime;
#[cfg(test)]
pub(crate) mod tests;

// Everything keeps its `crate::sessions::…` path: the split is by concern, not a new API. `persist`
// only adds methods to `App`, so it has nothing to re-export.
pub(crate) use agentd::*;
// By name: the glob imports of `lifecycle` and `publish` above bring a `routes` of their own.
pub(crate) use api::routes;
pub use api::*;
pub(crate) use attention::*;
pub use launch::*;
pub use model::*;
pub use runtime::*;

pub(crate) const AGENTD_PORT: u16 = 7070;
const MAX_LOGS: usize = 200;

/// Fixed failure messages the harness records verbatim. They double as the closed vocabulary
/// usage.rs buckets failures with: `Session.error` itself is free text and is never sent anywhere.
pub(crate) const AGENTD_NOT_READY: &str = "the agent daemon in the microVM did not become ready";
/// Recorded when a live colony's microVM is gone once the harness has restarted (`recover`).
pub(crate) const VM_GONE_AFTER_RESTART: &str = "the microVM was not running when the harness restarted";
/// Recorded when a live colony's microVM stops on its own (its max session length) or the host stopped it.
pub(crate) const VM_STOPPED_EARLY: &str =
    "the microVM stopped (its max session length, or the host stopped it); press Resume to continue";
/// Recorded for a colony that was mid-publish when the harness restarted.
pub(crate) const PUBLISH_LOST_TO_RESTART: &str = "the harness restarted while publishing; the worktree is intact, publish again";
