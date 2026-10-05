//! The source registry (#843, first slice): which ledger files the exporter tails, and for which
//! signal. Only the design's source inventory is ever opened; `transcripts/` and `chats/` are not
//! listed, so they cannot be read.

use crate::contract::Settings;
use crate::policy::Source;
use crate::state::Signal;
use std::path::{Path, PathBuf};

/// One file to tail for one signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    pub source: Source,
    pub signal: Signal,
    /// The colony a per-colony file belongs to; `None` for an install-wide file.
    pub colony: Option<String>,
    pub live: PathBuf,
    /// The single rolled-over predecessor, for a source that rotates by rename.
    pub rolled: Option<PathBuf>,
    /// The path relative to the data dir: the cursor's key.
    pub relative: String,
}

/// The stream a source's log records belong to (`colonizer.stream`), and the switch that gates it.
pub fn stream(source: Source) -> &'static str {
    match source {
        Source::Harness | Source::Gateway | Source::Mothership => "operational",
        _ => "activity",
    }
}

fn logs_enabled(settings: &Settings, source: Source) -> bool {
    match stream(source) {
        "operational" => settings.stream_operational,
        _ => settings.stream_activity,
    }
}

/// The per-colony ledgers this slice maps, in `sessions/<id>/`.
const COLONY_SOURCES: [(Source, &str); 3] = [
    (Source::Harness, "harness.jsonl"),
    (Source::Events, "events.jsonl"),
    (Source::Gateway, "gateway.jsonl"),
];

/// Every file to tail under `data_dir` with these settings. A stream that is off is not listed, so
/// it is never read (P1). Metrics read the gateway and spend ledgers on cursors of their own.
pub fn discover(data_dir: &Path, settings: &Settings) -> Vec<SourceFile> {
    let mut out = Vec::new();
    let mut add = |source: Source, signal: Signal, colony: Option<&str>, relative: String, rolled: Option<String>| {
        out.push(SourceFile {
            source,
            signal,
            colony: colony.map(str::to_string),
            live: data_dir.join(&relative),
            rolled: rolled.map(|r| data_dir.join(r)),
            relative,
        });
    };

    if logs_enabled(settings, Source::Activity) {
        add(
            Source::Activity,
            Signal::Logs,
            None,
            "activity.jsonl".into(),
            Some("activity.jsonl.1".into()),
        );
    }
    if logs_enabled(settings, Source::Spend) {
        add(Source::Spend, Signal::Logs, None, "spend.jsonl".into(), None);
    }
    if settings.stream_metrics {
        add(Source::Spend, Signal::Metrics, None, "spend.jsonl".into(), None);
    }

    let mut colonies: Vec<String> = std::fs::read_dir(data_dir.join("sessions"))
        .map(|dir| {
            dir.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|e| e.file_name().into_string().ok())
                // Session ids are short slugs; anything else in the directory is not a colony.
                .filter(|id| !id.starts_with('.') && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
                .collect()
        })
        .unwrap_or_default();
    colonies.sort();
    for colony in &colonies {
        for (source, file) in COLONY_SOURCES {
            let relative = format!("sessions/{colony}/{file}");
            if logs_enabled(settings, source) {
                add(source, Signal::Logs, Some(colony), relative.clone(), None);
            }
            if source == Source::Gateway && settings.stream_metrics {
                add(source, Signal::Metrics, Some(colony), relative, None);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_that_is_off_is_never_listed_and_private_dirs_never_are() {
        let root = std::env::temp_dir().join(format!("colonizer-sources-{}", crate::testkit::unique()));
        std::fs::create_dir_all(root.join("sessions/abc123/transcripts")).unwrap();
        std::fs::create_dir_all(root.join("chats")).unwrap();
        std::fs::create_dir_all(root.join("sessions/.tmp")).unwrap();

        let all = discover(&root, &Settings::default());
        let rel: Vec<_> = all.iter().map(|s| (s.relative.as_str(), s.signal)).collect();
        assert!(rel.contains(&("activity.jsonl", Signal::Logs)));
        assert!(rel.contains(&("sessions/abc123/harness.jsonl", Signal::Logs)));
        assert!(rel.contains(&("sessions/abc123/gateway.jsonl", Signal::Metrics)));
        assert!(rel.contains(&("spend.jsonl", Signal::Metrics)));
        assert!(
            all.iter()
                .all(|s| !s.relative.contains("transcripts") && !s.relative.contains("chats"))
        );
        assert!(all.iter().all(|s| !s.relative.contains(".tmp")));

        let quiet = Settings {
            stream_operational: false,
            stream_activity: false,
            stream_metrics: false,
            ..Settings::default()
        };
        assert!(discover(&root, &quiet).is_empty(), "every stream off reads nothing");

        let ops_only = Settings {
            stream_activity: false,
            stream_metrics: false,
            ..Settings::default()
        };
        let sources: Vec<_> = discover(&root, &ops_only).into_iter().map(|s| s.source).collect();
        assert_eq!(sources, vec![Source::Harness, Source::Gateway]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
