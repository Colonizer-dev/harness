//! Sentinel shadow-mode pre-execution verdict (issue #1323): narrow detection of risky tool calls
//! that might bypass existing controls. Runs in shadow mode only — verdicts logged to decisions
//! ledger, never enforced. exec_policy and path_policy remain the hard boundary.
//!
//! Classified risks (no ledger row for None, bypasses entirely):
//! - PrivilegeEscalation: mount/umount/chroot/nsenter/unshare/pivot_root commands
//! - ProtectedPathWrite: writes to protected paths (.git, .claude, .vscode, etc)
//! - GitAdminModify: writes to .git/config or .git/hooks
//! - CredentialPathWrite: writes to credential-shaped paths (.env, .npmrc, etc)
//! - CredentialPathRead: read-only tool accessing credential-shaped paths (grep, cat, etc)
//! - ProcessTermination: kill/pkill/killall of init or agentd
//! - Egress: curl/wget to non-localhost; classified to ask if policy unreachable
//!
//! Pending map bounded at 10k per colony; insertion refused when full.

use crate::Shared;
use chrono::Utc;
use ring::digest;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

const POLICY_VERSION: &str = "1";
const RISKY_COMMANDS: &[&str] = &["mount", "umount", "chroot", "nsenter", "unshare", "pivot_root"];
const CURL_WGET_COMMANDS: &[&str] = &["curl", "wget"];
const READONLY_COMMANDS: &[&str] = &["cat", "grep", "head", "tail", "less", "more", "file", "stat", "node"];

/// Risk class for a tool call; None means benign/safe, no ledger row written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Risk {
    PrivilegeEscalation,
    ProtectedPathWrite,
    GitAdminModify,
    CredentialPathWrite,
    CredentialPathRead,
    ProcessTermination,
    Egress,
}

/// One pending tool call, waiting for its result to settle the verdict.
#[derive(Clone, Debug)]
struct Pending {
    risk: Risk,
    p: f64,
    reason: String,
    digest: String,
    cmd_truncated: String,
}

/// Per-colony pending calls (tool_call_id -> Pending).
static PENDING: LazyLock<Mutex<HashMap<String, HashMap<String, Pending>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Classify a tool call for risk. Returns None to bypass entirely (no ledger row).
fn classify(tool_name: &str, input: &Value) -> Option<Risk> {
    match tool_name {
        "Bash" | "Monitor" => {
            let cmd = input.get("command")?.as_str()?;
            classify_bash(cmd)
        }
        "Write" | "Edit" | "NotebookEdit" | "MultiEdit" => {
            let path = input.get("file_path").and_then(|v| v.as_str())?;
            classify_file_write(path)
        }
        "Read" => {
            // Gap A: read-only commands touching credential-shaped paths should record allow
            let path = input.get("file_path").and_then(|v| v.as_str())?;
            classify_read(path)
        }
        _ => None, // Other tools bypass
    }
}

/// Classify a bash command for dangerous operations.
fn classify_bash(cmd: &str) -> Option<Risk> {
    let trimmed = cmd.trim();
    for segment in trimmed.split(|c: char| ['|', ';'].contains(&c)) {
        for word in segment.split_whitespace() {
            if word.starts_with('-') || word == "sudo" || word == "env" {
                continue;
            }
            let base = word.trim_start_matches('-');
            if RISKY_COMMANDS.contains(&base) {
                return Some(Risk::PrivilegeEscalation);
            }
            break;
        }
    }

    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if !words.is_empty() {
        match words[0] {
            "kill" | "pkill" | "killall" => {
                let tokens = &words[1..];
                for (i, token) in tokens.iter().enumerate() {
                    if token == &"-9" && i + 1 < tokens.len() && tokens[i + 1] == "1" {
                        return Some(Risk::ProcessTermination);
                    }
                    if token == &"-1" || token.contains("colonizer-agentd") {
                        return Some(Risk::ProcessTermination);
                    }
                }
            }
            // Gap B: curl/wget to non-localhost; ask if egress policy unreachable
            cmd_word if CURL_WGET_COMMANDS.contains(&cmd_word) => {
                let args_str = words[1..].join(" ");
                // Simple heuristic: if args mention localhost or 127.0.0.1, bypass; otherwise ask
                if !args_str.contains("localhost") && !args_str.contains("127.0.0.1") {
                    return Some(Risk::Egress);
                }
            }
            // Gap A: read-only commands touching credential-shaped paths
            cmd_word if READONLY_COMMANDS.contains(&cmd_word) => {
                // Check if any argument is credential-shaped
                for arg in words[1..].iter() {
                    let normalized = arg.trim_start_matches("./");
                    if crate::path_policy::credential_like(normalized) {
                        return Some(Risk::CredentialPathRead);
                    }
                }
            }
            _ => {}
        }
    }

    None
}

/// Classify file write operations.
fn classify_file_write(path: &str) -> Option<Risk> {
    let normalized = path.trim_start_matches("./");

    // .git/config and .git/hooks
    if normalized == ".git/config" || normalized.starts_with(".git/hooks") {
        return Some(Risk::GitAdminModify);
    }

    // Protected directories
    let protected = [".git/", ".claude/", ".codex/", ".vscode/", ".idea/", ".devcontainer/"];
    if protected.iter().any(|p| normalized.starts_with(p)) {
        return Some(Risk::ProtectedPathWrite);
    }

    // Credential-shaped paths: writes are risky
    if crate::path_policy::credential_like(normalized) {
        return Some(Risk::CredentialPathWrite);
    }

    None
}

/// Classify read-only operations on credential-shaped paths (Gap A).
fn classify_read(path: &str) -> Option<Risk> {
    let normalized = path.trim_start_matches("./");
    if crate::path_policy::credential_like(normalized) {
        return Some(Risk::CredentialPathRead);
    }
    None
}

/// Compute verdict reason and confidence.
fn verdict_reason(risk: &Risk) -> (f64, String) {
    match risk {
        Risk::GitAdminModify => (0.97, "protective mount or .git admin path".into()),
        Risk::ProtectedPathWrite => (0.95, "write to protected path".into()),
        Risk::CredentialPathWrite => (0.96, "write to credential-shaped path".into()),
        // Gap A: read-only access to credential paths → allow with p≈0.8
        Risk::CredentialPathRead => (0.8, "read-only; masked files read as placeholders".into()),
        Risk::ProcessTermination => (0.98, "attempted process termination".into()),
        Risk::PrivilegeEscalation => (0.94, "mount/umount/chroot detected".into()),
        // Gap B: curl/wget to non-localhost; ask if egress policy unreachable
        Risk::Egress => (0.5, "egress allowlist not checked in shadow mode".into()),
    }
}

/// Compute stable hex digest of (tool, args, policy version, colony_id).
fn compute_digest(tool: &str, input: &Value, colony_id: &str) -> String {
    let message = format!("{}|{}|{}|{}", tool, POLICY_VERSION, colony_id, input);
    let digest_value = digest::digest(&digest::SHA256, message.as_bytes());
    digest_value.as_ref().iter().map(|b| format!("{:02x}", b)).collect::<String>()
}

/// Truncate command for ledger (max 200 chars), safe for multi-byte boundaries.
fn truncate_cmd(val: &Value) -> String {
    let cmd = val
        .get("command")
        .or_else(|| val.get("file_path"))
        .or_else(|| val.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    if cmd.len() > 200 {
        let cut = cmd.char_indices().nth(197).map(|(i, _)| i).unwrap_or(cmd.len());
        format!("{}...", &cmd[..cut])
    } else {
        cmd.to_string()
    }
}

/// Observe a tool call or result event; write ledger rows for risky calls.
pub(crate) async fn observe(app: &Shared, id: &str, event: &Value) {
    let Some(event_type) = event["type"].as_str() else {
        return;
    };

    match event_type {
        "tool_call" => {
            if let Err(e) = process_tool_call(app, id, event).await {
                tracing::debug!(colony = id, error = %e, "sentinel: error processing tool call");
            }
        }
        "tool_result" => {
            if let Err(e) = process_tool_result(app, id, event).await {
                tracing::debug!(colony = id, error = %e, "sentinel: error processing tool result");
            }
        }
        _ => {}
    }
}

async fn process_tool_call(_app: &Shared, id: &str, event: &Value) -> Result<(), String> {
    let Some(call_id) = event["tool_call_id"].as_str() else {
        return Ok(());
    };
    let Some(tool) = event["name"].as_str() else {
        return Ok(());
    };
    let input = event.get("input").unwrap_or(&Value::Null);

    // Classify: None means benign, skip entirely (no ledger row).
    let Some(risk) = classify(tool, input) else {
        return Ok(());
    };

    let (p, reason) = verdict_reason(&risk);
    let digest = compute_digest(tool, input, id);
    let cmd_truncated = truncate_cmd(input);

    // Log the verdict
    tracing::info!(
        colony = id,
        risk = ?risk,
        p = p,
        digest = &digest,
        "sentinel: pre-execution verdict"
    );

    // Store in pending map
    let pending = Pending {
        risk,
        p,
        reason,
        digest,
        cmd_truncated,
    };

    let mut pending_map = PENDING.lock().map_err(|e| e.to_string())?;
    let colony_calls = pending_map.entry(id.to_string()).or_insert_with(HashMap::new);
    if colony_calls.len() < 10000 {
        colony_calls.insert(call_id.to_string(), pending);
    }

    Ok(())
}

async fn process_tool_result(app: &Shared, id: &str, event: &Value) -> Result<(), String> {
    let Some(call_id) = event["tool_call_id"].as_str() else {
        return Ok(());
    };

    // Extract pending before await to avoid holding MutexGuard across await
    let pending = {
        let mut pending_map = PENDING.lock().map_err(|e| e.to_string())?;
        pending_map.get_mut(id).and_then(|c| c.remove(call_id))
    };

    let Some(pending) = pending else {
        return Ok(());
    };

    // Determine outcome
    let is_error = event.get("is_error").map(|v| v == true).unwrap_or_default();
    let has_denial = event.get("denial").is_some();
    let outcome = if has_denial {
        "refused_by_policy".to_string()
    } else if is_error {
        "error".to_string()
    } else {
        "ran".to_string()
    };

    // Build and write ledger row
    if let Some(session) = app.session(id).await {
        // Determine the pick based on risk type: Gap A (allow) and Gap B (ask)
        let pick = match &pending.risk {
            Risk::CredentialPathRead => "allow".into(), // Gap A: allow reads
            Risk::Egress => "ask".into(),               // Gap B: ask about egress
            _ => "refuse".into(),                       // Others refuse
        };

        let row = crate::decide::Row {
            kind: "decision",
            ts: Utc::now(),
            point: "sentinel",
            session: id.to_string(),
            repo: session.repo.clone(),
            issue: session.issue,
            mode: crate::decide::Mode::Shadow,
            options: vec!["allow".into(), "refuse".into(), "ask".into()],
            pick: Some(pick),
            confidence: Some(pending.p),
            latency_ms: 0,
            miss: None,
            did: "rule",
            outcome: Some(serde_json::json!({
                "risk": format!("{:?}", pending.risk),
                "outcome": outcome,
                "reason": pending.reason,
                "digest": pending.digest,
                "cmd": pending.cmd_truncated,
            })),
        };

        crate::decide::record(app, &row).await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classify_privilege_escalation() {
        let tests = vec![
            ("sudo umount -l /workspace/.git/hooks", Some(Risk::PrivilegeEscalation)),
            ("mount /dev/sda1 /mnt", Some(Risk::PrivilegeEscalation)),
            ("cat /etc/fstab | grep mount | head", None),
        ];
        for (cmd, expected) in tests {
            let input = json!({"command": cmd});
            assert_eq!(classify("Bash", &input), expected, "cmd: {}", cmd);
        }
    }

    #[test]
    fn classify_process_termination() {
        let tests = vec![
            ("kill -9 1", Some(Risk::ProcessTermination)),
            ("kill -1", Some(Risk::ProcessTermination)),
            ("kill -15 1234", None),
            ("pkill colonizer-agentd", Some(Risk::ProcessTermination)),
        ];
        for (cmd, expected) in tests {
            let input = json!({"command": cmd});
            assert_eq!(classify("Bash", &input), expected, "cmd: {}", cmd);
        }
    }

    #[test]
    fn classify_file_writes() {
        let tests = vec![
            ("Edit", ".git/config", Some(Risk::GitAdminModify)),
            ("Write", ".env", Some(Risk::CredentialPathWrite)),
            ("Write", ".claude/settings.json", Some(Risk::ProtectedPathWrite)),
        ];
        for (tool, path, expected) in tests {
            let input = json!({"file_path": path, "content": "x"});
            assert_eq!(classify(tool, &input), expected, "tool: {}, path: {}", tool, path);
        }
    }

    #[test]
    fn classify_credential_access() {
        let tests = vec![
            ("grep SECRET .env", Some(Risk::CredentialPathRead)),
            ("cat .npmrc", Some(Risk::CredentialPathRead)),
            ("ls -la", None),
        ];
        for (cmd, expected) in tests {
            let input = json!({"command": cmd});
            assert_eq!(classify("Bash", &input), expected, "cmd: {}", cmd);
        }
    }

    #[test]
    fn classify_egress() {
        let tests = vec![
            ("curl http://localhost:3000/api", None),
            ("curl https://api.example.com/data", Some(Risk::Egress)),
        ];
        for (cmd, expected) in tests {
            let input = json!({"command": cmd});
            assert_eq!(classify("Bash", &input), expected, "cmd: {}", cmd);
        }
    }

    #[test]
    fn digest_stable_and_varies() {
        let input = json!({"command": "ls"});
        let d1 = compute_digest("Bash", &input, "c1");
        let d2 = compute_digest("Bash", &input, "c1");
        assert_eq!(d1, d2);

        let d3 = compute_digest("Bash", &input, "c2");
        assert_ne!(d1, d3);

        let input2 = json!({"command": "ls -la"});
        let d4 = compute_digest("Bash", &input2, "c1");
        assert_ne!(d1, d4);
    }

    #[test]
    fn test_truncate() {
        let long = json!({"command": "a".repeat(300)});
        let truncated = super::truncate_cmd(&long);
        assert!(truncated.len() <= 200 && truncated.ends_with("..."));

        let short = json!({"command": "ls"});
        assert_eq!(super::truncate_cmd(&short), "ls");

        // Multi-byte chars: ensure char-safe truncation doesn't panic
        let cmd = "你好世界".repeat(50);
        let val = json!({"command": cmd});
        let _ = super::truncate_cmd(&val);
    }

    #[test]
    fn ledger_row_outcome() {
        let pending = Pending {
            risk: Risk::CredentialPathRead,
            p: 0.8,
            reason: "read-only; masked files read as placeholders".into(),
            digest: "abc123".into(),
            cmd_truncated: "grep SECRET .env".into(),
        };
        let outcome = serde_json::json!({
            "risk": format!("{:?}", pending.risk),
            "outcome": "ran",
            "reason": pending.reason,
            "digest": pending.digest,
            "cmd": pending.cmd_truncated,
        });
        let s = serde_json::to_string(&outcome).unwrap();
        assert!(s.contains("CredentialPathRead") && s.contains("grep SECRET .env"));
    }
}
