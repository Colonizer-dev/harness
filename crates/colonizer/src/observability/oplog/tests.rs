//! The tests for the mothership log: the stderr layer's exact bytes, the rotated pair of files, the
//! secret canary, back-pressure, and the CLI that installs nothing.

use super::*;

use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

/// A scratch data dir that removes itself.
struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn root(tag: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("colonizer-oplog-{tag}-{}", crate::util::short_id()));
    std::fs::create_dir_all(&dir).unwrap();
    Root(dir)
}

/// Captures a layer's bytes in memory, so the stderr formatter can be asserted on directly.
#[derive(Clone)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

struct BufferGuard(Arc<Mutex<Vec<u8>>>);

impl io::Write for BufferGuard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Buffer {
    type Writer = BufferGuard;
    fn make_writer(&'a self) -> Self::Writer {
        BufferGuard(Arc::clone(&self.0))
    }
}

/// The stderr layer's formatter over a buffer, with nothing filtering anything out.
fn stderr_capture(buffer: &Buffer) -> impl Subscriber + Send + Sync {
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(buffer.clone()).event_format(MessageOnly))
        .with(EnvFilter::new("trace"))
}

/// The file layer's formatter, without the channel: straight into the buffer, so the JSON shape can
/// be read back as an object.
fn json_capture(buffer: &Buffer) -> impl Subscriber + Send + Sync {
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(buffer.clone()).event_format(JsonEvent))
        .with(EnvFilter::new("trace"))
}

fn bytes(buffer: &Buffer) -> String {
    String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap()
}

// ---------------------------------------------------------------------------
// The filter.
// ---------------------------------------------------------------------------

/// The environment variable is process-global, so these tests take this lock for as long as they
/// have it changed. It is never held across a panic: [`FilterVar`]'s `Drop` restores the value and
/// releases the guard on the way out of an unwinding test too.
static FILTER_ENV: Mutex<()> = Mutex::new(());

/// Sets [`FILTER_VAR`] for one test and puts it back afterwards — including on a panic, since the
/// restore is in `Drop`. The docs tell an operator to export `COLONIZER_LOG`, so a test that read
/// the variable as it found it would fail on that developer's machine and nowhere else.
struct FilterVar {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Option<std::ffi::OsString>,
}

impl FilterVar {
    fn set(value: &str) -> Self {
        let guard = Self::take();
        // SAFETY: the `FILTER_ENV` lock above is held for the whole time the value is set, so no
        // other thread in this binary is reading or writing the variable.
        unsafe { std::env::set_var(FILTER_VAR, value) };
        guard
    }

    fn unset() -> Self {
        let guard = Self::take();
        // SAFETY: as above — `FILTER_ENV` is held.
        unsafe { std::env::remove_var(FILTER_VAR) };
        guard
    }

    fn take() -> Self {
        let lock = FILTER_ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        Self {
            saved: std::env::var_os(FILTER_VAR),
            _lock: lock,
        }
    }
}

impl Drop for FilterVar {
    fn drop(&mut self) {
        match self.saved.take() {
            // SAFETY: as above — this guard is still held.
            Some(value) => unsafe { std::env::set_var(FILTER_VAR, value) },
            // SAFETY: as above.
            None => unsafe { std::env::remove_var(FILTER_VAR) },
        }
    }
}

/// What one `info!`, one `debug!` and one `trace!` make it through [`filter`] to, on their own.
fn emitted_at_filter() -> String {
    let buffer = Buffer(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::registry()
        .with(fmt::layer().with_writer(buffer.clone()).event_format(MessageOnly))
        .with(filter());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("fleet: the mesh policy was not updated");
        tracing::debug!("mesh: attempt 1 failed");
        tracing::trace!("mesh: attempt 2 failed");
    });
    bytes(&buffer)
}

// ---------------------------------------------------------------------------

/// The stderr layer writes the message and nothing else, so every line an operator used to see off
/// `eprintln!` reproduces byte for byte — no timestamp, no level, no target, no colour, and the
/// message's own argument formatting intact.
#[test]
fn stderr_writes_only_the_message() {
    let buffer = Buffer(Arc::new(Mutex::new(Vec::new())));
    tracing::subscriber::with_default(stderr_capture(&buffer), || {
        tracing::error!("storage: append: disk full");
        tracing::warn!(
            "fleet: could not read {} ({e}); starting with no fleet",
            "fleet.json",
            e = "EACCES"
        );
        tracing::info!("push: could not build an HTTP client; the resolution was not sent");
        tracing::debug!("attempt {attempt} failed: {e:#}", attempt = 3, e = anyhow::anyhow!("boom"));
    });
    assert_eq!(
        bytes(&buffer),
        "storage: append: disk full\n\
         fleet: could not read fleet.json (EACCES); starting with no fleet\n\
         push: could not build an HTTP client; the resolution was not sent\n\
         attempt 3 failed: boom\n"
    );
}

/// A structured field is metadata: it reaches the file and stays off the terminal.
#[test]
fn stderr_drops_the_structured_fields() {
    let buffer = Buffer(Arc::new(Mutex::new(Vec::new())));
    tracing::subscriber::with_default(stderr_capture(&buffer), || {
        tracing::error!(path = %"fleet.json", colony = %"c-1", error = %"EACCES", "fleet: could not save the state");
    });
    assert_eq!(bytes(&buffer), "fleet: could not save the state\n");
}

/// The file line is one JSON object with the fixed key order, a millisecond RFC 3339 timestamp, the
/// level's lowercase name, the target, and every non-message field under `fields` — numbers and
/// bools as scalars, strings as strings.
#[test]
fn file_line_is_one_json_object() {
    let buffer = Buffer(Arc::new(Mutex::new(Vec::new())));
    tracing::subscriber::with_default(json_capture(&buffer), || {
        tracing::error!(path = %"fleet.json", error = %"EACCES", bytes = 4096u64, fresh = true, "fleet: could not save the state");
    });
    let line = bytes(&buffer);
    let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(value["level"], "error");
    // The target is the module the event was emitted from — here the test's own, which is the
    // point: a migrated site's target names the file an operator greps for.
    assert!(
        value["target"].as_str().unwrap().ends_with("observability::oplog::tests"),
        "{line}"
    );
    assert_eq!(value["message"], "fleet: could not save the state");
    assert_eq!(value["fields"]["path"], "fleet.json");
    assert_eq!(value["fields"]["error"], "EACCES");
    assert_eq!(value["fields"]["bytes"], 4096);
    assert_eq!(value["fields"]["fresh"], true);
    // The key order is the documented one, so a reader (and a diff) can rely on it.
    let keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["ts", "level", "target", "message", "fields"]);
    // …and the timestamp is RFC 3339 with milliseconds.
    let ts = value["ts"].as_str().unwrap();
    assert!(ts.ends_with('Z'), "{ts}");
    assert_eq!(ts.len(), "2026-10-08T09:12:33.481Z".len(), "{ts}");
}

/// The writer rolls the live file to `.1` at the cap, keeps the older lines there, and never leaves
/// a third file behind.
#[test]
fn the_pair_rolls_at_the_cap() {
    let scratch = root("roll");
    let dir = scratch.0.join(LOG_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    let (tx, rx) = sync_channel(64);
    let for_writer = dir.clone();
    let writer = std::thread::Builder::new().spawn(move || pump(rx, for_writer, 200)).unwrap();
    // Each line is well over the 200-byte cap, so the second one rolls the first away.
    for at in 0..6 {
        let _ = tx.send(format!("line {at} {}", "x".repeat(120)));
    }
    drop(tx);
    writer.join().unwrap();

    let live = std::fs::read_to_string(dir.join(FILE)).unwrap();
    let rolled = std::fs::read_to_string(dir.join(ROLLED)).unwrap();
    // The newest line is in the live file and nowhere else, the roll happened (the pair is not
    // empty), and the two share no line — a rename replaced `.1` whole rather than appending to it.
    assert!(live.contains("line 5"), "{live}");
    assert!(!rolled.contains("line 5"), "{rolled}");
    assert!(!rolled.is_empty(), "the roll never happened");
    for line in rolled.lines().filter(|l| l.starts_with("line ")) {
        assert!(!live.contains(line), "line {line} is in both files");
    }

    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
}

/// A canary the repo is not allowed to hold: built at runtime, and absent from the file whether it
/// arrives in the message or in a field.
#[test]
fn a_secret_is_redacted_in_the_message_and_in_a_field() {
    let scratch = root("canary");
    let dir = scratch.0.join(LOG_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    let (tx, rx) = sync_channel(64);
    let for_writer = dir.clone();
    let writer = std::thread::Builder::new()
        .spawn(move || pump(rx, for_writer, ROTATE_BYTES))
        .unwrap();
    let canary = format!("sk-{}", "a".repeat(40));

    // Through the real layer, so the whole path is exercised: format, redact, rotate, append.
    let dropped = Arc::new(AtomicU64::new(0));
    let subscriber = tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(ChannelWriter { tx, dropped })
                .event_format(JsonEvent),
        )
        .with(EnvFilter::new("trace"));
    tracing::subscriber::with_default(subscriber, || {
        tracing::error!("push: the provider answered with {canary}");
        tracing::error!(error = %canary, "gateway: the upstream refused the request");
    });
    writer.join().unwrap();

    let file = std::fs::read_to_string(dir.join(FILE)).unwrap();
    assert!(!file.contains(&canary), "the canary reached the log: {file}");
    assert_eq!(file.matches("[REDACTED:").count(), 2, "{file}");
}

/// The emitter never waits on the disk. Filling the channel and then emitting far past it returns
/// promptly, drops the surplus, and counts exactly it.
#[test]
fn a_full_channel_drops_rather_than_blocking() {
    let (tx, rx) = sync_channel::<String>(4);
    let dropped = Arc::new(AtomicU64::new(0));
    let subscriber = tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(ChannelWriter {
                    tx,
                    dropped: Arc::clone(&dropped),
                })
                .event_format(MessageOnly),
        )
        .with(EnvFilter::new("trace"));

    const EMITTED: u64 = 500;
    let started = std::time::Instant::now();
    tracing::subscriber::with_default(subscriber, || {
        for at in 0..EMITTED {
            tracing::warn!("mesh: attempt {at} failed");
        }
    });
    let elapsed = started.elapsed();

    // Nothing drains `rx` for the whole loop, so a blocking send would hang here rather than time.
    assert!(elapsed.as_secs() < 1, "the emitting loop took {elapsed:?}");

    // Four lines are queued; the rest were dropped, and the counter says so.
    let accepted = rx.try_iter().count() as u64;
    assert_eq!(accepted, 4);
    assert_eq!(dropped.load(Ordering::Relaxed), EMITTED - accepted);
}

/// A subcommand that never called [`install`] leaves no log behind: no `logs/` directory, no file,
/// and no dropped-line counter — the three things `install` is what brings into being.
#[test]
fn an_install_that_never_ran_writes_nothing() {
    let dir = root("cli");
    // No test calls `install` (a global subscriber would fight the rest of this binary), so the
    // counter a real run would publish is not there either.
    assert_eq!(dropped_lines(), None);
    assert!(!dir.0.join(LOG_DIR).exists());
    assert!(!dir.0.join(LOG_DIR).join(FILE).exists());
    // And nothing global leaked out of the other tests either.
    tracing::error!("nothing subscribes to this");
}

/// The writer survives a file it cannot open: it does not die, and it does not recurse.
#[test]
fn an_unwritable_log_does_not_kill_the_writer() {
    // `logs` is a regular file, so the directory below it cannot be made and every open fails.
    let dir = root("unwritable");
    std::fs::write(dir.0.join(LOG_DIR), "not a directory").unwrap();
    let log_dir = dir.0.join(LOG_DIR);
    let (tx, rx) = sync_channel(8);
    let writer = std::thread::Builder::new()
        .spawn(move || pump(rx, log_dir, ROTATE_BYTES))
        .unwrap();
    for at in 0..4 {
        let _ = tx.send(format!("line {at}"));
    }
    drop(tx);
    // Joining proves the thread drained to the end instead of stopping at the first failure.
    writer.join().unwrap();
    assert_eq!(std::fs::read_to_string(dir.0.join(LOG_DIR)).unwrap(), "not a directory");
}

/// With the variable unset the filter is `info`: the `info` line is written, the `debug` and
/// `trace` ones are not. `install` itself is never called by a test — it would fight every other
/// test in the binary for the global subscriber — so the filter is exercised through [`filter`],
/// which is what `install` reads. The variable is cleared and restored around the call, so an
/// exported `COLONIZER_LOG` cannot decide whether this passes.
#[test]
fn the_default_filter_is_info() {
    assert_eq!(DEFAULT_FILTER, LevelFilter::INFO);
    assert_eq!(FILTER_VAR, "COLONIZER_LOG");
    let _var = FilterVar::unset();
    assert_eq!(emitted_at_filter(), "fleet: the mesh policy was not updated\n");
}

/// No value of [`FILTER_VAR`] that a mistyped operator would actually produce can blank the log
/// (#856): unset, empty, or a directive that does not parse all keep the default `info`.
///
/// This is the builder form's whole reason for existing. `EnvFilter::try_from_env` accepts the empty
/// string as a *successful* filter with no directives, which enables nothing at all, so
/// `mothership.jsonl` was never even created.
#[test]
fn no_value_of_the_filter_var_silences_the_default() {
    for value in ["", " ", ",", ",,", "="] {
        let _var = FilterVar::set(value);
        assert_eq!(
            emitted_at_filter(),
            "fleet: the mesh policy was not updated\n",
            "COLONIZER_LOG={value:?} silenced the info level"
        );
    }
}

/// …and a value that does name a level is obeyed, in both directions: `warn` drops the `info`
/// line, `debug` adds the `debug` one. A default directive is a floor, not a clamp.
///
/// A bare word (`COLONIZER_LOG=nope`) is *not* in the test above for a reason worth stating:
/// `RUST_LOG` reads that as "everything for the target `nope`", which is a real request for a
/// target this crate does not have, and it is answered as one. What it is not is a value that
/// fails to parse, so it does not fall back — and the docs' claim is about parsing, not about
/// naming a target that matches nothing.
#[test]
fn a_named_level_still_narrows_the_log() {
    for (value, expected) in [
        ("warn", ""),
        ("debug", "fleet: the mesh policy was not updated\nmesh: attempt 1 failed\n"),
        ("error", ""),
    ] {
        let _var = FilterVar::set(value);
        assert_eq!(emitted_at_filter(), expected, "COLONIZER_LOG={value:?}");
    }
}
