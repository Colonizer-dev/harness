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

pub const COLONIZER_HOSTNAME: &str = "colonizer";
const COLONIZER_USER: &str = "harness";
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
    /// Bundled DERP relay map, so headscale never fetches one at runtime.
    derp_map: PathBuf,
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

/// Whether the mesh binaries were vendored for this platform. `scripts/fetch-vendor.sh` creates
/// `vendor/tailscale/` whatever happens, so the directory existing proves nothing — only the three
/// binaries do. Every platform the lock supports gets all three now: Tailscale publishes no macOS
/// `tailscaled`, so `scripts/build-tailscaled.sh` builds one from the pinned source. A `--bundle`
/// build, or an install where that build did not run, still lands without them, and colonies are
/// reached on a loopback port instead.
pub fn binaries_present(assets: &Path) -> bool {
    [MESH_HEADSCALE, MESH_TAILSCALE, MESH_TAILSCALED].iter().all(|rel| assets.join(rel).exists())
}

const MESH_HEADSCALE: &str = "vendor/headscale";
const MESH_TAILSCALE: &str = "vendor/tailscale/tailscale";
const MESH_TAILSCALED: &str = "vendor/tailscale/tailscaled";

impl Mesh {
    pub fn new(assets: &Path, data_dir: &Path, runtime_dir: &Path, ports: Ports) -> Self {
        Self {
            headscale_bin: assets.join(MESH_HEADSCALE),
            tailscale_bin: assets.join(MESH_TAILSCALE),
            tailscaled_bin: assets.join(MESH_TAILSCALED),
            derp_map: assets.join("vendor/derpmap.yaml"),
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
        let harness_user = self.ensure_user(COLONIZER_USER).await?;
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
            let key = self.create_key(harness_user).await?;
            let key_file = self.runtime_dir.join("harness-authkey");
            write_private(&key_file, key.as_bytes())?;
            let result = exec(
                self.tailscale()
                    .arg("up")
                    .arg(format!("--login-server=http://127.0.0.1:{}", self.ports.control))
                    .arg(format!("--auth-key=file:{}", key_file.display()))
                    .arg(format!("--hostname={COLONIZER_HOSTNAME}"))
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
        let derp = if self.derp_map.exists() {
            format!("  urls: []\n  paths:\n    - {}\n  auto_update_enabled: false\n  update_frequency: 24h\n", q(self.derp_map.clone()))
        } else {
            // Older app bundles without a DERP map: fall back to fetching Tailscale's public map.
            "  urls:\n    - https://controlplane.tailscale.com/derpmap/default\n  paths: []\n  auto_update_enabled: true\n  update_frequency: 3h\n".to_string()
        };
        let config = format!(
            r#"# Generated by colonizer; edits are overwritten.
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
{derp}disable_check_updates: true
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
  base_domain: colonizer.internal
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
        let policy = json!({"acls": [{"action": "accept", "src": [format!("{COLONIZER_USER}@")], "dst": [format!("{VMS_USER}@:*")]}]});
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

    async fn create_key(&self, user: u64) -> Result<String> {
        let mut cmd = self.headscale();
        cmd.args(["preauthkeys", "create", "--user", &user.to_string(), "--expiration", "30m", "-o", "json"]);
        let out = exec(&mut cmd).await?;
        let key: Value = serde_json::from_str(&out).context("unexpected headscale preauthkeys output")?;
        key["key"].as_str().map(String::from).context("headscale returned no key")
    }

    /// A single-use key for one microVM. Deliberately not ephemeral: headscale deletes an ephemeral node
    /// once it disconnects, so a colony that outlives a mothership restart — or whose microVM is stopped and
    /// later resumed — would find its node gone and its single-use key spent, with no way back onto the mesh.
    /// Colony nodes are deleted explicitly when the colony is torn down.
    pub async fn mint_vm_key(&self) -> Result<String> {
        self.ensure_started().await?;
        let user = self.running.lock().await.as_ref().map(|r| r.vms_user).context("mesh is not running")?;
        self.create_key(user).await
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
        // `ip` is iproute2, which macOS does not ship; there the same addresses come from
        // `ifconfig` in a different shape. Both are tried everywhere rather than split by `cfg`,
        // so the fallback macOS depends on is exercised on Linux, and a box without iproute2
        // still gets rules instead of silently dropping to the DERP relay.
        let ip_out = exec(Command::new("ip").args(["-4", "-o", "addr", "show", "scope", "global"]))
            .await
            .unwrap_or_default();
        let listed = parse_ip_addr_show(&ip_out);
        let listed = if listed.is_empty() {
            let ifconfig_out = exec(&mut Command::new("ifconfig"))
                .await
                .unwrap_or_default();
            parse_ifconfig(&ifconfig_out)
        } else {
            listed
        };
        listed
            .into_iter()
            .map(|ip| format!("allow@{ip}:udp:{}", self.ports.udp))
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
        if self.running.lock().await.is_none() {
            return json!({"enabled": true, "provider": "headscale", "state": "stopped", "harness_ip": null, "nodes": 0});
        }
        let state = self.backend_state().await.ok().map(|s| s.to_lowercase());
        let harness_ip = exec(self.tailscale().args(["ip", "-4"])).await.ok().map(|ip| ip.trim().to_string());
        let colonies = self
            .nodes()
            .await
            .map(|nodes| nodes.iter().filter(|n| n["user"]["name"] == VMS_USER && n["online"] == true).count())
            .unwrap_or(0);
        json!({
            "enabled": true,
            "provider": "headscale",
            "state": state,
            "harness_ip": harness_ip,
            "nodes": colonies,
            "control_port": self.ports.control,
        })
    }
}

/// Whether an address seen on `iface` may become an allow rule. Both parsers share it so the
/// exclusions cannot drift apart between the two tools, and so anything unrecognisable is
/// rejected in one place: a rule microsandbox cannot parse fails the colony boot outright,
/// where no rule at all only falls back to the DERP relay.
fn usable_address(iface: &str, ip: &str) -> bool {
    // Loopback is never where the harness node lives, and a 127.x rule would point colony traffic
    // at the VM's own loopback instead.
    !ip.starts_with("127.")
        // The bundled tailscaled runs userspace networking and creates no interface, so a
        // `tailscale*` interface on Linux belongs to a tailscaled of the user's own, on a
        // tailnet colony traffic has no business being allowed toward.
        && !iface.starts_with("tailscale")
        // Same story on macOS, where the official Tailscale GUI app rides a `utun*` interface.
        && !iface.starts_with("utun")
        // What survives has to be a bare dotted address, so an `ifconfig` or `ip` dialect nobody
        // anticipated degrades to the relay instead of to a rule the VM would reject.
        && ip.parse::<std::net::Ipv4Addr>().is_ok()
}

/// IPv4 addresses listed by `ip -4 -o addr show scope global` (iproute2, Linux). Every line looks
/// like `2: eth0    inet 192.168.1.5/24 brd ... scope global eth0\       valid_lft forever ...`;
/// the lifetime text after the `\` is trailing noise on the same line, so the address is the field
/// after `inet` and the interface the field before it.
fn parse_ip_addr_show(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.get(2) != Some(&"inet") {
                return None;
            }
            let iface = *fields.get(1)?;
            let ip = fields.get(3)?.split('/').next()?;
            usable_address(iface, ip).then(|| ip.to_string())
        })
        .collect()
}

/// IPv4 addresses listed by `ifconfig`, the only address tool macOS ships. An interface header at
/// column 0 (`en0: flags=8863<UP,...> mtu 1500`) owns every indented line below it, so an
/// `inet 192.168.1.5 netmask 0xffffff00 broadcast ...` line is tied to `en0` by position alone;
/// macOS prints the netmask in hex, but only the dotted address matters here. The net-tools and
/// busybox dialects on Linux say `inet addr:192.168.1.5` instead, so that prefix comes off too,
/// and whatever is left that is still not an address is dropped by `usable_address` — no rule at
/// all costs a DERP hop, where a malformed rule costs the boot. `inet6` lines are skipped by
/// matching the token `inet` exactly.
fn parse_ifconfig(out: &str) -> Vec<String> {
    let mut addresses = Vec::new();
    let mut iface = "";
    for line in out.lines() {
        if line.starts_with(char::is_whitespace) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let Some(pos) = fields.iter().position(|f| *f == "inet") else { continue };
            let Some(token) = fields.get(pos + 1).copied() else { continue };
            let ip = token.strip_prefix("addr:").unwrap_or(token);
            if usable_address(iface, ip) {
                addresses.push(ip.to_string());
            }
        } else {
            // Header line; interface names cannot contain `:`.
            iface = line.split(':').next().unwrap_or("");
        }
    }
    addresses
}

fn write_pid(path: &Path, pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = std::fs::write(path, pid.to_string());
    }
}

/// The executable behind a pid: `/proc` where there is one, `ps` otherwise. Not `cfg`-gated on purpose —
/// one code path means the fallback macOS depends on is exercised on Linux too.
fn exe_path(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok().or_else(|| exe_path_via_ps(pid))
}

/// macOS has no `/proc`, and its `ps` reports the executable's full path.
fn exe_path_via_ps(pid: u32) -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "comm="]).output().ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(path))
}

/// Whether a pid is still around. `kill -0` answers that everywhere.
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Kills a leftover process from a previous harness run, but only if it is the bundled binary.
fn kill_stale(pid_file: &Path, bin: &Path) {
    let Some(pid) = std::fs::read_to_string(pid_file).ok().and_then(|p| p.trim().parse::<u32>().ok()) else { return };
    let expected = std::fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
    let matches = exe_path(pid).is_some_and(|exe| {
        let exe = exe.to_string_lossy().trim_end_matches(" (deleted)").to_string();
        Path::new(&exe) == expected
    });
    if matches {
        let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
        for _ in 0..30 {
            if !alive(pid) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `kill_stale` only kills a pid it can prove is the bundled binary, so both of its answers matter:
    /// which executable a pid is, and whether it is still there. macOS gets the `ps` half.
    #[test]
    fn a_pid_can_be_identified_and_checked_without_proc() {
        let me = std::process::id();
        let via_ps = exe_path_via_ps(me).expect("ps knows about this process");
        let name = via_ps.file_name().unwrap_or_default().to_string_lossy().to_string();
        assert!(name.starts_with("colonizer"), "ps reported {via_ps:?}");

        assert!(alive(me));
        // A pid that cannot exist: the kernel's maximum is far below this.
        assert!(!alive(u32::MAX - 1));
    }

    /// Linux's answer, in its real one-line shape: the escaped lifetime tail after the `\`, and a
    /// `tailscale0` address that must never become a rule.
    #[test]
    fn the_ip_listing_yields_lan_addresses_and_skips_tailscale() {
        let out = "2: eth0    inet 192.168.1.5/24 brd 192.168.1.255 scope global eth0\\       valid_lft forever preferred_lft forever\n\
                   5: tailscale0    inet 100.64.12.7/32 scope global tailscale0\\       valid_lft forever preferred_lft forever\n";
        assert_eq!(parse_ip_addr_show(out), vec!["192.168.1.5"]);
    }

    /// macOS has no `ip`, so this parser is exercised from Linux on real `ifconfig` output:
    /// column-0 headers owning tab-indented `inet` lines, hex netmasks, two addresses on one
    /// interface, and the `lo0` and `utun3` addresses that must never become rules.
    #[test]
    fn the_ifconfig_listing_yields_lan_addresses_and_skips_loopback_and_utun() {
        let out = "\
lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384
	inet 127.0.0.1 netmask 0xff000000
	inet6 ::1 prefixlen 128
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	options=6463<TSO4,TSO6,CHANNEL_IO,PARTIAL_CSUM,ZEROINVOKE>
	ether a4:83:e7:00:11:22
	inet6 fe80::10a3:b1ff:fe22:3344%en0 prefixlen 64 secured scopeid 0x6
	inet 192.168.1.5 netmask 0xffffff00 broadcast 192.168.1.255
	inet 192.168.1.6 netmask 0xffffff00 broadcast 192.168.1.255
	media: autoselect
utun3: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1380
	inet 100.100.111.112 --> 100.100.111.113 netmask 0xffffffff
";
        assert_eq!(parse_ifconfig(out), vec!["192.168.1.5", "192.168.1.6"]);
    }

    /// Old net-tools and busybox builds (Alpine, slim containers) put `addr:` in front of the
    /// address where macOS does not; the prefix comes off, and the `lo` entry still never
    /// becomes a rule.
    #[test]
    fn the_legacy_ifconfig_listing_yields_the_address_behind_its_addr_prefix() {
        let out = "\
eth0      Link encap:Ethernet  HWaddr 02:11:22:33:44:55
          inet addr:192.168.1.5  Bcast:192.168.1.255  Mask:255.255.255.0
          inet6 addr: fe80::211:22ff:fe33:4455/64 Scope:Link
          UP BROADCAST RUNNING MULTICAST  MTU:1500  Metric:1
lo        Link encap:Local Loopback
          inet addr:127.0.0.1  Mask:255.0.0.0
          UP LOOPBACK RUNNING  MTU:65536  Metric:1
";
        assert_eq!(parse_ifconfig(out), vec!["192.168.1.5"]);
    }

    /// A token nothing puts where the address belongs must yield no rule at all: one that parses
    /// as junk is exactly what microsandbox rejects, and that fails the whole boot.
    #[test]
    fn a_garbage_token_where_the_address_should_be_yields_no_rule() {
        let out = "\
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	inet 192.168.1.5 netmask 0xffffff00 broadcast 192.168.1.255
en1: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	inet ??? netmask 0xffffff00 broadcast 192.168.1.255
";
        assert_eq!(parse_ifconfig(out), vec!["192.168.1.5"]);
    }
}
