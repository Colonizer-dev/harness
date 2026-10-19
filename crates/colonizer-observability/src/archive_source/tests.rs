//! The archive source's tests (#859): discovery keeps the newest revision and refuses what cannot
//! be a colony, the reader yields only the four ledgers and never a credential, and a half-gigabyte
//! bundle streams through it in bounded memory.

use super::*;
use crate::contract::ColonyPolicy;

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn root(tag: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("colonizer-archive-{tag}-{}", crate::testkit::unique()));
    std::fs::create_dir_all(&dir).unwrap();
    Root(dir)
}

/// Writes `entries` as one tar+zstd bundle at `path`, entry names session-dir-relative the way the
/// mothership's `archive.rs` writes them.
fn write_bundle(path: &Path, entries: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut zstd = zstd::Encoder::new(file, 3).unwrap();
    {
        let mut tar = tar::Builder::new(&mut zstd);
        for (name, body) in entries {
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
}

/// The `.json` sidecar next to a bundle, as the mothership writes it.
fn write_sidecar(dir: &Path, session: &str, mothership: &str, revision: u32, bundle: &str) {
    let record = serde_json::json!({
        "session": session,
        "repo": "acme/widgets",
        "org": "acme",
        "status": "running",
        "agent": "claude_code",
        "created_at": "2026-09-24T09:30:00Z",
        "mothership": mothership,
        "revision": revision,
        "bundle": bundle,
    });
    std::fs::write(dir.join(format!("{session}.json")), record.to_string()).unwrap();
}

fn one_bundle(root: &Root, name: &str) -> PathBuf {
    let rel = format!("acme/widgets/2026/09/{name}");
    let path = root.0.join("archive").join(&rel);
    write_bundle(
        &path,
        &[
            (
                "events.jsonl",
                "{\"seq\":1,\"type\":\"progress\",\"ts\":\"2026-09-24T09:31:10Z\"}\n",
            ),
            ("findings.jsonl", "{\"state\":\"finding\",\"title\":\"t\"}\n"),
        ],
    );
    write_sidecar(&root.0.join("archive/acme/widgets/2026/09"), "a1b2c3d4", "host-1", 1, &rel);
    path
}

/// Yields every source and line of a bundle, in order, as `(source, line)` pairs.
fn read_all(bundle: &Path, malformed: &mut u64) -> Vec<(Source, Value)> {
    let mut out = Vec::new();
    read_bundle(
        bundle,
        &mut |line| {
            out.push((line.source, line.line));
            true
        },
        malformed,
    )
    .unwrap();
    out
}

#[test]
fn discovery_keeps_the_newest_revision_and_skips_what_it_must() {
    let root = root("discover");
    let dir = root.0.join("archive/acme/widgets/2026/09");
    // Two revisions of one colony: the newer sidecar wins however they were named on disk.
    write_bundle(&dir.join("a1b2c3d4.r1.tar.zst"), &[("events.jsonl", "{\"seq\":1}\n")]);
    write_sidecar(&dir, "a1b2c3d4", "host-1", 1, "acme/widgets/2026/09/a1b2c3d4.r1.tar.zst");
    write_bundle(&dir.join("a1b2c3d4.r2.tar.zst"), &[("events.jsonl", "{\"seq\":1}\n")]);
    write_sidecar(&dir, "a1b2c3d4", "host-1", 2, "acme/widgets/2026/09/a1b2c3d4.r2.tar.zst");
    // A colony the disk still holds, an id that cannot be one, and a bundle path that is not a
    // plain relative path: none of them may be opened.
    write_sidecar(&dir, "e5f60718", "host-1", 1, "acme/widgets/2026/09/e5f60718.tar.zst");
    write_sidecar(&dir, "not!!an!!id", "host-1", 1, "acme/widgets/2026/09/x.tar.zst");
    write_sidecar(&dir, "d4c3b2a1", "host-1", 1, "../../secrets/session.json");

    let found = discover(&root.0, None, &BTreeSet::from(["e5f60718".to_string()]));
    assert_eq!(found.keys().map(|id| id.as_str()).collect::<Vec<_>>(), ["a1b2c3d4"]);
    let kept = &found["a1b2c3d4"];
    assert_eq!(kept.host, "host-1");
    assert_eq!(kept.bundle, dir.join("a1b2c3d4.r2.tar.zst"));
    // The synthesized policy is fail-closed and carries the sidecar's identity (P3).
    assert_eq!(kept.policy.org, "acme");
    assert_eq!(kept.policy.repo, "acme/widgets");
    assert_eq!(kept.policy.sensitivity, None);
    assert!(!kept.policy.content);
    assert_eq!(kept.policy.status.as_deref(), Some("running"));

    // `--repo` names another repository: nothing matches. Case-insensitive, and without the skip
    // set the on-disk colony is discoverable too.
    assert!(discover(&root.0, Some("acme/other"), &BTreeSet::new()).is_empty());
    assert_eq!(discover(&root.0, Some("ACME/Widgets"), &BTreeSet::new()).len(), 2);
}

#[test]
fn a_line_out_of_a_bundle_carries_the_disk_readers_identity() {
    let root = root("identity");
    let path = one_bundle(&root, "a1b2c3d4.tar.zst");
    let mut malformed = 0;
    let lines = read_all(&path, &mut malformed);
    assert_eq!(malformed, 0);
    assert_eq!(lines.len(), 2);
    // The digest is the SHA-256 of the raw line bytes newline-excluded, the size len + 1: what
    // `cursor.rs` feeds the mapper, so a record id built here matches one built there.
    let mut digests = Vec::new();
    let mut sizes = Vec::new();
    read_bundle(
        &path,
        &mut |line| {
            digests.push(line.digest);
            sizes.push(line.size);
            true
        },
        &mut malformed,
    )
    .unwrap();
    let raw = b"{\"seq\":1,\"type\":\"progress\",\"ts\":\"2026-09-24T09:31:10Z\"}";
    let expect = ring::digest::digest(&ring::digest::SHA256, raw);
    assert_eq!(digests[0].as_slice(), expect.as_ref());
    assert_eq!(sizes[0], raw.len() as u64 + 1);
}

/// Test (d): a bundle holds more than its four ledgers, and some of what it holds is a credential.
/// Only the ledgers are read; a credential is skipped on its entry's name, before a byte of it is
/// unpacked.
#[test]
fn only_the_four_ledgers_are_read_and_credentials_never() {
    let root = root("credentials");
    let canary = "colonizer-vm-credential-canary";
    let path = root.0.join("cred.tar.zst");
    write_bundle(
        &path,
        &[
            (
                "events.jsonl",
                "{\"seq\":1,\"type\":\"progress\",\"ts\":\"2026-09-24T09:31:10Z\"}\n",
            ),
            (
                "harness.jsonl",
                "{\"level\":\"info\",\"message\":\"up\",\"ts\":\"2026-09-24T09:31:10Z\"}\n",
            ),
            (
                "gateway.jsonl",
                "{\"provider\":\"anthropic\",\"ts\":\"2026-09-24T09:31:10Z\"}\n",
            ),
            ("findings.jsonl", "{\"state\":\"finding\",\"title\":\"t\"}\n"),
            // A rotated ledger and a work file: not credentials, but not ledgers either.
            ("events-2.jsonl", "{\"seq\":2,\"type\":\"progress\"}\n"),
            ("out/pr.md", "the pull request body\n"),
            // Every shape `is_credential_file` refuses, each holding the canary.
            ("vm/token", &format!("{{\"token\":\"{canary}\"}}\n")),
            ("gateway-token", &format!("{canary}\n")),
            ("vm/mesh-authkey", &format!("{canary}\n")),
            ("vm/session.json", &format!("{{\"token\":\"{canary}\"}}\n")),
            ("secrets/env", &format!("{canary}\n")),
            ("colony-secrets/env", &format!("{canary}\n")),
            ("agent.pem", &format!("{canary}\n")),
            ("signing.key", &format!("{canary}\n")),
        ],
    );
    let mut malformed = 0;
    let lines = read_all(&path, &mut malformed);
    assert_eq!(malformed, 0);
    let sources: BTreeSet<String> = lines.iter().map(|(s, _)| s.as_str().to_string()).collect();
    assert_eq!(
        sources,
        BTreeSet::from(["events", "harness", "gateway", "findings"].map(String::from))
    );
    assert_eq!(lines.len(), 4, "one line per ledger, nothing else");
    let read = serde_json::to_string(&lines.iter().map(|(_, l)| l).collect::<Vec<_>>()).unwrap();
    crate::testkit::assert_absent(read.as_bytes(), [canary], "the bundle's credentials");
}

/// The committed fixture — a bundle the mothership's own archiver wrote over the v0.1.9 colony —
/// reads back as its two ledgers: `out/pr.md` is not a ledger, and the credential `vm/session.json`
/// never made it into the bundle at all.
#[test]
fn the_committed_fixture_yields_only_its_ledgers() {
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../colonizer/tests/fixtures/observability/archive/acme/widgets/2026/09/a1b2c3d4.tar.zst");
    let mut malformed = 0;
    let lines = read_all(&bundle, &mut malformed);
    assert_eq!(malformed, 0);
    let mut events = 0;
    let mut findings = 0;
    for (source, _) in &lines {
        match source {
            Source::Events => events += 1,
            Source::Findings => findings += 1,
            other => panic!("the fixture bundle yielded {other:?}"),
        }
    }
    assert_eq!((events, findings), (2, 1));
}

/// Test (e): a bundle of roughly 500 MiB uncompressed streams through the reader in bounded memory.
/// The measure is the growth of the process's own peak resident set (`VmHWM` in /proc/self/status)
/// across the read — a high-water mark, so the delta is exactly what reading the bundle added on
/// top of the build's peak — asserted under 64 MiB. Ignored: it weighs out half a gigabyte in the
/// temp directory and takes a while.
#[test]
#[ignore = "builds a ~500 MiB bundle in the temp directory; run it on purpose"]
fn a_half_gigabyte_bundle_streams_in_bounded_memory() {
    // One line of one length — the counter padded inside the message string, since a JSON number
    // may not carry leading zeros — so the tar header's size is exact and the bundle's weight is
    // known before it is built.
    let line =
        "{\"seq\":1,\"type\":\"progress\",\"message\":\"filler to weigh the bundle 0000000\",\"ts\":\"2026-09-24T09:31:10Z\"}\n";
    let target: u64 = 500 * 1024 * 1024;
    let count = target / line.len() as u64;
    let mut filler = Filler {
        left: count,
        line: line.as_bytes().to_vec(),
        buf: Vec::new(),
    };
    let root = root("half-gigabyte");
    let bundle = root.0.join("big.tar.zst");
    {
        let file = std::fs::File::create(&bundle).unwrap();
        let mut zstd = zstd::Encoder::new(file, 3).unwrap();
        let mut tar = tar::Builder::new(&mut zstd);
        let mut header = tar::Header::new_gnu();
        header.set_path("events.jsonl").unwrap();
        header.set_size(count * line.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        tar.append(&header, &mut filler).unwrap();
        tar.finish().unwrap();
        drop(tar);
        zstd.finish().unwrap().sync_all().unwrap();
    }

    let before = peak_rss();
    let (tx, rx) = channel();
    let colony = ArchivedColony {
        colony: "a1b2c3d4".into(),
        host: "host-1".into(),
        policy: ColonyPolicy::default(),
        bundle: bundle.clone(),
    };
    std::thread::spawn(move || stream(&[colony], tx));
    let mut lines = 0u64;
    for message in rx {
        match message {
            Message::Lines { lines: chunk, .. } => lines += chunk.len() as u64,
            Message::Done { malformed } => assert_eq!(malformed, 0),
        }
    }
    let after = peak_rss();
    let grew = after.saturating_sub(before);
    println!(
        "bundle {} bytes uncompressed, {} lines, peak rss {} -> {} bytes ({} grown)",
        count * line.len() as u64,
        lines,
        before,
        after,
        grew
    );
    assert_eq!(lines, count);
    assert!(grew <= 64 * 1024 * 1024, "the read grew peak rss by {grew} bytes");
}

/// Endless identical JSON lines, read straight into the tar without ever holding the entry.
struct Filler {
    left: u64,
    line: Vec<u8>,
    buf: Vec<u8>,
}

impl Read for Filler {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.buf.len() < out.len() && self.left > 0 {
            self.buf.extend_from_slice(&self.line);
            self.left -= 1;
        }
        let n = out.len().min(self.buf.len());
        let rest = self.buf.split_off(n);
        out[..n].copy_from_slice(&self.buf);
        self.buf = rest;
        Ok(n)
    }
}

/// The process's peak resident set, in bytes, straight from the kernel.
fn peak_rss() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb = status
        .lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|kb| kb.parse::<u64>().ok())
        .expect("VmHWM in /proc/self/status");
    kb * 1024
}
