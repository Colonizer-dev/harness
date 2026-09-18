//! Append-only session event log (`<state-dir>/events.jsonl`) with gap-free replay and live
//! broadcast. Every event gets a monotonically increasing `seq` and an RFC 3339 `ts`.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

pub type Event = Map<String, Value>;

pub struct Stored {
    pub seq: u64,
    pub line: String,
}

pub struct EventStore {
    path: PathBuf,
    inner: Mutex<Inner>,
    live: broadcast::Sender<Arc<Stored>>,
}

struct Inner {
    file: File,
    last_seq: u64,
    agent_state: String,
}

#[derive(Deserialize)]
struct SeqOnly {
    seq: u64,
}

impl EventStore {
    /// Opens (or creates) the log and continues numbering after the highest stored `seq`.
    pub fn open(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("events.jsonl");
        let last_seq = read_events(&path, 0, u64::MAX)
            .iter()
            .map(|e| e.seq)
            .max()
            .unwrap_or(0);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        // A crash can leave a torn final line; start the next event on a fresh line.
        if file.metadata()?.len() > 0 {
            let mut last = [0u8; 1];
            file.seek(SeekFrom::End(-1))?;
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        let (live, _) = broadcast::channel(1024);
        Ok(Self {
            path,
            inner: Mutex::new(Inner {
                file,
                last_seq,
                agent_state: "starting".into(),
            }),
            live,
        })
    }

    pub fn append(&self, mut event: Event) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        inner.last_seq += 1;
        let seq = inner.last_seq;
        event.insert("seq".into(), seq.into());
        event.insert(
            "ts".into(),
            Value::String(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        );
        if event.get("type").and_then(Value::as_str) == Some("status")
            && let Some(state) = event.get("state").and_then(Value::as_str)
        {
            inner.agent_state = state.to_string();
        }
        let line = Value::Object(event).to_string();
        if let Err(e) = writeln!(inner.file, "{line}") {
            eprintln!("colonizer-agentd: cannot write event log: {e}");
        }
        let _ = self.live.send(Arc::new(Stored { seq, line }));
        seq
    }

    /// Current position plus a live receiver, taken atomically so replay + live never gaps.
    pub fn subscribe(&self) -> (u64, broadcast::Receiver<Arc<Stored>>) {
        let inner = self.inner.lock().unwrap();
        (inner.last_seq, self.live.subscribe())
    }

    pub fn last_seq(&self) -> u64 {
        self.inner.lock().unwrap().last_seq
    }

    pub fn agent_state(&self) -> String {
        self.inner.lock().unwrap().agent_state.clone()
    }

    /// Stored events with `after < seq <= upto`, in order.
    pub async fn replay(&self, after: u64, upto: u64) -> Vec<Stored> {
        if upto <= after {
            return Vec::new();
        }
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || read_events(&path, after, upto))
            .await
            .unwrap_or_default()
    }
}

fn read_events(path: &Path, after: u64, upto: u64) -> Vec<Stored> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .split(b'\n')
        .map_while(Result::ok)
        .filter_map(|bytes| {
            let line = String::from_utf8(bytes).ok()?;
            let seq = serde_json::from_str::<SeqOnly>(&line).ok()?.seq;
            (seq > after && seq <= upto).then_some(Stored { seq, line })
        })
        .collect()
}

pub fn log_event(level: &str, message: impl Into<String>) -> Event {
    object(json!({"type": "log", "level": level, "message": message.into()}))
}

pub fn status_event(state: &str, detail: Option<String>) -> Event {
    let mut event = object(json!({"type": "status", "state": state}));
    if let Some(detail) = detail {
        event.insert("detail".into(), Value::String(detail));
    }
    event
}

fn object(value: Value) -> Event {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}
