//! The disk guard (#850): the `file` provider writes under the mothership's own data dir, so
//! nothing stops it running the disk dry the way an unbounded log could. Every reconcile tick
//! checks the same free-space verdict the queue's admission pause already computes, and the same
//! sticky write alert the cockpit already shows, and leaves a marker for the add-on to see before
//! its next write: `<data>/observability/disk-low`. The add-on checks the marker's presence, not
//! its contents — this file's shape is free to change.

use crate::app::Shared;
use std::path::PathBuf;

/// `<data>/observability/disk-low`.
fn marker_path(app: &Shared) -> PathBuf {
    app.cfg.data_dir.join("observability").join("disk-low")
}

/// Runs once per reconcile tick (every `RECONCILE`, from `start_tasks`): writes the marker while
/// disk space is low or admission is paused, or a storage alert is shown, and removes it
/// otherwise. Never touches the sink's own files — only the one marker.
pub(super) async fn sync(app: &Shared) {
    let verdict = app.disk_verdict.lock().await.clone();
    let alert = app.shown_storage_alert().await;
    let reason = if verdict.low_disk || verdict.admission_paused {
        Some("disk")
    } else if alert.is_some() {
        Some("storage")
    } else {
        None
    };
    let path = marker_path(app);
    match reason {
        Some(reason) => {
            // Already there: leave it alone rather than rewriting it every five seconds — its
            // `at_unix` is when the hold started, not a liveness heartbeat.
            if tokio::fs::metadata(&path).await.is_ok() {
                return;
            }
            let body = serde_json::json!({"reason": reason, "at_unix": chrono::Utc::now().timestamp()});
            if let Some(dir) = path.parent()
                && let Err(error) = tokio::fs::create_dir_all(dir).await
            {
                tracing::warn!(path = %dir.display(), %error, "could not create the observability dir for the disk-low marker");
                return;
            }
            match crate::util::write_atomic(&path, body.to_string().as_bytes()).await {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
                            tracing::warn!(path = %path.display(), %error, "could not set the disk-low marker's permissions");
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "could not write the disk-low marker");
                }
            }
        }
        None => {
            if let Err(error) = tokio::fs::remove_file(&path).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %path.display(), %error, "could not remove the disk-low marker");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{StorageAlert, StorageAlertKind};
    use crate::reclaim::FreeSpaceVerdict;

    fn marker(app: &Shared) -> PathBuf {
        marker_path(app)
    }

    #[cfg(unix)]
    fn mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn low_disk_writes_a_private_marker_naming_the_reason() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        *app.disk_verdict.lock().await = FreeSpaceVerdict {
            low_disk: true,
            ..Default::default()
        };

        sync(&app).await;

        let path = marker(&app);
        let body: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(body["reason"], "disk");
        assert!(body["at_unix"].as_i64().is_some());
        #[cfg(unix)]
        assert_eq!(mode(&path), 0o600, "the marker is private");
    }

    #[tokio::test]
    async fn clearing_disk_space_removes_the_marker() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        *app.disk_verdict.lock().await = FreeSpaceVerdict {
            admission_paused: true,
            low_disk: true,
            ..Default::default()
        };
        sync(&app).await;
        assert!(marker(&app).try_exists().unwrap());

        *app.disk_verdict.lock().await = FreeSpaceVerdict::default();
        sync(&app).await;

        assert!(
            !marker(&app).try_exists().unwrap(),
            "the marker is gone once the disk recovers"
        );
    }

    #[tokio::test]
    async fn a_shown_storage_alert_also_holds_the_sink_with_its_own_reason() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        *app.storage_alert.write().await = Some(StorageAlert {
            kind: StorageAlertKind::Write,
            message: "disk full".into(),
            ts: chrono::Utc::now(),
            failures: 1,
            recovered_at: None,
        });

        sync(&app).await;

        let body: serde_json::Value = serde_json::from_slice(&std::fs::read(marker(&app)).unwrap()).unwrap();
        assert_eq!(body["reason"], "storage");
    }

    #[tokio::test]
    async fn an_existing_marker_is_left_as_is_while_still_low() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        *app.disk_verdict.lock().await = FreeSpaceVerdict {
            low_disk: true,
            ..Default::default()
        };
        sync(&app).await;
        let first = std::fs::read(marker(&app)).unwrap();

        // Still low, ticks later: the marker is not rewritten, so its `at_unix` keeps the first
        // tick's time rather than resetting on every reconcile.
        sync(&app).await;
        sync(&app).await;

        assert_eq!(std::fs::read(marker(&app)).unwrap(), first);
    }
}
