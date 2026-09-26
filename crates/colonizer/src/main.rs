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
mod activity;
mod answer_cache;
mod api_tokens;
mod app;
mod archive;
mod auth;
mod authority;
mod autonomy;
mod boot;
mod burn_down;
mod cache_store;
mod chat;
mod chat_images;
mod claims;
mod claude_accounts;
mod claude_login;
mod cli;
mod code;
mod colonize;
mod colony_secrets;
mod config;
mod deps;
mod diagnosis;
mod egress;
mod epic;
mod events;
mod exec_bits;
mod execution;
mod findings;
mod fleet;
mod gateway;
mod gateway_audit;
mod github;
mod graft;
mod headroom;
mod hunters;
mod img_proxy;
mod jev;
mod jev_ladder;
mod ledger;
mod lifecycle;
mod login_item;
mod loops;
mod maps;
mod mcp;
mod mem0;
mod memory;
mod mesh;
mod modules;
mod notify;
mod openai;
mod orgs;
mod packages;
mod path_policy;
mod plugins;
mod presets;
mod protocol;
mod provider_quota;
mod providers;
mod publish;
mod push;
mod queue;
mod rebase;
mod reclaim;
mod redteam;
mod remote;
mod repo_meta;
mod restack;
mod routing;
mod runtime;
mod sandbox;
mod schedule;
mod screen;
mod secrets;
mod sensitivity;
mod server;
mod sessions;
mod spend;
mod stack;
mod stale;
mod status;
mod store;
mod stream;
mod summaries;
mod telemetry;
mod timing;
mod update;
mod usage;
mod util;
mod validation;
mod verify;
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
#[tokio::main]
async fn main() -> ExitCode {
    ExitCode::from(cli::run(cli::parse()).await as u8)
}
