//! The source registry (#843): which ledger files the exporter tails, and for which signal. Only
//! the design's source inventory is ever opened; `transcripts/` and `chats/` are not listed, so
//! they cannot be read. Colonies come from the contract's colony list (plus any colony the state
//! still holds a read position for), never from a walk of `sessions/`.

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
/// `meta` is the exporter's own gap records: on whenever any log stream is.
pub fn stream(source: Source) -> &'static str {
    match source {
        Source::Harness | Source::Gateway | Source::Mothership => "operational",
        Source::ExportGap => "meta",
        _ => "activity",
    }
}

/// Whether `source`'s log records are exported under these settings.
pub(crate) fn logs_enabled(settings: &Settings, source: Source) -> bool {
    match stream(source) {
        "operational" => settings.stream_operational,
        "meta" => settings.stream_operational || settings.stream_activity,
        _ => settings.stream_activity,
    }
}

/// The per-colony ledgers, in `sessions/<id>/`.
pub(crate) const COLONY_SOURCES: [(Source, &str); 4] = [
    (Source::Harness, "harness.jsonl"),
    (Source::Events, "events.jsonl"),
    (Source::Gateway, "gateway.jsonl"),
    (Source::Findings, "findings.jsonl"),
];

/// The install-wide ledgers, relative to the data dir, and the rolled predecessor of those that
/// rotate by rename. `logs/mothership.jsonl` is absent until #856 writes it: a missing file is
/// simply no records.
const INSTALL_SOURCES: [(Source, &str, Option<&str>); 7] = [
    (Source::Activity, "activity.jsonl", Some("activity.jsonl.1")),
    (Source::Spend, "spend.jsonl", None),
    (Source::Decisions, "decisions.jsonl", None),
    (Source::Routing, "routing.jsonl", None),
    (Source::JevLadder, "jev_ladder.jsonl", None),
    (Source::JevFocus, "jev_focus.jsonl", None),
    (Source::Mothership, "logs/mothership.jsonl", Some("logs/mothership.jsonl.1")),
];

/// Whether `id` can be a colony id: session ids are short slugs, so anything else (a `..`, a path)
/// is never joined onto `sessions/`.
pub(crate) fn is_colony_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The colony a `sessions/<id>/<file>` cursor path belongs to.
pub(crate) fn colony_of(relative: &str) -> Option<&str> {
    let rest = relative.strip_prefix("sessions/")?;
    let (id, _) = rest.split_once('/')?;
    Some(id)
}

/// Every file to tail under `data_dir` for these colonies, with these settings. A stream that is
/// off is not listed, so it is never read (P1). Metrics read the gateway and spend ledgers on
/// cursors of their own, and traces (#846) read each colony's events and the activity ledger (for
/// the outcomes that close a colony's root span) on theirs.
pub fn discover(data_dir: &Path, settings: &Settings, colonies: &[String]) -> Vec<SourceFile> {
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

    for (source, file, rolled) in INSTALL_SOURCES {
        if logs_enabled(settings, source) {
            add(source, Signal::Logs, None, file.into(), rolled.map(str::to_string));
        }
        if source == Source::Spend && settings.stream_metrics {
            add(source, Signal::Metrics, None, file.into(), None);
        }
        if source == Source::Activity && settings.stream_traces {
            add(source, Signal::Traces, None, file.into(), rolled.map(str::to_string));
        }
    }

    let mut colonies: Vec<&String> = colonies.iter().filter(|id| is_colony_id(id)).collect();
    colonies.sort();
    colonies.dedup();
    for colony in colonies {
        for (source, file) in COLONY_SOURCES {
            let relative = format!("sessions/{colony}/{file}");
            if logs_enabled(settings, source) {
                add(source, Signal::Logs, Some(colony), relative.clone(), None);
            }
            if source == Source::Gateway && settings.stream_metrics {
                add(source, Signal::Metrics, Some(colony), relative.clone(), None);
            }
            if source == Source::Events && settings.stream_traces {
                add(source, Signal::Traces, Some(colony), relative, None);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn every_source_of_the_inventory_is_listed_and_private_dirs_never_are() {
        let root = std::env::temp_dir().join(format!("colonizer-sources-{}", crate::testkit::unique()));
        std::fs::create_dir_all(root.join("sessions/abc123/transcripts")).unwrap();
        std::fs::create_dir_all(root.join("chats")).unwrap();

        let all = discover(&root, &Settings::default(), &ids(&["abc123", "../etc", ".tmp", "abc123"]));
        let rel: Vec<_> = all.iter().map(|s| (s.relative.as_str(), s.signal)).collect();
        for path in [
            "activity.jsonl",
            "spend.jsonl",
            "decisions.jsonl",
            "routing.jsonl",
            "jev_ladder.jsonl",
            "jev_focus.jsonl",
            "logs/mothership.jsonl",
            "sessions/abc123/harness.jsonl",
            "sessions/abc123/events.jsonl",
            "sessions/abc123/gateway.jsonl",
            "sessions/abc123/findings.jsonl",
        ] {
            assert!(rel.contains(&(path, Signal::Logs)), "{path} missing");
        }
        assert!(rel.contains(&("sessions/abc123/gateway.jsonl", Signal::Metrics)));
        assert!(rel.contains(&("spend.jsonl", Signal::Metrics)));
        assert!(rel.contains(&("sessions/abc123/events.jsonl", Signal::Traces)));
        assert!(rel.contains(&("activity.jsonl", Signal::Traces)));
        assert_eq!(
            all.iter().filter(|s| s.relative == "sessions/abc123/harness.jsonl").count(),
            1,
            "a colony listed twice is read once"
        );
        let rolled: Vec<_> = all
            .iter()
            .filter_map(|s| s.rolled.as_ref())
            .map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(rolled, ["activity.jsonl.1", "activity.jsonl.1", "logs/mothership.jsonl.1"]);
        assert!(
            all.iter()
                .all(|s| !s.relative.contains("transcripts") && !s.relative.contains("chats"))
        );
        assert!(all.iter().all(|s| !s.relative.contains("..") && !s.relative.contains(".tmp")));

        let quiet = Settings {
            stream_operational: false,
            stream_activity: false,
            stream_traces: false,
            stream_metrics: false,
            ..Settings::default()
        };
        assert!(
            discover(&root, &quiet, &ids(&["abc123"])).is_empty(),
            "every stream off reads nothing"
        );

        let ops_only = Settings {
            stream_activity: false,
            stream_traces: false,
            stream_metrics: false,
            ..Settings::default()
        };
        let sources: Vec<_> = discover(&root, &ops_only, &ids(&["abc123"]))
            .into_iter()
            .map(|s| s.source)
            .collect();
        assert_eq!(sources, vec![Source::Mothership, Source::Harness, Source::Gateway]);

        let activity_only = Settings {
            stream_operational: false,
            stream_traces: false,
            stream_metrics: false,
            ..Settings::default()
        };
        let sources: Vec<_> = discover(&root, &activity_only, &ids(&["abc123"]))
            .into_iter()
            .map(|s| s.source)
            .collect();
        assert_eq!(
            sources,
            vec![
                Source::Activity,
                Source::Spend,
                Source::Decisions,
                Source::Routing,
                Source::JevLadder,
                Source::JevFocus,
                Source::Events,
                Source::Findings,
            ]
        );

        let traces_only = Settings {
            stream_operational: false,
            stream_activity: false,
            stream_metrics: false,
            ..Settings::default()
        };
        let traced: Vec<_> = discover(&root, &traces_only, &ids(&["abc123"]))
            .into_iter()
            .map(|s| (s.relative, s.signal))
            .collect();
        assert_eq!(
            traced,
            vec![
                ("activity.jsonl".to_string(), Signal::Traces),
                ("sessions/abc123/events.jsonl".to_string(), Signal::Traces),
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn colonies_come_from_the_list_not_from_the_directory() {
        let root = std::env::temp_dir().join(format!("colonizer-sources-list-{}", crate::testkit::unique()));
        std::fs::create_dir_all(root.join("sessions/unlisted1")).unwrap();
        let all = discover(&root, &Settings::default(), &ids(&["listed01"]));
        assert!(all.iter().any(|s| s.colony.as_deref() == Some("listed01")));
        assert!(all.iter().all(|s| s.colony.as_deref() != Some("unlisted1")));
        assert_eq!(colony_of("sessions/listed01/events.jsonl"), Some("listed01"));
        assert_eq!(colony_of("activity.jsonl"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
