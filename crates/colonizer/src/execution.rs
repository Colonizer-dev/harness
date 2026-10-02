//! Execution seam: the control/execution plane boundary.
//!
//! The control plane (queue, credentials, git, publish) stays on the mothership.
//! The execution plane runs colonies: it boots and removes microVMs and reports
//! which ones are running. This module names that boundary as a trait so a
//! future remote outpost can host colonies without the control plane changing.
//!
//! The colony path goes through the trait, via the `execution` field on
//! [`crate::app::App`]: `boot.rs` pulls the image and boots the microVM,
//! `lifecycle::teardown_vm` and reclaim's auto-reclaim remove it, and the
//! liveness checks (the minute-tick watchdog, the restart sweep, warm resume)
//! ask it what is running. [`LocalBackend`] is the only backend; it delegates
//! to [`crate::sandbox`] with no behavior change.
//!
//! Some calls stay on [`crate::sandbox`] free functions on purpose. The image
//! cache is not a trait method: `boot.rs` checks `sandbox::is_cached` before a
//! launch because it describes the local image cache, not an execution
//! primitive, and the Setup pane's pre-pull keeps calling the sandbox
//! functions directly. The reclaim sweep's orphan pass stays there too: it
//! enumerates every sandbox with `sandbox::all` — not a trait method — and
//! removes the ones no session owns with `sandbox::running` and
//! `sandbox::remove`.

use std::{collections::HashSet, future::Future, pin::Pin};

/// Identity of one execution node: `"local"` today, a mesh node name later.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

/// What an execution node offers. The scheduler matches colonies to nodes on
/// these; today the only node is local and always qualifies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Whether the node can boot KVM microVMs.
    pub kvm: bool,
    /// Placement hints a future scheduler can match on (e.g. `"gpu"`).
    /// Empty on the local backend: it takes everything.
    pub labels: Vec<String>,
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Runs colonies on some machine. Mirrors the [`crate::sandbox`] free
/// functions on purpose: `boot` takes `&BootSpec`, `remove` takes `&str`,
/// `running` returns the running names, and `pull` fetches an image.
/// Boxed futures instead of `async fn` keep the trait object-safe so the
/// harness can hold an `Arc<dyn ExecutionBackend>` (as `App.execution` does)
/// and dispatch without knowing which backend it has.
pub trait ExecutionBackend: Send + Sync {
    /// Boot a detached microVM running `spec.command` as its main process.
    fn boot<'a>(&'a self, spec: &'a crate::sandbox::BootSpec) -> BoxFuture<'a, anyhow::Result<()>>;
    /// Remove a microVM and confirm it is gone: `Ok` only once the node's own listing no longer
    /// names it (a listed-but-stopped microVM is not gone), `Err` when the state is unknown — a
    /// caller that must not touch the worktree while the microVM may still be there refuses.
    /// See [`crate::sandbox::remove_confirmed`]; the best-effort [`crate::sandbox::remove`] stays
    /// for the callers that only tidy up.
    fn remove<'a>(&'a self, name: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;
    /// Names of the currently running microVMs.
    fn running<'a>(&'a self) -> BoxFuture<'a, anyhow::Result<HashSet<String>>>;
    /// Download an image into the node's cache.
    fn pull<'a>(&'a self, image: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;
    /// Which node this backend runs on.
    fn node_id(&self) -> NodeId;
    /// What this node offers for placement.
    fn capabilities(&self) -> Capabilities;
}

/// Execution on this machine, via microsandbox. SHIPPING.
pub struct LocalBackend {
    /// Path to the `msb` binary, mirroring `app.cfg.msb` at each call site.
    msb: String,
}

impl LocalBackend {
    /// `msb` is the microsandbox binary path (today `app.cfg.msb`).
    pub fn new(msb: impl Into<String>) -> Self {
        Self { msb: msb.into() }
    }
}

impl ExecutionBackend for LocalBackend {
    fn boot<'a>(&'a self, spec: &'a crate::sandbox::BootSpec) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(crate::sandbox::boot(&self.msb, spec))
    }

    fn remove<'a>(&'a self, name: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(crate::sandbox::remove_confirmed(
            &self.msb,
            name,
            crate::sandbox::REMOVE_CONFIRM_TIMEOUT,
        ))
    }

    fn running<'a>(&'a self) -> BoxFuture<'a, anyhow::Result<HashSet<String>>> {
        Box::pin(crate::sandbox::running(&self.msb))
    }

    fn pull<'a>(&'a self, image: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(crate::sandbox::pull(&self.msb, image))
    }

    fn node_id(&self) -> NodeId {
        NodeId("local".to_string())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            kvm: true,
            labels: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn the_backend_trait_is_object_safe() {
        // The harness holds an `Arc<dyn ExecutionBackend>` (`App.execution`) so
        // dispatch does not depend on which backend is behind it. This fails
        // to compile if the trait ever stops being object-safe.
        let backend: Arc<dyn ExecutionBackend> = Arc::new(LocalBackend::new("msb"));
        assert_eq!(backend.node_id(), NodeId("local".to_string()));
    }

    #[test]
    fn local_backend_identity_and_capabilities() {
        let backend = LocalBackend::new("/usr/local/bin/msb");
        assert_eq!(backend.node_id(), NodeId("local".to_string()));
        let caps = backend.capabilities();
        assert!(caps.kvm);
        assert!(caps.labels.is_empty());
    }

    /// In-memory backend: proves dispatch through the dyn trait reaches the
    /// right implementation without touching KVM.
    #[derive(Default)]
    struct FakeBackend {
        running: Mutex<HashSet<String>>,
    }

    impl ExecutionBackend for FakeBackend {
        fn boot<'a>(&'a self, spec: &'a crate::sandbox::BootSpec) -> BoxFuture<'a, anyhow::Result<()>> {
            let name = spec.name.clone();
            Box::pin(async move {
                self.running.lock().unwrap().insert(name);
                Ok(())
            })
        }

        fn remove<'a>(&'a self, name: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
            let name = name.to_string();
            Box::pin(async move {
                self.running.lock().unwrap().remove(&name);
                Ok(())
            })
        }

        fn running<'a>(&'a self) -> BoxFuture<'a, anyhow::Result<HashSet<String>>> {
            Box::pin(async move { Ok(self.running.lock().unwrap().clone()) })
        }

        fn pull<'a>(&'a self, _image: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move { Ok(()) })
        }

        fn node_id(&self) -> NodeId {
            NodeId("fake".to_string())
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                kvm: false,
                labels: vec!["fake".to_string()],
            }
        }
    }

    fn boot_spec(name: &str) -> crate::sandbox::BootSpec {
        crate::sandbox::BootSpec {
            name: name.to_string(),
            image: "test:latest".to_string(),
            cpus: 1,
            memory: "1g".to_string(),
            root_disk: "8g".to_string(),
            max_duration: "1h".to_string(),
            workdir: "/workspace".to_string(),
            mounts: Vec::new(),
            env: Vec::new(),
            secrets: Vec::new(),
            net_profiles: Vec::new(),
            net_rules: Vec::new(),
            net_deny_egress: false,
            publish: None,
            command: vec!["true".to_string()],
        }
    }

    #[tokio::test]
    async fn dyn_dispatch_reaches_the_backend_behind_the_trait() {
        let backend: Box<dyn ExecutionBackend> = Box::new(FakeBackend::default());
        assert_eq!(backend.node_id(), NodeId("fake".to_string()));

        backend.boot(&boot_spec("colony-a")).await.unwrap();
        backend.boot(&boot_spec("colony-b")).await.unwrap();
        let running = backend.running().await.unwrap();
        assert!(running.contains("colony-a") && running.contains("colony-b"));

        backend.remove("colony-a").await.unwrap();
        let running = backend.running().await.unwrap();
        assert!(!running.contains("colony-a"));
        assert!(running.contains("colony-b"));

        backend.pull("test:latest").await.unwrap();
    }
}
