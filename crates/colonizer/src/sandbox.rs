//! Sandbox module: microsandbox provider.

use crate::util::{exec, mount_spec};
use anyhow::{Context, Result};
use std::{collections::HashSet, path::PathBuf};
use tokio::process::Command;

pub struct Mount {
    pub source: PathBuf,
    pub target: String,
    pub read_only: bool,
}

pub struct Secret {
    pub env: String,
    pub value: String,
    pub hosts: Vec<String>,
}

pub struct BootSpec {
    pub name: String,
    pub image: String,
    pub cpus: u64,
    pub memory: String,
    pub root_disk: String,
    pub max_duration: String,
    pub workdir: String,
    pub mounts: Vec<Mount>,
    pub env: Vec<(String, String)>,
    pub secrets: Vec<Secret>,
    pub net_profiles: Vec<String>,
    pub net_rules: Vec<String>,
    /// `(host_port, guest_port)` published on 127.0.0.1.
    pub publish: Option<(u16, u16)>,
    pub command: Vec<String>,
}

/// Boots a detached microVM running `spec.command` as its main process.
pub async fn boot(msb: &str, spec: &BootSpec) -> Result<()> {
    let mut cmd = Command::new(msb);
    cmd.args(["run", "--detach", "--replace", "--quiet", "--name", spec.name.as_str()])
        .arg("--cpus")
        .arg(spec.cpus.to_string())
        .args(["--memory", spec.memory.as_str()])
        .args(["--root-disk", spec.root_disk.as_str()])
        .args(["--max-duration", spec.max_duration.as_str()])
        .args(["--workdir", spec.workdir.as_str()]);
    for mount in &spec.mounts {
        cmd.arg("-v").arg(mount_spec(&mount.source, &mount.target, mount.read_only)?);
    }
    for (key, value) in &spec.env {
        cmd.arg("-e").arg(format!("{key}={value}"));
    }
    for secret in &spec.secrets {
        // The value stays in msb's host process; the guest env only holds a placeholder.
        cmd.arg("--secret").arg(format!("{}@{}", secret.env, secret.hosts.join(","))).env(&secret.env, &secret.value);
    }
    if !spec.net_profiles.is_empty() {
        cmd.arg("--net").arg(spec.net_profiles.join(","));
    }
    for rule in &spec.net_rules {
        cmd.arg("--net-rule").arg(rule);
    }
    if let Some((host, guest)) = spec.publish {
        cmd.arg("-p").arg(format!("127.0.0.1:{host}:{guest}"));
    }
    cmd.arg(&spec.image).arg("--").args(&spec.command);
    exec(&mut cmd).await.with_context(|| format!("microVM {} failed to boot", spec.name))?;
    Ok(())
}

pub async fn remove(msb: &str, name: &str) {
    let _ = exec(Command::new(msb).args(["rm", "--force", "--quiet", name])).await;
}

pub async fn running(msb: &str) -> Result<HashSet<String>> {
    let out = exec(Command::new(msb).args(["ls", "--running", "--quiet"])).await?;
    Ok(out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}
