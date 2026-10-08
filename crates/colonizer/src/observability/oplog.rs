//! The mothership's own process log (#856): everything `tracing` records from the running harness,
//! written as redacted JSON lines into a rotated file the observability add-on tails.
//!
//! This is the harness's console, not a colony's log. A colony's turns live in its own session log
//! and in `activity.jsonl`; what is here is what *the process* has to say — a provider's quota ran
//! out, a fleet file did not parse, a mesh policy could not be written. Those lines used to be
//! `eprintln!`s that vanished when the terminal closed, and the exporter could not see them, so a
//! support answer about a mothership had nothing to read.
//!
//! Two layers, one filter. **stderr** writes the message and nothing else — no timestamp, no level,
//! no target, no colour — so at the default filter every line an operator used to see still appears,
//! byte for byte, under the terminal they ran it in. (`COLONIZER_LOG` can of course narrow that,
//! which an `eprintln!` could not: that is the point of it.) **The file** writes the whole event as
//! one JSON object, with the time, the level, the target and every structured field, which is what
//! the add-on ships and what makes a line searchable after the fact.
//!
//! ## The file
//!
//! `<data>/logs/mothership.jsonl`, rotated to `mothership.jsonl.1` at [`ROTATE_BYTES`] and one
//! generation only, so at most two files ever exist and the pair on disk stays under 16 MB whatever
//! happens. Every line goes through [`crate::redact::redact_line`] before it is appended: a
//! provider key in a request body a gateway echoed, a token in an error from an upstream, and the
//! log says `[REDACTED:…]` instead. The redaction is the same code the activity log uses, so
//! nothing can reach this file that would not have been kept out of that one.
//!
//! ## Logging never blocks
//!
//! The emitting thread does not touch the disk. It hands the line to a bounded channel and returns;
//! one writer thread drains it, redacts, rolls and appends. When the channel is full the line is
//! *dropped and counted*, never waited on: a log call is made from the middle of a request or a
//! colony's boot path, and a slow disk must not become a slow gateway. [`dropped_lines`] is how a
//! test — and, later, a metric — reads that count.
//!
//! ## The flood rule
//!
//! This module — and `colonizer::observability` as a whole — targets **log state changes only**:
//! the provider went out of quota, the policy was not written, the subscription list could not be
//! read. Never a per-record line. A backend that is failing must not be able to flood the file it is
//! being shipped, because the flood would bury the one line that says why it is failing. A loop that
//! wants to say something once per item logs its outcome, its rate or its count — not each item.
//!
//! ## Where it is installed
//!
//! [`install`] is called from `serve()` and nowhere else, so a CLI subcommand — `colonizer setup`,
//! `colonizer update` — never creates a `logs/` directory or a log file in a data
//! directory it was only asked to inspect. Nothing here is async-safe to rely on either: a `serve()`
//! that never ran has no subscriber, and a `tracing` event emitted without one costs a call and
//! writes nothing.

use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
};

use chrono::SecondsFormat;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    EnvFilter,
    filter::LevelFilter,
    fmt::{
        self, FmtContext,
        format::{FormatEvent, FormatFields, Writer},
    },
    layer::SubscriberExt,
    registry::LookupSpan,
};

/// The directory under the data dir that holds the live and rolled log.
pub(crate) const LOG_DIR: &str = "logs";
/// The live file, in [`LOG_DIR`].
pub(crate) const FILE: &str = "mothership.jsonl";
/// The previous generation, kept whole until the live file fills again.
pub(crate) const ROLLED: &str = "mothership.jsonl.1";
/// When the live file rolls over. 8 MB is roughly a hundred thousand lines of a busy install, and
/// the pair on disk stays under 16 MB whatever happens.
pub(crate) const ROTATE_BYTES: u64 = 8 * 1024 * 1024;
/// Lines the writer thread can be behind before the emitting thread starts dropping. A busy
/// gateway answers thousands of requests a minute; four thousand queued lines is seconds of a slow
/// disk, which is the whole point at which dropping beats blocking.
pub(crate) const CHANNEL_CAPACITY: usize = 4096;

/// The env var that sets the filter, in `RUST_LOG` syntax. Unset or empty means [`DEFAULT_FILTER`].
const FILTER_VAR: &str = "COLONIZER_LOG";
/// What an unset, empty or unusable [`FILTER_VAR`] falls back to: the state changes, none of the
/// chatter.
const DEFAULT_FILTER: LevelFilter = LevelFilter::INFO;

// ---------------------------------------------------------------------------
// Installation.
// ---------------------------------------------------------------------------

/// Sets the global subscriber: the message-only stderr layer and the JSON file layer, both filtered
/// by [`COLONIZER_LOG`]. Called once, from `serve()`, after the data dir exists.
///
/// A subscriber that is already set — a test binary, an embedder — is left alone rather than
/// panicking: losing the log is better than refusing to start. So is a log directory that cannot be
/// created, which leaves stderr working and the file not written.
///
/// CLI subcommands never call this, so a data dir that was only inspected keeps no `logs/`.
pub(crate) fn install(data_dir: &Path) {
    // The filter goes on last, as its own layer over both: an `EnvFilter` is itself a `Layer`, and
    // a per-layer `with_filter` produces a type that can only sit on the bare registry, not on top
    // of another layer. One filter over the pair is what the two layers have to agree on anyway.
    let filter = filter();
    // Both `fmt::Layer`s are built inside their arm, and each carries its subscriber in its type:
    // a layer made for the bare registry cannot be stacked on a registry the file layer is already
    // on. Two arms rather than one composed subscriber because of it — and both arms carry the
    // stderr layer, so a log dir that could not be made leaves the terminal working and the file
    // simply unwritten.
    match log_channel(data_dir) {
        Some(channel) => {
            let dropped = Arc::new(AtomicU64::new(0));
            DROPPED.set(Arc::clone(&dropped)).ok();
            let file = fmt::layer()
                .with_writer(ChannelWriter { tx: channel.tx, dropped })
                .event_format(JsonEvent);
            // A writer that never started loses every line silently, so the refusal is said out
            // loud — on stderr, once, before the subscriber below is installed to replace it.
            if let Err(error) = spawn_writer(channel.rx, data_dir.join(LOG_DIR), ROTATE_BYTES) {
                eprintln!(
                    "colonizer: the {LOG_DIR} writer thread could not start ({error}); \
                     {FILE} will not be written"
                );
            }
            let stderr = fmt::layer().with_writer(io::stderr).event_format(MessageOnly);
            let subscriber = tracing_subscriber::registry().with(file).with(stderr).with(filter);
            let _ = tracing::subscriber::set_global_default(subscriber);
        }
        None => {
            let stderr = fmt::layer().with_writer(io::stderr).event_format(MessageOnly);
            let subscriber = tracing_subscriber::registry().with(stderr).with(filter);
            let _ = tracing::subscriber::set_global_default(subscriber);
        }
    }
}

/// How many lines the emitting threads gave up because the writer was behind. `None` until a
/// subscriber is installed and the counter exists.
pub(crate) fn dropped_lines() -> Option<u64> {
    DROPPED.get().map(|d| d.load(Ordering::Relaxed))
}

/// The counter [`install`] publishes, so [`dropped_lines`] reads one the file layer is using.
static DROPPED: OnceLock<Arc<AtomicU64>> = OnceLock::new();

/// Whether the "the writer thread is gone" warning has already been printed. One message for the
/// process: a dead writer is a permanent condition, and a line per event would be the flood the
/// flood rule exists to prevent.
static WRITER_GONE: AtomicBool = AtomicBool::new(false);

/// `COLONIZER_LOG` in `RUST_LOG` syntax, defaulting to [`DEFAULT_FILTER`].
///
/// The builder form, not `EnvFilter::try_from_env`, and the difference is the whole point: a
/// *default directive* is a floor, so a value that yields no directives at all — `COLONIZER_LOG=`
/// set but empty, or only separators and whitespace — falls back to it. `try_from_env` has no such
/// floor: the empty string parses *successfully* to a filter with **no** directives, which enables
/// nothing, so `mothership.jsonl` was never even created. That is exactly the failure this module
/// exists to remove, so an empty filter variable must never blank the log.
///
/// `from_env_lossy` is lossy *per directive*: `COLONIZER_LOG=debug,bogus!!` keeps `debug` and says
/// on stderr that it ignored the other, where `try_from_env` would have thrown the whole value away.
///
/// What the floor does *not* do is overrule a value that parsed: a bare word is `RUST_LOG`'s
/// shorthand for "everything for this target", so `COLONIZER_LOG=push` means `push` and nothing
/// else. That is a request the operator can make and mean, and no fallback should answer it.
fn filter() -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(DEFAULT_FILTER.into())
        .with_env_var(FILTER_VAR)
        .from_env_lossy()
}

/// The bounded channel the file layer writes into, when the log directory could be made.
fn log_channel(data_dir: &Path) -> Option<Channel> {
    let dir = data_dir.join(LOG_DIR);
    std::fs::create_dir_all(&dir).ok()?;
    let (tx, rx) = sync_channel(CHANNEL_CAPACITY);
    Some(Channel { tx, rx })
}

/// The two halves of the channel: the emitting side the `MakeWriter` clones per event, and the
/// receiver the one writer thread owns.
struct Channel {
    tx: SyncSender<String>,
    rx: Receiver<String>,
}

// ---------------------------------------------------------------------------
// The two formatters.
// ---------------------------------------------------------------------------

/// Takes the field named `message` and nothing else. Every field a migrated site adds is metadata
/// for the file layer; the terminal gets the sentence and nothing else, which is what makes the
/// old `eprintln!` output reproduce exactly.
struct MessageOnly;

impl<S, N> FormatEvent<S, N> for MessageOnly
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, _ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> std::fmt::Result {
        let mut message = MessageVisitor::default();
        event.record(&mut message);
        writer.write_str(&message.0)?;
        writer.write_str("\n")
    }
}

/// Collects the `message` field. Its `Debug` is the formatted text itself — `format_args!` renders
/// as `Display` — so no unquoting is needed; the tests prove it byte for byte.
#[derive(Default)]
struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

/// One JSON object per line, in the fixed shape the exporter's mapping expects:
///
/// ```json
/// {"ts":"2026-10-08T09:12:33.481Z","level":"error","target":"colonizer::push","message":"…","fields":{…}}
/// ```
///
/// Every field other than `message` goes into `fields`, as the JSON value it printed as when it can
/// be one (a number, a bool, a quoted string) and as a string otherwise. The object is always
/// present, empty when there is nothing to say, so a reader never has to test for it.
struct JsonEvent;

impl<S, N> FormatEvent<S, N> for JsonEvent
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, _ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> std::fmt::Result {
        let meta = event.metadata();
        let mut fields = FieldVisitor::default();
        event.record(&mut fields);
        // The three scalars go through `json`, which quotes and escapes them; `fields` is already
        // a serialised object and goes in as it stands, or it would land as a JSON string of JSON.
        writer.write_str(&format!(
            "{{\"ts\":{},\"level\":{},\"target\":{},\"message\":{},\"fields\":{}}}",
            json(&chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
            // `Level::as_str` is uppercase ("ERROR"); the file's `level` is the lowercase name, which is what
            // a filter directive and a `jq` selector both spell.
            json(&meta.level().as_str().to_ascii_lowercase()),
            json(meta.target()),
            json(&fields.message),
            serde_json::Value::Object(std::mem::take(&mut fields.rest)),
        ))
    }
}

/// The message on its own and every other field beside it.
#[derive(Default)]
struct FieldVisitor {
    message: String,
    rest: serde_json::Map<String, serde_json::Value>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let debug = format!("{value:?}");
        if field.name() == "message" {
            self.message = debug;
        } else {
            self.rest.insert(field.name().to_string(), scalar(&debug));
        }
    }
}

/// A field's `Debug` text as the JSON value it already is, or as a string when it is prose.
/// `Debug` renders a string *with* its quotes, so a quoted parse is the string itself and comes
/// back unquoted — the two cancel, and `path` lands in the file as `"path":"fleet.json"`.
fn scalar(debug: &str) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(debug) {
        Ok(value) if value.is_boolean() || value.is_number() => value,
        Ok(value @ serde_json::Value::String(_)) => value,
        // An object or an array is a field that printed as a structure: prose, kept as text rather
        // than spliced into the line as a shape a reader did not ask for.
        _ => serde_json::Value::String(debug.to_string()),
    }
}

/// One string as a JSON string, quotes and escaping included.
fn json(text: &str) -> String {
    serde_json::Value::String(text.to_string()).to_string()
}

// ---------------------------------------------------------------------------
// The file layer's writer and the thread behind it.
// ---------------------------------------------------------------------------

/// The file layer's `MakeWriter`: a clone of the sender and the shared dropped counter. No file
/// handle, no `File`, no lock — a log call on the request path is a channel hand-off and a counter
/// add at worst.
struct ChannelWriter {
    tx: SyncSender<String>,
    dropped: Arc<AtomicU64>,
}

/// The per-event handle the formatter writes through. It buffers until a newline so a formatter that
/// writes the line in pieces still sends one line.
struct ChannelGuard {
    tx: SyncSender<String>,
    dropped: Arc<AtomicU64>,
    buf: String,
}

impl io::Write for ChannelGuard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.push_str(&String::from_utf8_lossy(buf));
        while let Some(at) = self.buf.find('\n') {
            let line = self.buf.drain(..=at).collect::<String>();
            self.send(&line);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.send(&line);
        }
        Ok(())
    }
}

impl Drop for ChannelGuard {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

impl ChannelGuard {
    /// Hands the line over, never waits. A full channel is the disk being behind and the caller
    /// being a request: the line goes, the count goes up, and neither the request nor the colony's
    /// boot path learns about it.
    ///
    /// A *disconnected* receiver is the other failure, and the louder one: the writer thread is
    /// gone, so the file is now stale for the rest of the run and every line after this is lost.
    /// It is not counted as dropped — the dropped counter means "the writer was too slow", and the
    /// shutdown line built on it would say the wrong thing — but it is not silent either: one
    /// [`eprintln!`] the first time it happens, never one per line. `eprintln!` and not `tracing`,
    /// which would be trying to log through the very layer that has stopped working.
    fn send(&self, line: &str) {
        let mut owned = line.to_string();
        if !owned.ends_with('\n') {
            owned.push('\n');
        }
        match self.tx.try_send(owned) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                if !WRITER_GONE.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "colonizer: the {LOG_DIR} writer thread is gone; \
                         {FILE} is no longer being written and its lines are being dropped"
                    );
                }
            }
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ChannelWriter {
    type Writer = ChannelGuard;

    fn make_writer(&'a self) -> Self::Writer {
        ChannelGuard {
            tx: self.tx.clone(),
            dropped: Arc::clone(&self.dropped),
            buf: String::new(),
        }
    }
}

/// The one writer thread. It is the only thing in the process that opens the log file.
///
/// A spawn that fails — the thread limit, a naming clash on a platform that enforces one — leaves
/// nothing draining the channel, so every later line is silently lost and the file goes stale
/// mid-run while the process shuts down clean. The error is therefore returned for [`install`] to
/// print, once, at the one moment stderr is still the only trustworthy sink: it is before the
/// subscriber it would log through exists.
fn spawn_writer(rx: Receiver<String>, dir: PathBuf, rotate_bytes: u64) -> io::Result<()> {
    std::thread::Builder::new()
        .name("colonizer-oplog".into())
        .spawn(move || pump(rx, dir, rotate_bytes))?;
    Ok(())
}

/// Drains the channel until the last sender goes away: redact, roll, append.
fn pump(rx: Receiver<String>, dir: PathBuf, rotate_bytes: u64) {
    let live = dir.join(FILE);
    let rolled = dir.join(ROLLED);
    for line in rx {
        // The line is a JSON object by construction, so the redactor's JSON path rewrites every
        // string leaf and leaves the shape intact; a line it cannot parse falls through to text.
        let line = crate::redact::redact_line(&line);
        // Rotation is one generation: `.1` is replaced, so at most two files ever exist.
        if std::fs::metadata(&live).is_ok_and(|m| m.len() >= rotate_bytes) && std::fs::rename(&live, &rolled).is_err() {
            // Appending on past the cap is the lesser harm: the line is kept, the file grows until
            // the next roll works, and the next line says why it could not.
        }
        // The newline is added here rather than trusted: the layer's writer terminates its own
        // line, but `pump` takes whatever it is given, and two joined lines would be one record
        // the reader could not split.
        let terminated = if line.ends_with('\n') {
            line.into_owned()
        } else {
            format!("{line}\n")
        };
        match OpenOptions::new().create(true).append(true).open(&live) {
            Ok(mut file) => {
                let _ = file.write_all(terminated.as_bytes());
            }
            Err(_) => {
                // Not logged, and that is the point: an append failure is exactly the case where
                // logging through the same broken file would flood the very file it is failing to
                // write. The next line retries.
            }
        }
    }
}

#[cfg(test)]
mod tests;
