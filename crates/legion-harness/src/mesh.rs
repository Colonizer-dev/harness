//! Private mesh module: supervises the bundled Headscale control server and the harness's own
//! userspace tailscaled, mints single-use keys for microVMs, and dials VMs over SOCKS5.
//!
//! This network is completely separate from any tailnet the host is on: its own control server,
//! its own state directory, its own socket, and `--no-logs-no-support`.

use crate::util::{exec, write_private};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{net::TcpStream, process::{Child, Command}, sync::Mutex};

pub const HARNESS_HOSTNAME: &str = "legion-harness";
const HARNESS_USER: &str = "harness";
const VMS_USER: &str = "vms";

#[derive(Clone, Copy, Debug)]
pub struct Ports {
    pub control: u16,
    pub udp: u16,
    pub socks: u16,
}

pub struct Mesh {
    headscale_bin: PathBuf,
    tailscale_bin: PathBuf,
    tailscaled_bin: PathBuf,
    state_dir: PathBuf,
    runtime_dir: PathBuf,
    ports: Ports,
    running: Mutex<Option<Running>>,
}

struct Running {
    headscale: Child,
    tailscaled: Child,
    vms_user: u64,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: u64,
    pub ip: String,
}

impl Mesh {
    pub fn new(assets: &Path, data_dir: &Path, runtime_dir: &Path, ports: Ports) -> Self {
        Self {
            headscale_bin: assets.join("vendor/headscale"),
            tailscale_bin: assets.join("vendor/tailscale/tailscale"),
            tailscaled_bin: assets.join("vendor/tailscale/tailscaled"),
            state_dir: data_dir.join("mesh"),
            runtime_dir: runtime_dir.to_path_buf(),
            ports,
            running: Mutex::new(None),
        }
    }

    pub fn ports(&self) -> Ports {
        self.ports
    }

    fn headscale_config(&self) -> PathBuf {
        self.state_dir.join("headscale/config.yaml")
    }

    fn headscale_socket(&self) -> PathBuf {
        self.runtime_dir.join("headscale.sock")
    }

    fn tailscaled_socket(&self) -> PathBuf {
        self.runtime_dir.join("tailscaled.sock")
    }

    /// How microVMs reach the control server (microsandbox's `host` network profile).
    pub fn vm_login_server(&self) -> String {
        format!("http://host.microsandbox.internal:{}", self.ports.control)
    }

    fn headscale(&self) -> Command {
        let mut c = Command::new(&self.headscale_bin);
        c.arg("-c").arg(self.headscale_config());
        c
    }

    fn tailscale(&self) -> Command {
        let mut c = Command::new(&self.tailscale_bin);
        c.arg("--socket").arg(self.tailscaled_socket());
        c
    }

    /// Starts (or restarts) headscale and the harness node, and logs the node in if needed.
    pub async fn ensure_started(&self) -> Result<()> {
        let mut running = self.running.lock().await;
        if let Some(r) = running.as_mut() {
            let alive = matches!(r.headscale.try_wait(), Ok(None)) && matches!(r.tailscaled.try_wait(), Ok(None));
            if alive {
                return Ok(());
            }
            *running = None;
        }
        for bin in [&self.headscale_bin, &self.tailscale_bin, &self.tailscaled_bin] {
            if !bin.exists() {
                bail!("mesh binary {} is missing (run scripts/install.sh)", bin.display());
            }
        }
        std::fs::create_dir_all(self.state_dir.join("headscale"))?;
        std::fs::create_dir_all(self.state_dir.join("node"))?;
        create_private_dir(&self.runtime_dir)?;
        for socket in [self.headscale_socket(), self.tailscaled_socket()] {
            if socket.as_os_str().len() >= 100 {
                bail!("socket path {} is too long for a unix socket", socket.display());
            }
        }
        self.write_headscale_files()?;

        let _ = std::fs::remove_file(self.headscale_socket());
        kill_stale(&self.runtime_dir.join("headscale.pid"), &self.headscale_bin);
        kill_stale(&self.runtime_dir.join("tailscaled.pid"), &self.tailscaled_bin);
        let headscale = Command::new(&self.headscale_bin)
            .arg("serve")
            .arg("-c")
            .arg(self.headscale_config())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log_file(&self.state_dir.join("headscale.log"))?)
            .kill_on_drop(true)
            .spawn()
            .context("failed to start headscale")?;
        write_pid(&self.runtime_dir.join("headscale.pid"), headscale.id());
        wait_for(Duration::from_secs(30), || async { exec(self.headscale().args(["users", "list", "-o", "json"])).await.is_ok() })
            .await
            .context("headscale did not become ready (see mesh/headscale.log)")?;
        let harness_user = self.ensure_user(HARNESS_USER).await?;
        let vms_user = self.ensure_user(VMS_USER).await?;

        let _ = std::fs::remove_file(self.tailscaled_socket());
        let node_state = self.state_dir.join("node");
        let tailscaled = Command::new(&self.tailscaled_bin)
            .arg("--tun=userspace-networking")
            .arg("--statedir")
            .arg(&node_state)
            .arg("--socket")
            .arg(self.tailscaled_socket())
            .arg("--port")
            .arg(self.ports.udp.to_string())
            .arg("--socks5-server")
            .arg(format!("127.0.0.1:{}", self.ports.socks))
            .arg("--no-logs-no-support")
            .env("TS_LOGS_DIR", &node_state)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log_file(&self.state_dir.join("tailscaled.log"))?)
            .kill_on_drop(true)
            .spawn()
            .context("failed to start the harness tailscaled")?;
        write_pid(&self.runtime_dir.join("tailscaled.pid"), tailscaled.id());
        wait_for(Duration::from_secs(20), || async { self.backend_state().await.is_ok() })
            .await
            .context("harness tailscaled did not start (see mesh/tailscaled.log)")?;

        if self.backend_state().await? != "Running" {
            let key = self.create_key(harness_user, false).await?;
            let key_file = self.runtime_dir.join("harness-authkey");
            write_private(&key_file, key.as_bytes())?;
            let result = exec(
                self.tailscale()
                    .arg("up")
                    .arg(format!("--login-server=http://127.0.0.1:{}", self.ports.control))
                    .arg(format!("--auth-key=file:{}", key_file.display()))
                    .arg(format!("--hostname={HARNESS_HOSTNAME}"))
                    .args(["--accept-dns=false", "--accept-routes=false", "--timeout=60s"]),
            )
            .await;
            let _ = std::fs::remove_file(&key_file);
            result.context("harness node could not join the mesh")?;
        }
        *running = Some(Running { headscale, tailscaled, vms_user });
        Ok(())
    }

    fn write_headscale_files(&self) -> Result<()> {
        let dir = self.state_dir.join("headscale");
        let q = |p: PathBuf| serde_json::to_string(&p.display().to_string()).unwrap_or_default();
        let control = self.ports.control;
        let config = format!(
            r#"# Generated by legion-harness; edits are overwritten.
server_url: http://127.0.0.1:{control}
listen_addr: 127.0.0.1:{control}
metrics_listen_addr: 127.0.0.1:{metrics}
grpc_listen_addr: 127.0.0.1:{grpc}
grpc_allow_insecure: false
noise:
  private_key_path: {noise}
prefixes:
  v4: 100.64.0.0/10
  v6: fd7a:115c:a1e0::/48
  allocation: sequential
derp:
  server:
    enabled: false
  urls:
    - https://controlplane.tailscale.com/derpmap/default
  paths: []
  auto_update_enabled: true
  update_frequency: 3h
disable_check_updates: true
node:
  expiry: 0
  ephemeral:
    inactivity_timeout: 30m
database:
  type: sqlite
  sqlite:
    path: {db}
    write_ahead_log: true
log:
  level: warn
  format: text
policy:
  mode: file
  path: {policy}
dns:
  magic_dns: true
  base_domain: legion.internal
  override_local_dns: false
  nameservers:
    global: []
  split: {{}}
  search_domains: []
  extra_records: []
unix_socket: {socket}
unix_socket_permission: "0700"
logtail:
  enabled: false
taildrop:
  enabled: false
"#,
            metrics = control.saturating_add(1),
            grpc = control.saturating_add(2),
            noise = q(dir.join("noise_private.key")),
            db = q(dir.join("db.sqlite")),
            policy = q(dir.join("policy.json")),
            socket = q(self.headscale_socket()),
        );
        std::fs::write(self.headscale_config(), config)?;
        // The harness may reach every VM; VMs cannot reach each other or start connections.
        let policy = json!({"acls": [{"action": "accept", "src": [format!("{HARNESS_USER}@")], "dst": [format!("{VMS_USER}@:*")]}]});
        std::fs::write(dir.join("policy.json"), serde_json::to_vec_pretty(&policy)?)?;
        Ok(())
    }

    async fn backend_state(&self) -> Result<String> {
        let out = exec(self.tailscale().args(["status", "--json"])).await?;
        let status: Value = serde_json::from_str(&out)?;
        Ok(status["BackendState"].as_str().unwrap_or_default().to_string())
    }

    async fn ensure_user(&self, name: &str) -> Result<u64> {
        if let Some(id) = self.find_user(name).await? {
            return Ok(id);
        }
        exec(self.headscale().args(["users", "create", name])).await?;
        self.find_user(name).await?.context("headscale user was not created")
    }

    async fn find_user(&self, name: &str) -> Result<Option<u64>> {
        let out = exec(self.headscale().args(["users", "list", "-o", "json"])).await?;
        let users: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
        Ok(users.as_array().into_iter().flatten().find(|u| u["name"] == name).and_then(|u| as_u64(&u["id"])))
    }

    async fn create_key(&self, user: u64, ephemeral: bool) -> Result<String> {
        let mut cmd = self.headscale();
        cmd.args(["preauthkeys", "create", "--user", &user.to_string(), "--expiration", "30m", "-o", "json"]);
        if ephemeral {
            cmd.arg("--ephemeral");
        }
        let out = exec(&mut cmd).await?;
        let key: Value = serde_json::from_str(&out).context("unexpected headscale preauthkeys output")?;
        key["key"].as_str().map(String::from).context("headscale returned no key")
    }

    /// A single-use, ephemeral key for one microVM.
    pub async fn mint_vm_key(&self) -> Result<String> {
        self.ensure_started().await?;
        let user = self.running.lock().await.as_ref().map(|r| r.vms_user).context("mesh is not running")?;
        self.create_key(user, true).await
    }

    async fn nodes(&self) -> Result<Vec<Value>> {
        let out = exec(self.headscale().args(["nodes", "list", "-o", "json"])).await?;
        Ok(serde_json::from_str::<Value>(&out).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default())
    }

    pub async fn wait_online(&self, hostname: &str, timeout: Duration) -> Result<Node> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(node) = self.nodes().await?.into_iter().find(|n| n["given_name"] == hostname && n["online"] == true) {
                let ip = node["ip_addresses"]
                    .as_array()
                    .and_then(|ips| ips.iter().filter_map(Value::as_str).find(|ip| ip.contains('.')))
                    .context("mesh node has no IPv4 address")?
                    .to_string();
                return Ok(Node { id: as_u64(&node["id"]).unwrap_or_default(), ip });
            }
            if tokio::time::Instant::now() > deadline {
                bail!("microVM did not join the mesh within {}s", timeout.as_secs());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub async fn delete_nodes_named(&self, hostname: &str) -> Result<()> {
        if self.running.lock().await.is_none() {
            return Ok(());
        }
        for node in self.nodes().await? {
            if node["given_name"] == hostname {
                if let Some(id) = as_u64(&node["id"]) {
                    let _ = exec(self.headscale().args(["nodes", "delete", "--identifier", &id.to_string(), "--force"])).await;
                }
            }
        }
        Ok(())
    }

    /// Opens a TCP connection to a VM through the harness node's SOCKS5 proxy.
    pub async fn dial(&self, ip: &str, port: u16) -> Result<tokio_socks::tcp::Socks5Stream<TcpStream>> {
        tokio_socks::tcp::Socks5Stream::connect(("127.0.0.1", self.ports.socks), (ip, port))
            .await
            .with_context(|| format!("mesh connection to {ip}:{port} failed"))
    }

    /// Network rules that let a VM send WireGuard UDP straight to the harness node, so traffic
    /// stays on the machine instead of going through a public DERP relay. Only this port is opened.
    pub async fn direct_path_rules(&self) -> Vec<String> {
        let out = exec(Command::new("ip").args(["-4", "-o", "addr", "show", "scope", "global"])).await.unwrap_or_default();
        out.lines()
            .filter_map(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                let iface = *fields.get(1)?;
                let ip = fields.get(3)?.split('/').next()?;
                (!iface.starts_with("tailscale")).then(|| format!("allow@{ip}:udp:{}", self.ports.udp))
            })
            .collect()
    }

    /// Stops headscale and the harness node. VMs keep their state and rejoin on the next start.
    pub async fn shutdown(&self) {
        if let Some(mut running) = self.running.lock().await.take() {
            let _ = running.tailscaled.kill().await;
            let _ = running.headscale.kill().await;
        }
        let _ = std::fs::remove_file(self.runtime_dir.join("headscale.pid"));
        let _ = std::fs::remove_file(self.runtime_dir.join("tailscaled.pid"));
    }

    pub async fn status(&self) -> Value {
        let running = self.running.lock().await.is_some();
        let node = if running { self.backend_state().await.ok() } else { None };
        json!({"running": running, "harness_node": node, "control_port": self.ports.control})
    }
}

fn write_pid(path: &Path, pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = std::fs::write(path, pid.to_string());
    }
}

/// Kills a leftover process from a previous harness run, but only if it is the bundled binary.
fn kill_stale(pid_file: &Path, bin: &Path) {
    let Some(pid) = std::fs::read_to_string(pid_file).ok().and_then(|p| p.trim().parse::<u32>().ok()) else { return };
    let expected = std::fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
    let actual = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    let matches = actual.is_some_and(|exe| {
        let exe = exe.to_string_lossy().trim_end_matches(" (deleted)").to_string();
        Path::new(&exe) == expected
    });
    if matches {
        let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
        for _ in 0..30 {
            if !Path::new(&format!("/proc/{pid}")).exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = std::fs::remove_file(pid_file);
}

fn as_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn log_file(path: &Path) -> Result<std::fs::File> {
    Ok(std::fs::OpenOptions::new().create(true).append(true).open(path)?)
}

async fn wait_for<F, Fut>(timeout: Duration, mut check: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if check().await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    bail!("timed out after {}s", timeout.as_secs())
}
