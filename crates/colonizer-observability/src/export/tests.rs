//! The offline export's tests (#859): the flag grammar, the private `0600` files, lines that parse
//! as the OTLP/JSON they claim to be, the window — and the content gate, which opens for an
//! opted-in colony on disk and never for one the archive supplies.

use super::*;
use crate::contract::{ColonyPolicy, Settings};
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::collector::trace::v1::ExportTraceServiceRequest;
use crate::testkit::{Canaries, assert_absent};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn root(tag: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("colonizer-export-{tag}-{}", crate::testkit::unique()));
    std::fs::create_dir_all(&dir).unwrap();
    Root(dir)
}

fn append(path: &Path, line: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn now_ts() -> String {
    chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now()).to_rfc3339()
}

/// Writes the contract over `root`'s data dir and returns its path.
fn contract(root: &Root, colonies: &BTreeMap<String, ColonyPolicy>) -> PathBuf {
    let c = Contract {
        contract: contract::CONTRACT,
        mothership_version: env!("CARGO_PKG_VERSION").into(),
        host_id: "host-1".into(),
        fleet_id: String::new(),
        data_dir: root.0.clone(),
        settings: Settings::default(),
        policy: colonies.clone(),
        relay_members: Vec::new(),
    };
    let path = contract::path(&root.0);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
    path
}

fn colony_policy(content: bool) -> ColonyPolicy {
    ColonyPolicy {
        org: "acme".into(),
        repo: "acme/widgets".into(),
        status: Some("running".into()),
        sensitivity: Some("open".into()),
        content,
        ..ColonyPolicy::default()
    }
}

/// One colony's events line and one finding whose `title` is content tier, plus the install-wide
/// activity outcome that closes the colony's trace root.
fn seed_colony(root: &Root, id: &str, ts: &str, canary: &str) {
    append(
        &root.0.join(format!("sessions/{id}/events.jsonl")),
        &serde_json::json!({"seq": 1, "type": "progress", "ts": ts}),
    );
    append(
        &root.0.join(format!("sessions/{id}/findings.jsonl")),
        &serde_json::json!({"state": "finding", "title": format!("the finding of {canary}")}),
    );
    append(
        &root.0.join("activity.jsonl"),
        &serde_json::json!({"seq": 1, "ts": ts, "kind": "outcome.merged", "actor": "you", "colony": id}),
    );
}

/// A test-time archive bundle for `id`, with the same ledgers `seed_colony` writes on disk. The
/// sidecar names the bundle the way the mothership does: its path under the archive root.
fn seed_archive(root: &Root, id: &str, ts: &str, canary: &str, host: &str) {
    let rel = format!("acme/widgets/2026/09/{id}.tar.zst");
    let bundle = root.0.join("archive").join(&rel);
    std::fs::create_dir_all(bundle.parent().unwrap()).unwrap();
    let events = format!("{{\"seq\": 1, \"type\": \"progress\", \"ts\": \"{ts}\"}}\n");
    let findings = format!("{{\"state\": \"finding\", \"title\": \"the finding of {canary}\"}}\n");
    let file = std::fs::File::create(&bundle).unwrap();
    let mut zstd = zstd::Encoder::new(file, 3).unwrap();
    {
        let mut tar = tar::Builder::new(&mut zstd);
        for (name, body) in [("events.jsonl", events.as_str()), ("findings.jsonl", findings.as_str())] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            tar.append_data(&mut header, name, body.as_bytes()).unwrap();
        }
        tar.finish().unwrap();
    }
    zstd.finish().unwrap().sync_all().unwrap();
    let sidecar = serde_json::json!({
        "session": id,
        "repo": "acme/widgets",
        "org": "acme",
        "status": "running",
        "agent": "claude_code",
        "created_at": "2026-09-24T09:30:00Z",
        "mothership": host,
        "revision": 1,
        "bundle": rel,
    });
    std::fs::write(bundle.with_file_name(format!("{id}.json")), sidecar.to_string()).unwrap();
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

fn mode_is_private(path: &Path) {
    let mode = std::fs::metadata(path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{} is {:o}", path.display(), mode);
}

fn flag(args: &[&str]) -> Result<Options, String> {
    Options::from_args(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
}

fn nanos(when: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(when)
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap() as u64
}

#[test]
fn the_flags_parse() {
    let opts = flag(&[
        "--out",
        "/tmp/out",
        "--since",
        "2026-09-24T09:30:00Z",
        "--include-archive",
        "--content",
    ])
    .unwrap();
    assert_eq!(
        opts,
        Options {
            since: Some(nanos("2026-09-24T09:30:00Z")),
            out: PathBuf::from("/tmp/out"),
            include_archive: true,
            content: true,
            ..Options::default()
        }
    );
    assert!(flag(&[]).is_err(), "--out is required");
    assert!(flag(&["--out", "x", "--signals", "metrics"]).is_err());
    assert!(flag(&["--out", "x", "--signals", ","]).is_err());
    assert!(flag(&["--out", "x", "--signals", "logs"]).is_ok());
    assert!(flag(&["--out", "x", "--since", "not a date"]).is_err());
    assert!(flag(&["--out", "x", "--when"]).is_err());
}

/// Test (c): a directory `--out` writes `logs.jsonl` and `traces.jsonl`, each line one OTLP/JSON
/// request that parses back into the request it claims to be; the files are `0600`; and the content
/// gate stays shut for an org that never opted in, `--content` or not.
#[test]
fn a_directory_export_writes_otlp_json_lines_the_collector_would_accept() {
    let root = root("directory");
    let canaries = Canaries::new();
    let ts = now_ts();
    seed_colony(&root, "c0ffee12", &ts, &canaries.content);
    let path = contract(&root, &BTreeMap::from([("c0ffee12".to_string(), colony_policy(false))]));

    let out = root.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let summary = run(
        &path,
        &Options {
            out: out.clone(),
            content: true,
            ..Options::default()
        },
    )
    .unwrap();

    assert_eq!(
        summary["files"],
        serde_json::json!([
            out.join("logs.jsonl").display().to_string(),
            out.join("traces.jsonl").display().to_string()
        ])
    );
    mode_is_private(&out.join("logs.jsonl"));
    mode_is_private(&out.join("traces.jsonl"));

    // The findings record and the events record are logs; the outcome closes the trace root.
    assert_eq!(summary["logs"], serde_json::json!(3));
    assert_eq!(summary["spans"].as_u64().unwrap(), 1);
    assert_eq!(summary["colonies"], serde_json::json!(1));
    assert_eq!(summary["archived"], serde_json::json!(0));
    assert_eq!(summary["skipped"]["malformed"], serde_json::json!(0));
    assert_eq!(summary["requests"].as_u64().unwrap(), 2);

    let logs = read_lines(&out.join("logs.jsonl"));
    assert_eq!(logs.len(), 1, "one request, one line");
    for line in &logs {
        serde_json::from_str::<ExportLogsServiceRequest>(line)
            .unwrap_or_else(|e| panic!("a logs line is not an OTLP/JSON request: {e}: {line}"));
    }
    for line in read_lines(&out.join("traces.jsonl")) {
        serde_json::from_str::<ExportTraceServiceRequest>(&line)
            .unwrap_or_else(|e| panic!("a traces line is not an OTLP/JSON request: {e}: {line}"));
    }

    // The org never opted in, so the finding's title — content tier — never leaves the machine.
    let bytes = std::fs::read(out.join("logs.jsonl")).unwrap();
    assert_absent(
        &bytes,
        [canaries.content.as_str()],
        "the content of a colony that never opted in",
    );
}

/// Test (b): `--content` with an org that opted in opens the gate for the colony on disk, and never
/// for the same-org colony the archive supplies — its synthesized policy fails every leg of P3, so
/// its findings travel as structure only. Two canaries, one per colony, tell the two apart.
#[test]
fn content_opens_for_an_opted_in_disk_colony_and_never_for_an_archived_one() {
    let root = root("content");
    let (disk_canary, archive_canary) = (Canaries::new().content, Canaries::new().content);
    let ts = now_ts();
    seed_colony(&root, "c0ffee12", &ts, &disk_canary);
    seed_archive(&root, "d4c3b2a1", &ts, &archive_canary, "host-1");
    let colonies = BTreeMap::from([
        ("c0ffee12".to_string(), colony_policy(true)),
        ("d4c3b2a1".to_string(), colony_policy(true)),
    ]);
    let path = contract(&root, &colonies);

    let out = root.0.join("one.jsonl");
    let summary = run(
        &path,
        &Options {
            out: out.clone(),
            include_archive: true,
            content: true,
            ..Options::default()
        },
    )
    .unwrap();

    // One file for both signals, private.
    assert_eq!(summary["files"], serde_json::json!([out.display().to_string()]));
    mode_is_private(&out);
    assert_eq!(summary["archived"], serde_json::json!(1));
    assert_eq!(summary["colonies"], serde_json::json!(2));

    // Both colonies' structure came through — the disk colony's events, finding and outcome line,
    // and the archived colony's events and finding (a bundle holds no install-wide activity) —
    // with only the disk colony's root carrying an outcome, so exactly one span.
    assert_eq!(summary["logs"], serde_json::json!(5));
    assert_eq!(summary["spans"].as_u64().unwrap(), 1);
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains(&disk_canary), "the opted-in disk colony's content is exported");
    // Line by line — one request each — so the check reads each request's every field.
    for line in read_lines(&out) {
        assert_absent(
            line.as_bytes(),
            [archive_canary.as_str()],
            "the content of a colony only the archive supplies",
        );
    }
    // The archived finding still arrived as structure: its event name and state.
    assert!(text.contains("colonizer.finding"));
}

/// The window bounds the export the backfill's own grammar bounds it: `--until` the line's own `ts`
/// is exclusive, and a line with no `ts` cannot be placed in a window at all.
#[test]
fn a_window_bounds_the_export() {
    let root = root("window");
    let ts = "2026-09-24T09:31:10Z";
    seed_colony(&root, "c0ffee12", ts, "canary-not-content");
    let path = contract(&root, &BTreeMap::from([("c0ffee12".to_string(), colony_policy(false))]));
    let out = root.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let summary = run(
        &path,
        &Options {
            out: out.clone(),
            until: Some(nanos(ts)),
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(summary["logs"], serde_json::json!(0));
    assert_eq!(summary["spans"], serde_json::json!(0));
    assert_eq!(read_lines(&out.join("logs.jsonl")), Vec::<String>::new());
}
