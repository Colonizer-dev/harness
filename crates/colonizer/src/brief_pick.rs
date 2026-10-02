//! Jev brief picks (#585), shadow only. A colony boots with shared-memory notes and skill packs it
//! may read; this asks Jev which of them it would need — up to [`MAX_PICKS`], one `choice` question a
//! round — records the picks, then watches the colony's tool calls and results for the ones it really
//! used: the ground truth the bench grades precision and recall against (`scripts/bench.mjs brief`).
//!
//! Shadow only: nothing a colony sees changes and memory stays pull-only. It is a standalone
//! measurement, ready to move onto #582's decision layer the way `verify_focus`'s chooser waits for
//! it. One rule is a safety property, not a measurement: a note tagged `house-rule` or `security`
//! ([`MANDATORY_TAGS`]) is always loaded and never offered, so a pick can never drop it — [`split`]
//! keeps such notes out of the candidates and [`selection`] puts them into what `act` would load.
//!
//! Two rows land in `<data dir>/brief_picks.jsonl`, tagged `kind` like the other Jev ledgers: one
//! `pick` per colony boot and one `used` per watched item really touched. Never any note content.

use crate::{Shared, memory::Note, sessions::Runtime, util::append_line};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

/// A note carrying any of these tags is mandatory: always loaded, never offered, never droppable.
/// Kept lowercase, matching how `memory::draft` normalises tags.
pub(crate) const MANDATORY_TAGS: [&str; 2] = ["house-rule", "security"];

/// How many candidates a colony offers Jev — enough to keep a boot's question and state small.
const MAX_CANDIDATES: usize = 64;

/// The most items one pick loop chooses — one round may land at most one pick.
pub(crate) const MAX_PICKS: usize = 5;

/// The one question id asked each round; the options are the remaining candidate labels plus `none`.
const QUESTION_ID: &str = "brief_item";

/// The option that ends a pick loop.
const NONE: &str = "none";

/// One offered item: a shared-memory note or a skill pack. `label` is `note:<scope>/<id>` or
/// `skill:<pack>` — the option string sent to Jev and the ledger's key; `path` is the guest path the
/// colony reads it at. Metadata only, never the note's content.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Candidate {
    pub label: String,
    pub title: String,
    pub path: String,
    pub bytes: u64,
    pub kind: String,
}

/// The guest path of a note's markdown file (boot mounts each scope at `/colonizer/memory/<scope>`).
fn note_path(scope: &str, id: &str) -> String {
    format!("/colonizer/memory/{scope}/notes/{id}.md")
}

fn is_mandatory(note: &Note) -> bool {
    note.tags.iter().any(|tag| MANDATORY_TAGS.contains(&tag.as_str()))
}

/// Splits the boot-time notes and packs into the mandatory labels (never offered) and the candidates
/// (offered, capped at [`MAX_CANDIDATES`]): offered notes newest first, then skills, so a full note
/// list crowds a pack out before it drops a recent note. `skills` is each pack's name and size.
pub(crate) fn split(notes: &[Note], skills: &[(String, u64)]) -> (Vec<String>, Vec<Candidate>) {
    let label = |note: &Note| format!("note:{}/{}", note.scope, note.id);
    let mandatory: Vec<String> = notes.iter().filter(|n| is_mandatory(n)).map(label).collect();
    let mut offered: Vec<&Note> = notes.iter().filter(|n| !is_mandatory(n)).collect();
    offered.sort_by_key(|note| std::cmp::Reverse(note.created_at));
    let mut candidates: Vec<Candidate> = offered
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|note| Candidate {
            label: label(note),
            title: note.title.clone(),
            path: note_path(&note.scope, &note.id),
            bytes: note.content.len() as u64,
            kind: note.kind.clone(),
        })
        .collect();
    for (name, bytes) in skills {
        if candidates.len() >= MAX_CANDIDATES {
            break;
        }
        candidates.push(Candidate {
            label: format!("skill:{name}"),
            title: name.clone(),
            path: format!("/opt/colonizer/plugins/{name}"),
            bytes: *bytes,
            kind: "skill".into(),
        });
    }
    (mandatory, candidates)
}

/// What `act` would load: the mandatory items, which always load, then the picks. Pure, so the rule
/// that a mandatory note can never be dropped is pinned by a test against it.
pub(crate) fn selection(mandatory: &[String], picks: &[String]) -> Vec<String> {
    let mut loaded = mandatory.to_vec();
    for pick in picks {
        if !loaded.contains(pick) {
            loaded.push(pick.clone());
        }
    }
    loaded
}

/// One `brief_picks.jsonl` row, tagged `kind` exactly like the other Jev ledgers.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum BriefRow {
    /// One boot's pick: what was mandatory, offered and chosen, and what `act` would have loaded.
    /// `rounds` is how many questions were asked (a miss counts as the round it happened on); `missed`
    /// is whether a round got no usable answer — no opinion, or a choice outside the options.
    Pick {
        session_id: String,
        at: DateTime<Utc>,
        model: String,
        mandatory: Vec<String>,
        candidates: Vec<String>,
        picks: Vec<String>,
        would_load: Vec<String>,
        rounds: usize,
        missed: bool,
    },
    /// A watched item the colony really touched, at most once per item; `via` is the tool that used
    /// it, or `tool_result` when only the output named it.
    Used {
        session_id: String,
        at: DateTime<Utc>,
        item: String,
        via: String,
    },
}

/// Append one ledger row. A row is plain data, so serialising cannot fail; a failed append is a lost
/// measurement, logged like the other Jev ledgers' appends, never a failed colony.
async fn record(app: &Shared, row: &BriefRow) {
    let Ok(line) = serde_json::to_string(row) else { return };
    if let Err(e) = append_line(&app.brief_picks_file(), &line).await {
        app.storage_failed("append to the Jev brief-pick ledger", &e).await;
    }
}

/// How a use looks: a note when its guest path turns up in a tool's input or output; a pack when a
/// `Skill` call names it.
#[derive(Clone)]
enum Key {
    Path(String),
    Skill(String),
}

/// The match key for a label, derived from the label alone so a watch is armed from the same strings
/// the ledger carries.
fn key_of(label: &str) -> Option<Key> {
    if let Some(rest) = label.strip_prefix("note:") {
        let (scope, id) = rest.split_once('/')?;
        Some(Key::Path(note_path(scope, id)))
    } else {
        label.strip_prefix("skill:").map(|name| Key::Skill(name.to_string()))
    }
}

/// Per-session watch state (`Runtime.brief_pick`): the candidate and mandatory labels not yet seen
/// used. In memory only, like the other watches: a restart forgets it, so a use that would have
/// landed after it is simply not counted — a lost measurement, never a wrong one.
#[derive(Default)]
pub(crate) struct Watch {
    pending: Vec<(String, Key)>,
}

impl Watch {
    /// Arms the watch over these labels, candidate and mandatory together, so a mandatory item's own
    /// use is recorded too (grading ignores it, but the ledger stays honest about what was read).
    fn arm(&mut self, labels: &[String]) {
        self.pending = labels
            .iter()
            .filter_map(|label| key_of(label).map(|key| (label.clone(), key)))
            .collect();
    }

    /// The labels this event used, each spent on its first use so a repeat cannot inflate the count.
    fn used(&mut self, event: &Value) -> Vec<String> {
        let (text, skill) = match event["type"].as_str().unwrap_or_default() {
            "tool_call" => {
                let input = event.get("input").cloned().unwrap_or(Value::Null);
                let skill = (event["name"].as_str() == Some("Skill"))
                    .then(|| input.get("skill").and_then(Value::as_str).map(str::to_string))
                    .flatten();
                (Some(input.to_string()), skill)
            }
            "tool_result" => (event.get("output").and_then(Value::as_str).map(str::to_string), None),
            _ => (None, None),
        };
        let mut hits = Vec::new();
        self.pending.retain(|(label, key)| {
            let hit = match key {
                Key::Path(path) => text.as_deref().is_some_and(|text| text.contains(path.as_str())),
                Key::Skill(pack) => skill
                    .as_deref()
                    .is_some_and(|skill| skill == pack || skill.starts_with(&format!("{pack}:"))),
            };
            if hit {
                hits.push(label.clone());
            }
            !hit
        });
        hits
    }
}

/// The boot path's one call (#585). Spawns the pick off the boot path so it never delays or fails a
/// boot: disabled, no Jev key, or memory off all return immediately, and everything the spawned task
/// does is measurement. `plugin_names` are the configured packs, `labels` the task's issue labels.
pub(crate) fn start(app: &Shared, id: &str, enabled: bool, memory_on: bool, plugin_names: &[String], labels: &[String]) {
    if !enabled || !memory_on {
        return;
    }
    let Some(api_key) = crate::jev::api_key() else { return };
    let (app, id) = (app.clone(), id.to_string());
    let (plugin_names, labels) = (plugin_names.to_vec(), labels.to_vec());
    tokio::spawn(async move { run(app, id, api_key, plugin_names, labels).await });
}

/// Each pack's name and its on-disk size, for the candidate's `bytes`; an unresolvable pack is 0.
fn skill_sizes(app: &Shared, names: &[String]) -> Vec<(String, u64)> {
    names
        .iter()
        .map(|name| {
            let bytes = crate::plugins::resolve(&app.cfg, name)
                .ok()
                .map(|dir| dir_bytes(&dir))
                .unwrap_or(0);
            (name.clone(), bytes)
        })
        .collect()
}

/// A directory's total file size, following into subdirectories; an unreadable entry counts as zero.
fn dir_bytes(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => dir_bytes(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

/// The spawned body: build the candidates, arm the watch, ask Jev, and record the pick.
async fn run(app: Shared, id: String, api_key: String, plugin_names: Vec<String>, labels: Vec<String>) {
    // mem0 notes are materialized into a colony's session directory at a different path, so the
    // watch keys here would not match them; skip those colonies rather than grade them wrongly.
    if crate::memory::uses_mem0(&app).await {
        return;
    }
    let Some(s) = app.session(&id).await else { return };
    // A scope that fails to list contributes nothing: a lost candidate, never a failed measurement.
    let mut notes = Vec::new();
    for (scope, key) in [("global", String::new()), ("org", s.org.clone()), ("repo", s.repo.clone())] {
        if let Ok(mut scoped) = app.memory.notes(scope, &key).await {
            notes.append(&mut scoped);
        }
    }
    let (mandatory, candidates) = split(&notes, &skill_sizes(&app, &plugin_names));
    if mandatory.is_empty() && candidates.is_empty() {
        return;
    }
    // Armed before Jev is asked, so a colony already reading a note while the pick runs is still
    // graded. A use before this line is a lost measurement, never a wrong one.
    let watch: Vec<String> = mandatory
        .iter()
        .chain(candidates.iter().map(|candidate| &candidate.label))
        .cloned()
        .collect();
    app.runtime(&id).await.brief_pick.lock().await.arm(&watch);

    let (picks, rounds, missed) = pick(
        &crate::jev::JevClient::new(api_key),
        &s.issue_title,
        &labels,
        &mandatory,
        &candidates,
    )
    .await;
    let would_load = selection(&mandatory, &picks);
    record(
        &app,
        &BriefRow::Pick {
            session_id: id.clone(),
            at: Utc::now(),
            model: crate::jev::JEV_MODEL.to_string(),
            mandatory: mandatory.clone(),
            candidates: candidates.iter().map(|candidate| candidate.label.clone()).collect(),
            picks: picks.clone(),
            would_load: would_load.clone(),
            rounds,
            missed,
        },
    )
    .await;
    let missed_note = if missed { "; a round got no answer" } else { "" };
    app.session_log(
        &id,
        "info",
        format!(
            "jev brief (shadow): {} mandatory, {} offered, picked {} (act would load {}); {} round(s){missed_note}",
            mandatory.len(),
            candidates.len(),
            picks.len(),
            would_load.len(),
            rounds
        ),
    )
    .await;
}

/// Asks Jev up to [`MAX_PICKS`] single `choice` questions, each offering the candidates still on the
/// table plus `none`, with those candidates and the picks so far as the state. Stops at `none`, an
/// empty table, a failed call, or the cap. Returns the picks, the rounds asked, and whether a round
/// got no usable answer.
async fn pick(
    client: &crate::jev::JevClient,
    title: &str,
    labels: &[String],
    mandatory: &[String],
    candidates: &[Candidate],
) -> (Vec<String>, usize, bool) {
    let mut remaining: Vec<Candidate> = candidates.to_vec();
    let mut picks: Vec<String> = Vec::new();
    let mut rounds = 0;
    let mut missed = false;
    while picks.len() < MAX_PICKS && !remaining.is_empty() {
        rounds += 1;
        let options: Vec<String> = remaining
            .iter()
            .map(|candidate| candidate.label.clone())
            .chain([NONE.to_string()])
            .collect();
        let state =
            json!({ "title": title, "labels": &labels, "mandatory": &mandatory, "candidates": &remaining, "picks": &picks });
        match client.ask_choice(QUESTION_ID, &options, &state).await {
            None => {
                missed = true;
                break;
            }
            Some((choice, _)) if choice == NONE => break,
            Some((choice, _)) => {
                picks.push(choice.clone());
                remaining.retain(|candidate| candidate.label != choice);
            }
        }
    }
    (picks, rounds, missed)
}

/// The `tool_call`/`tool_result` dispatch (events.rs keeps it thin, like `loops.rs`): the first use of
/// a watched note or pack is spent on this event and appends one `used` row. Cheap when the watch is
/// empty — the common case for a colony that never ran a pick — by returning before any append.
pub(crate) async fn note_event(app: &Shared, id: &str, rt: &Arc<Runtime>, event: &Value) {
    let kind = event["type"].as_str().unwrap_or_default();
    if !matches!(kind, "tool_call" | "tool_result") {
        return;
    }
    let (hits, via) = {
        let mut watch = rt.brief_pick.lock().await;
        if watch.pending.is_empty() {
            return;
        }
        let via = match kind {
            "tool_call" => event["name"].as_str().unwrap_or("tool_call").to_string(),
            _ => "tool_result".to_string(),
        };
        (watch.used(event), via)
    };
    for item in hits {
        record(
            app,
            &BriefRow::Used {
                session_id: id.to_string(),
                at: Utc::now(),
                item,
                via: via.clone(),
            },
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::handle_agent_event;
    use crate::sessions::{SessionStatus, tests::app_with_colony};

    fn note(id: &str, tags: &[&str]) -> Note {
        Note {
            id: id.into(),
            scope: "repo".into(),
            key: "acme/repo".into(),
            title: format!("note {id}"),
            content: "a body".into(),
            kind: "convention".into(),
            confidence: None,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            created_at: chrono::Utc::now(),
            source: json!({}),
        }
    }

    fn candidate(label: &str) -> Candidate {
        Candidate {
            label: label.into(),
            title: label.into(),
            path: format!("/colonizer/memory/repo/notes/{label}.md"),
            bytes: 4,
            kind: "convention".into(),
        }
    }

    /// A pick body whose chosen option is `choice`, as the vendor would answer `brief_item`.
    fn answer(choice: &str) -> Value {
        json!({"model": "jev-1.13.0", "answers": {"brief_item": {"choice": choice, "confidence": 0.9}}})
    }

    fn client(base: String) -> crate::jev::JevClient {
        crate::jev::JevClient::with_base_url("test-key".into(), base)
    }

    /// The acceptance rule: a `house-rule` or `security` note is never offered as an option, and is in
    /// `selection`/`would_load` however few picks there are.
    #[test]
    fn a_mandatory_note_is_never_offered_and_never_dropped() {
        let notes = vec![
            note("rule", &["house-rule"]),
            note("secret", &["security"]),
            note("plain", &["style"]),
        ];
        let (mandatory, candidates) = split(&notes, &[("archify".into(), 10)]);
        assert_eq!(mandatory, ["note:repo/rule", "note:repo/secret"]);
        let labels: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["note:repo/plain", "skill:archify"], "mandatory is never a candidate");
        // Even picking nothing, the mandatory items are in what act would load; a pick follows them.
        assert_eq!(selection(&mandatory, &[]), ["note:repo/rule", "note:repo/secret"]);
        assert_eq!(
            selection(&mandatory, &["skill:archify".into()]),
            ["note:repo/rule", "note:repo/secret", "skill:archify"]
        );
    }

    /// Against the mock server: stop at `none`, stop at the cap, and miss on an out-of-options choice.
    #[tokio::test]
    async fn picking_stops_at_none_the_cap_and_a_miss() {
        let c = |label: &str| candidate(label);
        let base = crate::jev::tests::serve_scripted(vec![answer("c0"), answer("none"), answer("c1")]).await;
        let (picks, rounds, missed) = pick(&client(base), "a task", &[], &[], &[c("c0"), c("c1")]).await;
        assert_eq!(
            (picks.as_slice(), rounds, missed),
            (&["c0".to_string()][..], 2, false),
            "the round after none is never asked"
        );

        let script: Vec<Value> = (0..MAX_PICKS + 3).map(|i| answer(&format!("c{i}"))).collect();
        let pool: Vec<Candidate> = (0..MAX_PICKS + 3).map(|i| c(&format!("c{i}"))).collect();
        let base = crate::jev::tests::serve_scripted(script).await;
        let (picks, rounds, missed) = pick(&client(base), "a task", &[], &[], &pool).await;
        assert_eq!((picks.len(), rounds, missed), (MAX_PICKS, MAX_PICKS, false));

        let base = crate::jev::tests::serve_scripted(vec![answer("ghost")]).await;
        let (picks, rounds, missed) = pick(&client(base), "a task", &[], &[], &[c("c0")]).await;
        assert!(picks.is_empty(), "a choice outside the options is a miss, not a pick");
        assert_eq!((rounds, missed), (1, true));
    }

    /// The watcher records a `Read` of a watched note, a `tool_result` naming another, and a `Skill`
    /// call naming a pack — each once — and ignores an unwatched path.
    #[tokio::test]
    async fn the_watch_records_a_note_read_and_a_skill_use_once_each() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let rt = app.runtime("abc").await;
        rt.brief_pick
            .lock()
            .await
            .arm(&["note:repo/n1".into(), "note:repo/n2".into(), "skill:archify".into()]);
        let send = |seq: u64, body: Value| {
            let (app, rt) = (app.clone(), rt.clone());
            async move {
                let mut event = body;
                event["seq"] = json!(seq);
                handle_agent_event(&app, "abc", &rt, &event.to_string()).await;
            }
        };
        let read = |seq: u64, path: &str| json!({"type": "tool_call", "tool_call_id": format!("t{seq}"), "name": "Read", "input": {"file_path": path}});
        send(1, read(1, "/colonizer/memory/repo/notes/n1.md")).await;
        send(2, read(2, "/colonizer/memory/repo/notes/n1.md")).await; // again: not a second use
        send(
            3,
            json!({"type": "tool_result", "tool_call_id": "t3", "output": "found [/colonizer/memory/repo/notes/n2.md]"}),
        )
        .await;
        send(
            4,
            json!({"type": "tool_call", "tool_call_id": "t4", "name": "Skill", "input": {"skill": "archify:draw"}}),
        )
        .await;
        send(5, read(5, "/colonizer/memory/repo/notes/other.md")).await; // unwatched: nothing

        let rows: Vec<BriefRow> = std::fs::read_to_string(app.brief_picks_file())
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(matches!(&rows[0], BriefRow::Used { item, via, .. } if item == "note:repo/n1" && via == "Read"));
        assert!(matches!(&rows[1], BriefRow::Used { item, via, .. } if item == "note:repo/n2" && via == "tool_result"));
        assert!(matches!(&rows[2], BriefRow::Used { item, via, .. } if item == "skill:archify" && via == "Skill"));
        let _ = std::fs::remove_dir_all(root);
    }
}
