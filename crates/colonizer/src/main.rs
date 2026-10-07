//! Colonizer: turn a task into a pull request by running a coding agent in a microVM, with a
//! web UI for chat (questions as choice cards), a terminal in the VM, and a private mesh network
//! between the harness and every VM. Every moving part is a module; see docs/architecture.md.
//!
//! Trust model: a microVM only sees its worktree (rw), the repository's git objects (ro), its
//! session files (ro) and an output directory (rw). The GitHub token never enters a VM; the Claude
//! credential is injected by microsandbox's host-side TLS proxy for the API host only, and model
//! provider keys are added by the mothership's provider gateway.

// One line per module, kept in alphabetical order: a module added in its own place in the list
// does not touch the lines a parallel pull request adds for another one.
mod account_health;
mod activity;
mod answer_cache;
mod answer_tokens;
mod api_tokens;
mod app;
mod archive;
mod auth;
mod authority;
mod autonomy;
mod backlog;
mod blocked;
mod boot;
mod boundary;
mod brief_pick;
mod built_with;
mod burn_down;
mod cache_store;
mod capacity;
mod chat;
mod chat_images;
mod claims;
mod claude_accounts;
mod claude_login;
mod cli;
mod code;
mod colonize;
mod colony_secrets;
mod commit_links;
mod config;
#[doc(hidden)]
pub mod contract;
mod coordination;
mod decide;
mod decisions;
mod deja;
mod deps;
mod diagnosis;
mod disk_cleanup;
mod docs_loop;
mod drain;
mod duplicates;
mod egress;
mod epic;
mod events;
mod exec_bits;
mod exec_policy;
mod execution;
mod features;
mod findings;
mod fleet;
mod fleet_export;
mod fleet_health;
mod fleet_history;
mod fleet_members;
mod fleet_policy;
mod fleet_sync;
mod gateway;
mod gateway_audit;
mod github;
mod github_breaker;
mod graft;
mod handoff;
mod headroom;
mod history;
mod hotspots;
mod hunters;
mod idle_park;
mod ignore;
mod img_proxy;
mod ipv6;
mod jev;
mod jev_ladder;
mod ledger;
mod lifecycle;
mod login_item;
mod loop_github;
mod loops;
mod maps;
mod mcp;
mod mem0;
mod memory;
mod merge_head;
mod merge_loop;
mod merge_steward;
mod merge_train;
mod mesh;
mod model_switch;
mod modules;
mod needs_feed;
mod notify;
mod observability;
mod openai;
mod orgs;
mod packages;
mod path_policy;
mod phone;
mod placement;
mod playbook;
mod plugins;
mod prescan;
mod presets;
mod previews;
mod protocol;
mod provider_history;
mod provider_quota;
mod providers;
mod publish;
mod push;
mod push_prefs;
mod queue;
mod queue_priority;
mod quota_cards;
mod rebase;
mod reclaim;
mod recovery;
use colonizer_redact as redact;
mod redteam;
mod remote;
mod repo_identity;
mod repo_meta;
mod restack;
mod retry;
mod routing;
mod runtime;
mod sandbox;
mod schedule;
mod screen;
mod secrets;
mod sensitivity;
mod server;
mod services;
mod sessions;
mod setup;
mod snapshot;
mod spend;
mod stack;
mod stale;
mod status;
mod store;
mod store_config;
mod store_s3;
mod stream;
mod summaries;
mod supersede;
mod supply_chain_loop;
mod switch_agent;
mod telemetry;
mod timing;
mod transcript;
mod ts_any_loop;
mod uhp;
mod uhp_responses;
mod update;
mod update_notices;
mod upload;
mod usage;
mod util;
mod validation;
mod vault;
mod verify;
mod verify_focus;
mod version;
mod voice;
mod watchdog;

// The paths the rest of the crate has always used (`crate::App`, `crate::AppError`, …), kept
// stable while their definitions live in `app`, `answer_cache` and `server`.
pub use answer_cache::{AnswerCache, cached_answer, cached_answer_nowait, with_cache_info};
#[cfg(test)]
pub(crate) use app::tests;
pub use app::{
    ApiResult, App, AppError, CLAUDE_API_HOST, ClaudeCred, Shared, StorageAlert, StorageAlertKind, client_error,
    resolve_guest_claude_bin, resolve_host_claude_bin,
};
pub(crate) use app::{config_unreadable, move_corrupt_aside};
pub(crate) use config::Settings;
pub(crate) use server::serve;

use std::process::ExitCode;

/// The definitions and the runners live in `cli` (the MCP server in `mcp`); this stays a parse and
/// a dispatch. An argument nobody planned for is clap's usage error, not a silently started server.
///
/// `pub` and `#[doc(hidden)]` because this file is also the library root (see `Cargo.toml`): the
/// `colonizer` binary is a thin wrapper that calls it, and the repository-level checks in
/// `crates/repo-contracts` link the library, not the binary.
#[doc(hidden)]
#[tokio::main]
pub async fn main() -> ExitCode {
    ExitCode::from(cli::run(cli::parse()).await as u8)
}
