//! Talking to `colonizer-agentd` inside a colony's microVM: dialling it over the mesh or the loopback
//! port, and its HTTP and WebSocket endpoints.

use super::*;

/// Maps agent settings to runner env vars via each schema property's `env` key.
pub(crate) fn agent_env(agent: &AgentModule, choice: &crate::config::ModuleChoice) -> Map<String, Value> {
    let mut env = Map::new();
    if let Some(properties) = agent.schema["properties"].as_object() {
        for (key, spec) in properties {
            let (Some(var), Some(value)) = (spec["env"].as_str(), setting(choice, &agent.schema, key)) else {
                continue;
            };
            let value = match value {
                Value::String(s) => s.clone(),
                Value::Null => continue,
                other => other.to_string(),
            };
            if !value.is_empty() {
                env.insert(var.to_string(), Value::String(value));
            }
        }
    }
    if agent.needs_claude {
        env.insert("COLONIZER_CLAUDE_BIN".into(), Value::String("/opt/claude/bin/claude".into()));
    }
    env
}

/// Whether a module's runner can apply the exec policy (#471): its settings schema declares an
/// `exec_policy` property — the same declaration [`agent_env`] passes the setting through on.
fn applies_exec_policy(schema: &Value) -> bool {
    schema["properties"].get("exec_policy").is_some()
}

/// The install's `exec_policy` setting, when the operator named one there. Empty and whitespace-only
/// read as unset, like every other string setting.
fn install_exec_policy(install: &crate::config::ModuleChoice) -> Option<String> {
    install
        .settings
        .get("exec_policy")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|policy| !policy.is_empty())
        .map(String::from)
}

/// The exec policy is the operator's rule about commands, not a model setting, so unlike models it
/// follows the colony to an org's module pick: a module that applies the policy inherits the
/// install's policy when its own settings do not name one, and a module that does not apply it
/// refuses to boot rather than silently ignoring a policy the operator set.
pub(crate) fn apply_exec_policy(
    agent: &AgentModule,
    install: &crate::config::ModuleChoice,
    agents: &[AgentModule],
    worktree: &std::path::Path,
    env: &mut Map<String, Value>,
) -> Result<()> {
    if applies_exec_policy(&agent.schema) {
        // The colony's own setting already travelled (agent_env); otherwise the install's policy
        // rides along under this module's env name for the setting.
        let Some(var) = agent.schema["properties"]["exec_policy"]["env"].as_str() else {
            return Ok(());
        };
        if !env.contains_key(var)
            && let Some(policy) = install_exec_policy(install)
        {
            env.insert(var.to_string(), Value::String(policy));
        }
        return Ok(());
    }
    let repo_policy = worktree.join(".colonizer/exec-policy.json").is_file();
    let (source, clear) = match (install_exec_policy(install).is_some(), repo_policy) {
        (false, false) => return Ok(()),
        (true, false) => ("the install's `exec_policy` setting is set", "it"),
        (false, true) => ("the repo's `.colonizer/exec-policy.json` is set", "it"),
        (true, true) => (
            "both the install's `exec_policy` setting and the repo's `.colonizer/exec-policy.json` are set",
            "them",
        ),
    };
    let pick = agents
        .iter()
        .filter(|a| applies_exec_policy(&a.schema))
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let pick = if pick.is_empty() {
        String::new()
    } else {
        format!(" ({pick})")
    };
    let name = &agent.name;
    bail!(
        "the {name} agent module does not apply the exec policy, and {source}: clear {clear}, or pick an agent module that applies it{pick}"
    )
}

/// Whether the agent needs the vendored node runtime mounted: its in-VM command starts with
/// `node`. Takes the resolved [`AgentModule::vm_command`] rather than the module, so the rule is
/// testable without a module directory on disk.
pub(crate) fn agent_needs_node(command: &[String]) -> bool {
    command.first().is_some_and(|arg| arg == "node")
}

pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub(crate) async fn dial_agentd(app: &App, s: &Session) -> Result<Box<dyn Io>> {
    if let Some(ip) = s.mesh.as_ref().and_then(|m| m.ip.clone()) {
        let stream = app.mesh().await?.dial(&ip, AGENTD_PORT).await?;
        return Ok(Box::new(stream));
    }
    if let Some(port) = s.local_port {
        return Ok(Box::new(tokio::net::TcpStream::connect(("127.0.0.1", port)).await?));
    }
    bail!("the microVM's address is not known yet")
}

pub(crate) fn agentd_token(app: &App, id: &str) -> Result<String> {
    read_trimmed(&app.session_dir(id).join("vm/token")).context("session token is missing")
}

pub(crate) async fn agentd_http(app: &App, s: &Session, method: &str, path: &str) -> Result<(u16, String)> {
    let token = agentd_token(app, &s.id)?;
    let request = async {
        let mut stream = dial_agentd(app, s).await?;
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: agentd\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).await?;
        // Framed on Content-Length, not on EOF. The reply is complete and says `Connection: close`,
        // but on macOS microsandbox's published-port forwarder does not pass the guest's FIN along,
        // so waiting for the socket to close waits for the timeout instead.
        let mut response = Vec::new();
        let head_end = loop {
            if let Some(at) = find_headers_end(&response) {
                break at;
            }
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk).await? {
                0 => bail!("agentd closed the connection before sending headers"),
                n => response.extend_from_slice(&chunk[..n]),
            }
        };
        let text = String::from_utf8_lossy(&response[..head_end]).into_owned();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .context("malformed agentd response")?;
        let length = content_length(&text);
        let mut body = response.split_off(head_end);
        match length {
            // No Content-Length: the body is whatever arrives before the peer hangs up.
            None => {
                stream.read_to_end(&mut body).await?;
            }
            Some(want) => {
                while body.len() < want {
                    let mut chunk = [0u8; 4096];
                    match stream.read(&mut chunk).await? {
                        0 => break,
                        n => body.extend_from_slice(&chunk[..n]),
                    }
                }
                body.truncate(want);
            }
        }
        anyhow::Ok((status, String::from_utf8_lossy(&body).into_owned()))
    };
    tokio::time::timeout(Duration::from_secs(10), request)
        .await
        .context("agentd request timed out")?
}

/// The offset just past the blank line that ends the response headers.
fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|at| at + 4)
}

/// `Content-Length` from a response head, if it declares one.
fn content_length(head: &str) -> Option<usize> {
    head.lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        })
        .and_then(|(_, value)| value.trim().parse().ok())
}

pub(crate) async fn agentd_ws(app: &App, s: &Session, path: &str) -> Result<WebSocketStream<Box<dyn Io>>> {
    let token = agentd_token(app, &s.id)?;
    let stream = dial_agentd(app, s).await?;
    let mut request = format!("ws://agentd{path}").into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse()?);
    let (ws, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::client_async(request, stream))
        .await
        .context("agentd websocket handshake timed out")??;
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn module(name: &str, exec_policy: bool) -> AgentModule {
        let mut schema = json!({"properties": {}});
        if exec_policy {
            schema["properties"]["exec_policy"] = json!({"env": "COLONIZER_EXEC_POLICY"});
        }
        AgentModule {
            id: name.to_lowercase().replace(' ', "-"),
            name: name.into(),
            description: String::new(),
            dir: PathBuf::new(),
            entry: vec![],
            needs_claude: false,
            requires: Default::default(),
            schema,
            egress: None,
            resume_dir: None,
            loop_tools: false,
            vendor_secrets: Vec::new(),
        }
    }

    fn install(exec_policy: &str) -> crate::config::ModuleChoice {
        crate::config::ModuleChoice {
            provider: "claude-code".into(),
            enabled: true,
            settings: [("exec_policy".to_string(), Value::String(exec_policy.into()))]
                .into_iter()
                .collect(),
        }
    }

    /// Little local tempdir worktree, so the tests do not need a new dev-dependency. Named and
    /// counter-stamped because cargo runs tests in parallel threads of one process; `Drop` removes
    /// the tree.
    struct Worktree(PathBuf);

    impl Drop for Worktree {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn worktree(with_repo_policy: bool) -> Worktree {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "colonizer-exec-policy-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join(".colonizer")).unwrap();
        if with_repo_policy {
            std::fs::write(dir.join(".colonizer/exec-policy.json"), "{}").unwrap();
        }
        Worktree(dir)
    }

    const INSTALL_POLICY: &str = r#"{"rules": [{"id": "deny-rm", "decision": "deny"}]}"#;

    #[test]
    fn a_colony_on_a_module_that_applies_the_policy_inherits_the_installs() {
        let agents = [module("Claude Code", true), module("ACP", true)];
        let wt = worktree(false);
        let mut env = Map::new();
        apply_exec_policy(&module("ACP", true), &install(INSTALL_POLICY), &agents, &wt.0, &mut env).unwrap();
        assert_eq!(env["COLONIZER_EXEC_POLICY"], INSTALL_POLICY);
    }

    #[test]
    fn a_colonys_own_exec_policy_setting_beats_the_installs() {
        // agent_env has already passed the colony's own setting through; the install's policy must
        // not overwrite it.
        let wt = worktree(false);
        let mut env = Map::new();
        env.insert("COLONIZER_EXEC_POLICY".into(), Value::String(r#"{"rules": []}"#.into()));
        apply_exec_policy(&module("Claude Code", true), &install(INSTALL_POLICY), &[], &wt.0, &mut env).unwrap();
        assert_eq!(env["COLONIZER_EXEC_POLICY"], r#"{"rules": []}"#);
    }

    #[test]
    fn a_module_that_does_not_apply_the_policy_refuses_the_installs_setting() {
        let agents = [module("Claude Code", true), module("ACP", true)];
        let wt = worktree(false);
        let mut env = Map::new();
        let error = apply_exec_policy(&module("Pi", false), &install(INSTALL_POLICY), &agents, &wt.0, &mut env)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "the Pi agent module does not apply the exec policy, and the install's `exec_policy` setting is set: clear it, or pick an agent module that applies it (Claude Code, ACP)"
        );
    }

    #[test]
    fn a_module_that_does_not_apply_the_policy_refuses_the_repos_exec_policy_file() {
        let agents = [module("Claude Code", true), module("ACP", true)];
        let wt = worktree(true);
        let mut env = Map::new();
        let error = apply_exec_policy(&module("Codex", false), &install(""), &agents, &wt.0, &mut env)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "the Codex agent module does not apply the exec policy, and the repo's `.colonizer/exec-policy.json` is set: clear it, or pick an agent module that applies it (Claude Code, ACP)"
        );
    }

    #[test]
    fn a_module_that_does_not_apply_the_policy_refuses_both_sources_at_once() {
        let agents = [module("Claude Code", true), module("ACP", true)];
        let wt = worktree(true);
        let mut env = Map::new();
        let error = apply_exec_policy(
            &module("Grok Build", false),
            &install(INSTALL_POLICY),
            &agents,
            &wt.0,
            &mut env,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "the Grok Build agent module does not apply the exec policy, and both the install's `exec_policy` setting and the repo's `.colonizer/exec-policy.json` are set: clear them, or pick an agent module that applies it (Claude Code, ACP)"
        );
    }

    #[test]
    fn a_module_that_does_not_apply_the_policy_boots_with_no_policy_anywhere() {
        let agents = [module("Claude Code", true), module("ACP", true)];
        let wt = worktree(false);
        let mut env = Map::new();
        // A whitespace-only install setting reads as unset, like every other string setting.
        apply_exec_policy(&module("Pi", false), &install("  "), &agents, &wt.0, &mut env).unwrap();
        assert!(env.is_empty());
    }

    #[test]
    fn exactly_claude_code_and_acp_apply_the_exec_policy() {
        // The rule is the manifest, not the runner on disk: a module applies the exec policy iff its
        // settings schema declares an `exec_policy` property. A new agent module has to take a side
        // — apply the policy, or be named here as one a set policy refuses at boot.
        let agents = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/agents");
        let mut applies = Vec::new();
        let mut refuses = Vec::new();
        for entry in std::fs::read_dir(&agents).unwrap().flatten() {
            let dir = entry.path();
            if !dir.join("module.json").is_file() {
                continue;
            }
            let id = dir.file_name().unwrap().to_string_lossy().into_owned();
            let manifest: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("module.json")).unwrap())
                .unwrap_or_else(|e| panic!("modules/agents/{id}/module.json: {e}"));
            if applies_exec_policy(&manifest["settings"]) {
                applies.push(id);
            } else {
                refuses.push(id);
            }
        }
        applies.sort();
        refuses.sort();
        assert_eq!(applies, ["acp", "claude-code"]);
        assert_eq!(refuses, ["codex", "grok-build", "hermes", "opencode", "pi"]);
    }
}
