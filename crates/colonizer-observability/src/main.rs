//! `colonizer-observability`: the add-on process the mothership spawns (docs/design/observability.md,
//! Supervision and Contract).
//!
//! ```text
//! colonizer-observability --version [--json]
//! colonizer-observability run --contract <data>/observability/exporter.json
//! colonizer-observability send-test-event --contract <data>/observability/exporter.json
//! ```
//!
//! Both commands read one JSON line of secrets from stdin first (`{"headers": "k=v,…"}`). `run`
//! then exports until its stdin closes — the mothership's death ends it without a heartbeat — and
//! exits 78 when the contract or version is refused. It never prints a header value.

use colonizer_observability::contract::{self, CONTRACT, EXIT_REFUSED};
use colonizer_observability::exporter::{self, Exporter};
use std::path::PathBuf;
use std::process::ExitCode;
use tokio::io::AsyncReadExt;

fn usage() -> ExitCode {
    eprintln!("usage: colonizer-observability (--version [--json] | run --contract <path> | send-test-event --contract <path>)");
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
            let mut exporter = match Exporter::new(&path, headers) {
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
            loop {
                let wait = exporter.tick().await;
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = stop_rx.changed() => break,
                }
            }
            exporter.shutdown();
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}
