//! `colonizer-observability`: the add-on process the mothership spawns (docs/design/observability.md,
//! Supervision and Contract).
//!
//! ```text
//! colonizer-observability --version [--json]
//! colonizer-observability run --contract <data>/observability/exporter.json
//! colonizer-observability send-test-event --contract <data>/observability/exporter.json
//! colonizer-observability backfill --contract <data>/observability/exporter.json [--since <when>] [--until <when>] [--repo <owner/name>] [--rate <n>] [--dry-run] [--include-archive]
//! colonizer-observability export --contract <data>/observability/exporter.json --out <file|dir> [--since <when>] [--until <when>] [--signals logs,traces] [--include-archive] [--content]
//! ```
//!
//! Every command reads one JSON line of secrets from stdin first (`{"headers": "k=v,…"}`). `run`
//! then exports until its stdin closes — the mothership's death ends it without a heartbeat — and
//! exits 78 when the contract or version is refused. `backfill` (#858) replays the ledgers'
//! on-disk history to the backend once, printing one JSON summary on stdout; it stops at its
//! stdin's end too, and its flags are parsed by [`colonizer_observability::backfill::Options`].
//! `export` (#859) is the offline backfill: it sends nothing, writing every OTLP request as one
//! OTLP/JSON line to `--out` instead, and answers with one JSON summary line; the secrets line is
//! read and dropped. It never prints a header value.

#![allow(clippy::disallowed_macros)] // standalone add-on: no tracing subscriber; the mothership supervisor captures this stderr

use colonizer_observability::backfill::{Backfill, Options as BackfillOptions};
use colonizer_observability::contract::{self, CONTRACT, EXIT_REFUSED};
use colonizer_observability::export::{self, Options as ExportOptions};
use colonizer_observability::exporter::{self, Exporter};
use colonizer_observability::relay::Relay;
use std::path::PathBuf;
use std::process::ExitCode;
use tokio::io::AsyncReadExt;

fn usage() -> ExitCode {
    eprintln!(
        "usage: colonizer-observability (--version [--json] | run --contract <path> | send-test-event --contract <path> | backfill --contract <path> [--since <when>] [--until <when>] [--repo <owner/name>] [--rate <n>] [--dry-run] [--include-archive] | export --contract <path> --out <file|dir> [--since <when>] [--until <when>] [--signals logs,traces] [--include-archive] [--content])"
    );
    ExitCode::from(2)
}

fn contract_arg(args: &[String]) -> Option<PathBuf> {
    let i = args.iter().position(|a| a == "--contract")?;
    args.get(i + 1).map(PathBuf::from)
}

fn read_headers() -> Result<Vec<(String, String)>, String> {
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    contract::read_secrets(&mut lock)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") => {
            let version = env!("CARGO_PKG_VERSION");
            if args.iter().any(|a| a == "--json") {
                println!(
                    "{}",
                    serde_json::json!({"name": "colonizer-observability", "version": version, "contract": CONTRACT})
                );
            } else {
                println!("colonizer-observability {version} (contract {CONTRACT})");
            }
            ExitCode::SUCCESS
        }
        Some("send-test-event") => {
            let Some(path) = contract_arg(&args) else {
                return usage();
            };
            let headers = match read_headers() {
                Ok(h) => h,
                Err(e) => {
                    println!("{}", serde_json::json!({"ok": false, "error": e}));
                    return ExitCode::FAILURE;
                }
            };
            let contract = match contract::load(&path) {
                Ok(c) => c,
                Err(e) => {
                    println!("{}", serde_json::json!({"ok": false, "error": e}));
                    return ExitCode::FAILURE;
                }
            };
            let result = exporter::send_test(&contract, headers).await;
            println!("{result}");
            if result["ok"] == true {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("run") => {
            let Some(path) = contract_arg(&args) else {
                return usage();
            };
            let headers = match read_headers() {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("observability: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let mut exporter = match Exporter::new(&path, headers.clone()) {
                Ok(x) => x,
                Err(e) if e.starts_with("refused") => {
                    eprintln!("observability: {e}");
                    // The contract may be one this build cannot read in full; its data dir is enough.
                    let data_dir = std::fs::read(&path)
                        .ok()
                        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                        .and_then(|v| v.get("data_dir").and_then(|d| d.as_str()).map(PathBuf::from));
                    if let Some(dir) = data_dir {
                        let status = serde_json::json!({"state": "refused", "last_error": e, "contract": CONTRACT, "version": env!("CARGO_PKG_VERSION")});
                        let _ = std::fs::write(exporter::status_path(&dir), status.to_string());
                    }
                    return ExitCode::from(EXIT_REFUSED as u8);
                }
                Err(e) => {
                    eprintln!("observability: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // The mothership holds our stdin open; its end is our cue to stop.
            let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
            tokio::spawn(async move {
                let mut stdin = tokio::io::stdin();
                let mut buf = [0u8; 256];
                while matches!(stdin.read(&mut buf).await, Ok(n) if n > 0) {}
                let _ = stop_tx.send(true);
            });
            // The fleet relay (#861): `None` unless the contract turns it on. On its own task, so
            // a slow pass over the fleet never delays the live export; it sleeps between passes
            // for as long as its tick says — a slow idle cadence once there is nothing to do.
            if let Some(mut relay) = Relay::new(&path, headers) {
                tokio::spawn(async move {
                    loop {
                        let wait = relay.tick().await;
                        tokio::time::sleep(wait).await;
                    }
                });
            }
            loop {
                // The tick races the stop signal (#850): a request still in flight when the
                // mothership goes away is cancelled, not awaited — its batch is neither sent on
                // nor committed, nothing further is written, and the restart replays it from the
                // committed cursors.
                let wait = tokio::select! {
                    wait = exporter.tick() => wait,
                    _ = stop_rx.changed() => break,
                };
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = stop_rx.changed() => break,
                }
            }
            exporter.shutdown();
            ExitCode::SUCCESS
        }
        Some("backfill") => {
            let Some(path) = contract_arg(&args) else {
                return usage();
            };
            let opts = match BackfillOptions::from_args(&args[1..]) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("backfill: {e}");
                    return usage();
                }
            };
            let headers = match read_headers() {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("observability: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let mut backfill = match Backfill::new(&path, headers, &opts) {
                Ok(b) => b,
                Err(e) if e.starts_with("refused") => {
                    eprintln!("observability: {e}");
                    return ExitCode::from(EXIT_REFUSED as u8);
                }
                Err(e) => {
                    eprintln!("observability: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // The mothership holds our stdin open; its end is our cue to stop what is in flight.
            let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
            tokio::spawn(async move {
                let mut stdin = tokio::io::stdin();
                let mut buf = [0u8; 256];
                while matches!(stdin.read(&mut buf).await, Ok(n) if n > 0) {}
                let _ = stop_tx.send(true);
            });
            while !backfill.done() {
                let wait = backfill.tick().await;
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = stop_rx.changed() => backfill.cancel(),
                }
            }
            let summary = backfill.summary();
            println!("{summary}");
            if summary["state"] == "failed" {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Some("export") => {
            let Some(path) = contract_arg(&args) else {
                return usage();
            };
            let opts = match ExportOptions::from_args(&args[1..]) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("export: {e}");
                    return usage();
                }
            };
            // The secrets line is read and dropped: nothing is sent, so nothing needs a header.
            if let Err(e) = read_headers() {
                eprintln!("export: {e}");
                return ExitCode::FAILURE;
            }
            // The whole export is blocking I/O; it runs off the async threads, like the archive
            // reader it may spawn.
            let exported = tokio::task::spawn_blocking(move || export::run(&path, &opts)).await;
            match exported {
                Ok(Ok(summary)) => {
                    println!("{summary}");
                    ExitCode::SUCCESS
                }
                Ok(Err(e)) if e.starts_with("refused") => {
                    eprintln!("export: {e}");
                    ExitCode::from(EXIT_REFUSED as u8)
                }
                Ok(Err(e)) => {
                    eprintln!("export: {e}");
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("export: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => usage(),
    }
}
