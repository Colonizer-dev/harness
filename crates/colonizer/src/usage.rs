//! Anonymous usage reporting: the batch that is sent, and the switch that says whether one may
//! ever leave the machine. The sender composes Cratefield's `module-telemetry` crate
//! (Cratefield/harness#413): the batch is built from live state, validated through the crate's
//! [`Batch::parse`] — the same grammar the collector on the other side parses — and posted to an
//! endpoint at most once per 24 hours, dropping the batch on any non-2xx. There is **no default
//! endpoint**: nothing is sent until `COLONIZER_TELEMETRY_ENDPOINT` names one, so an install that
//! never sets it never sends a byte. The mapping from what this harness knows to the payload's
//! closed event vocabulary is documented in docs/usage-data.md.
//!
//! The batch is shown whatever the switch says — at `GET /api/telemetry/usage`, at
//! `colonizer telemetry show`, and once on stderr at the first start — and it is the same value
//! the sender posts, so a user can always read exactly what is reported, before answering or
//! after.
//!
//! Reporting is on unless the user says no: `colonizer telemetry on|off` writes the answer straight
//! to `<config>/usage.json`, with no network and no running mothership needed, so switching off —
//! like switching on — is nothing but a file write.
//!
//! Everything in a batch comes from a closed vocabulary, enforced by the test at the bottom of this
//! file: counts and durations as buckets, settings as schema-declared names without values, boot
//! phases and failures as labels the harness itself defines. The install id rotates every
//! [`consent::ROTATION_DAYS`] days, Cratefield's bound, so no id accumulates for longer than that.

use crate::{
    Shared, client_error,
    config::{ModulesConfig, setting_str},
    events::AGENT_FAILED,
    modules::{AgentModule, KINDS, schema_for},
    presets,
    sessions::{self, Session, SessionStatus},
    telemetry, util,
};
use anyhow::{Context, Result, bail};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use cratefield_module_telemetry::{consent, payload};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{sync::Mutex, time::sleep};

/// The payload type the batch speaks: Cratefield's grammar, parsed, never hand-assembled on the
/// way out. Constructed only by [`build`], which goes through [`payload::Batch::parse`].
pub use payload::Batch;

/// The env var that names the collector, the full URL of its ingest route. There is no default:
/// unset, it means nothing is ever sent — the switch and the batch still work, shown locally.
const ENDPOINT_ENV: &str = "COLONIZER_TELEMETRY_ENDPOINT";
/// The file the answer is kept in, beside the live map's telemetry.json in the config dir.
const CHOICE_FILE: &str = "usage.json";
/// The file the last batch built is kept in, so `colonizer telemetry show` in another process can
/// print its exact bytes.
const LAST_BATCH_FILE: &str = "usage-last.json";
/// The file the last successful send is recorded in, which the 24-hour cadence compares.
const SENT_FILE: &str = "usage-sent.json";
/// One send at most per 24 hours (Cratefield's client contract: flush at most once per run or per
/// 24 hours, whichever is sooner — a run shorter than a day sends at most once).
const SEND_EVERY_SECS: i64 = 24 * 60 * 60;
/// How often the sender's loop looks at whether a send is due.
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);
/// How long after startup the sender's loop first looks: shortly after, not during — boot has
/// enough to do, and a batch built mid-boot would describe half a state anyway.
const STARTUP_DELAY: Duration = Duration::from_secs(60);

/// The boot phases `boot_inner` marks, in the order a boot runs them. A phase name outside this set
/// (a future version, a hand-edited sessions.json) is dropped rather than sent.
const BOOT_PHASES: [&str; 8] = [
    "issue",
    "git",
    "providers",
    "mesh-start",
    "image-pull",
    "vm-boot",
    "mesh-join",
    "agentd",
];

/// The attention reason autopilot sets when it holds a colony back from publishing (sessions.rs).
/// `AGENT_FAILED` (events.rs) is the sibling setter-owned hold for a runner that never started:
/// the watchdog neither sets nor clears either of them.
const AUTOPILOT_HELD: &str = "autopilot_held";

/// The fixed messages sessions.rs records verbatim, each with its closed label. Matched whole: any
/// other text — including a colony's own error string, which is free text — names no kind at all.
const FIXED_ERRORS: &[(&str, &str)] = &[
    (sessions::AGENTD_NOT_READY, "agentd_not_ready"),
    (sessions::VM_GONE_AFTER_RESTART, "harness_restarted"),
    (sessions::VM_STOPPED_EARLY, "vm_stopped"),
    (sessions::PUBLISH_LOST_TO_RESTART, "publish_interrupted"),
    (sessions::PUBLISH_VM_UNCONFIRMED, "publish_unconfirmed"),
];

/// The one module name a batch declares, in Cratefield's `modules` list: the thing being reported
/// on is this mothership, and there is nothing else it composes that a collector would know.
const MODULE: &str = "mothership";

/// The install value that stands in while there is no usage id — reporting off, or an environment
/// block — because the grammar requires 32 hex characters either way. It marks a batch that will
/// never be sent, and [`Usage::send`] refuses to post one, so it never reaches a collector.
fn no_install() -> String {
    "0".repeat(32)
}

/// The usage id as the payload's `install`: the UUID without its dashes — 32 lowercase hex, the
/// shape [`consent::install_id_is_valid`] accepts. An id that is not that shape (a hand-edited
/// usage.json) stands in as [`no_install`] rather than being sent broken.
fn install_of(usage_id: Option<String>) -> String {
    let Some(id) = usage_id else {
        return no_install();
    };
    let hex = id.replace('-', "").to_ascii_lowercase();
    if consent::install_id_is_valid(&hex) {
        hex
    } else {
        no_install()
    }
}

/// The closed `client` shape the grammar asks for, from the closed platform string the live map
/// sends: `linux-x86_64` becomes the pair (linux, x86-64), `darwin-arm64` (macos, aarch64), and
/// anything else (other, other).
fn client_shape(platform: &str) -> (&'static str, &'static str) {
    match platform {
        "linux-x86_64" => ("linux", "x86-64"),
        "darwin-arm64" => ("macos", "aarch64"),
        _ => ("other", "other"),
    }
}

/// The version the payload carries: a plain release triple, because the grammar rejects a
/// pre-release tag or build metadata rather than trim it. A version that is not a triple rounds
/// down to `0.0.0`, the honest reading of "not a release".
fn release_triple(version: &'static str) -> String {
    payload::Version::parse(version).map_or_else(|| "0.0.0".to_owned(), |v| v.to_string())
}

/// One counted observation, the shape every field of the old flat batch maps to: the label rides in
/// the event's name, and the outcome, error class, duration and count stay at the grammar's neutral
/// values — a usage batch is a set of observations, not runs. docs/usage-data.md has the mapping.
fn counted(name: String) -> Value {
    json!({"name": name, "outcome": "ok", "error": "none", "duration": "unknown", "count": 1})
}

/// Buckets a count so no exact number leaves the machine: 0, 1, then doubling bands, then 64 or more.
fn bucket_count(n: usize) -> &'static str {
    match n {
        0 => "0",
        1 => "1",
        2..=3 => "2-3",
        4..=7 => "4-7",
        8..=15 => "8-15",
        16..=63 => "16-63",
        _ => "64+",
    }
}

/// Buckets a duration in milliseconds: under a second, then bands at 1, 2, 5, 15 and 60 seconds.
fn bucket_ms(ms: u64) -> &'static str {
    match ms {
        0..=999 => "<1s",
        1_000..=1_999 => "1-2s",
        2_000..=4_999 => "2-5s",
        5_000..=14_999 => "5-15s",
        15_000..=59_999 => "15-60s",
        _ => "60s+",
    }
}

/// The sandbox stack as a closed label: `auto` when detection is in use — reported as configured,
/// never resolved to the stack it would fall back to, since showing whether detection is in use is
/// the whole point of the field — a preset id from the table, or `unknown` when a hand-edited
/// modules.json names a preset the harness has never heard of.
fn preset_label(preset: &str) -> &'static str {
    match presets::find(preset) {
        Some(found) => found.id,
        None if preset == presets::AUTO => presets::AUTO,
        None if preset == presets::CUSTOM => presets::CUSTOM,
        None => "unknown",
    }
}

/// The image the resolved stack would boot on its own: the preset's image when the stack is a known
/// preset, otherwise the sandbox schema's default. Compared against, never sent.
fn image_baseline(preset: &str, schema: &Value) -> String {
    // `presets::defaults` and not `presets::find(..).image`: a preset's image is pinned by digest
    // from vendor.lock before a colony boots, so the bare tag never equals what `colony_image`
    // resolves and every default install would otherwise report its image as changed.
    // `resolved` first: on an install left on `auto` there is no per-colony answer to compare
    // against, so the baseline is the fallback's rather than whatever the schema default is.
    match presets::defaults(presets::resolved(preset))["image"].as_str() {
        Some(image) => image.to_string(),
        None => schema["properties"]["image"]["default"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    }
}

/// `<kind>.<key>` for every setting an install carries that its module's schema declares, sorted.
///
/// `PUT /api/modules/{kind}` drops undeclared keys, but `ModulesConfig::load` does not re-validate,
/// so a hand-edited modules.json can carry anything. The keys are intersected with the schema here,
/// and only the names are sent — a setting's value may be free text.
fn settings_set(modules: &ModulesConfig, agents: &[AgentModule]) -> Vec<String> {
    let mut names = BTreeSet::new();
    for kind in KINDS {
        let Some(choice) = modules.get(kind) else { continue };
        let schema = schema_for(kind, &choice.provider, agents);
        let Some(properties) = schema["properties"].as_object() else {
            continue;
        };
        for key in choice.settings.keys() {
            if properties.contains_key(key) {
                names.insert(format!("{kind}.{key}"));
            }
        }
    }
    names.into_iter().collect()
}

/// Where boot time went: for each known phase the harness marks, the median across the colonies that
/// have booted (the upper of the two middles when there is an even number), bucketed. Phases the
/// harness does not mark are dropped rather than sent.
fn boot_ms(sessions: &[Session]) -> Vec<(&'static str, &'static str)> {
    let mut samples: Vec<(&str, Vec<u64>)> = BOOT_PHASES.iter().map(|phase| (*phase, Vec::new())).collect();
    for session in sessions {
        // A breakdown without `total_ms` is a boot still under way or one that stopped part way, with
        // only its early phases. Counting those would put different colonies behind each phase's
        // median, so only finished boots are sampled.
        let Some(timing) = session.boot_timing.as_ref().filter(|t| t.get("total_ms").is_some()) else {
            continue;
        };
        let Some(phases) = timing["phases"].as_array() else {
            continue;
        };
        for phase in phases {
            let (Some(name), Some(ms)) = (phase["name"].as_str(), phase["ms"].as_u64()) else {
                continue;
            };
            if let Some(entry) = samples.iter_mut().find(|(known, _)| *known == name) {
                entry.1.push(ms);
            }
        }
    }
    samples
        .into_iter()
        .filter(|(_, samples)| !samples.is_empty())
        .map(|(phase, mut samples)| {
            samples.sort_unstable();
            (phase, bucket_ms(samples[samples.len() / 2]))
        })
        .collect()
}

/// A colony's attention reason, when it is one the harness set: the watchdog's reasons, autopilot's
/// hold, the gateway's model/provider error, or the runner-never-started hold. Anything else in the
/// attention blob — a hand-edited sessions.json can carry anything — is ignored rather than sent.
fn attention_reason(session: &Session) -> Option<&'static str> {
    let reason = session.attention.as_ref().and_then(|a| a["reason"].as_str())?;
    if reason == AUTOPILOT_HELD {
        return Some(AUTOPILOT_HELD);
    }
    if reason == crate::gateway::MODEL_ERROR_REASON {
        return Some(crate::gateway::MODEL_ERROR_REASON);
    }
    if reason == AGENT_FAILED {
        return Some(AGENT_FAILED);
    }
    crate::watchdog::WATCHDOG_REASONS
        .iter()
        .find(|known| **known == reason)
        .copied()
}

/// The label for a fixed harness message, or nothing: a failure the harness did not name itself
/// contributes no kind, whatever its text says.
fn fixed_error_kind(message: &str) -> Option<&'static str> {
    FIXED_ERRORS.iter().find(|(text, _)| *text == message).map(|(_, kind)| *kind)
}

/// How failures and attention are distributed, as closed labels. A colony's own error text
/// (`Session.error`) is free text and never becomes a kind, so this map can hold fewer kinds than
/// `colonies.terminal.failed` covers — the unnamed failures are simply not named here.
fn error_kinds(sessions: &[Session]) -> BTreeMap<&'static str, &'static str> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for session in sessions {
        if let Some(kind) = session.error.as_deref().and_then(fixed_error_kind) {
            *counts.entry(kind).or_default() += 1;
        }
        if let Some(kind) = attention_reason(session) {
            *counts.entry(kind).or_default() += 1;
        }
    }
    counts.into_iter().map(|(kind, count)| (kind, bucket_count(count))).collect()
}

fn terminal_count(sessions: &[Session], status: SessionStatus) -> &'static str {
    bucket_count(sessions.iter().filter(|s| s.status == status).count())
}

/// The pure half of [`build`]: the same plain values, mapped into Cratefield's payload JSON, with
/// the declared vocabulary that names exactly the events it carries — so [`payload::Batch::parse`]
/// can hold the result against the grammar the collector parses. docs/usage-data.md documents the
/// mapping field by field.
fn cratefield_value(
    usage_id: Option<String>,
    sessions: &[Session],
    modules: &ModulesConfig,
    agents: &[AgentModule],
    providers: usize,
) -> (Value, payload::Vocabulary) {
    let sandbox_schema = schema_for("sandbox", &modules.sandbox.provider, agents);
    let preset = setting_str(&modules.sandbox, &sandbox_schema, "preset");
    let held = sessions
        .iter()
        .filter(|s| attention_reason(s) == Some(AUTOPILOT_HELD))
        .count();
    // Suspended colonies excluded: their microVMs are down — that is what the suspension freed
    // (issue #562).
    let parallel_now = bucket_count(
        sessions
            .iter()
            .filter(|s| s.status.is_live() && s.suspended.is_none())
            .count(),
    );
    let preset_label = preset_label(&preset);
    // Only the comparison is sent: the image string itself is user free text. The configured
    // preset, not any colony's detected one — telemetry reports what the install does, and this
    // code has no repository in hand to detect from.
    let image_changed = sessions::colony_image(agents, modules, &preset) != image_baseline(&preset, &sandbox_schema);
    let settings = settings_set(modules, agents);
    let boots = boot_ms(sessions);
    let kinds = error_kinds(sessions);

    // The fixed order below is the payload's order, so the bytes a batch serializes to are stable
    // and `usage-last.json` is not rewritten by a reorder.
    let mut events = vec![
        counted(format!("colonies.parallel_now.{parallel_now}")),
        counted(format!(
            "colonies.pr_opened.{}",
            terminal_count(sessions, SessionStatus::PrOpened)
        )),
        counted(format!(
            "colonies.no_changes.{}",
            terminal_count(sessions, SessionStatus::NoChanges)
        )),
        counted(format!(
            "colonies.stopped.{}",
            terminal_count(sessions, SessionStatus::Stopped)
        )),
        counted(format!("colonies.failed.{}", terminal_count(sessions, SessionStatus::Failed))),
        counted(format!("sandbox.preset.{preset_label}")),
        counted(format!("sandbox.image_changed.{image_changed}")),
        counted(format!("autopilot.enabled.{}", sessions::autopilot_default(agents, modules))),
        counted(format!("autopilot.held.{}", bucket_count(held))),
    ];
    for name in &settings {
        events.push(counted(format!("setting.{name}")));
    }
    for (phase, bucket) in &boots {
        events.push(counted(format!("boot.{phase}.{bucket}")));
    }
    events.push(counted(format!("providers.{}", bucket_count(providers))));
    for (kind, bucket) in &kinds {
        events.push(counted(format!("error.{kind}.{bucket}")));
    }

    let value = json!({
        "schema": payload::SCHEMA,
        "install": install_of(usage_id),
        "client": {
            "kind": "server",
            "version": release_triple(env!("CARGO_PKG_VERSION")),
            "platform": client_shape(telemetry::platform()).0,
            "arch": client_shape(telemetry::platform()).1,
        },
        "modules": [MODULE],
        "events": events,
    });
    let vocabulary = payload::Vocabulary {
        events: value["events"]
            .as_array()
            .expect("events is a list we just built")
            .iter()
            .map(|event| event["name"].as_str().expect("a counted event names itself").to_owned())
            .collect(),
        modules: vec![MODULE.to_owned()],
        max_events: payload::MAX_EVENTS_PER_BATCH,
    };
    (value, vocabulary)
}

/// Builds a batch from plain values, so tests can build one from fixtures without an `App`. The
/// result has been through [`payload::Batch::parse`], so the payload a sender would transmit is
/// valid by construction under the grammar the collector parses — a batch that could not parse is
/// a bug the test at the bottom of this file holds the line against, not something to show a user.
fn build(
    usage_id: Option<String>,
    sessions: &[Session],
    modules: &ModulesConfig,
    agents: &[AgentModule],
    providers: usize,
) -> Batch {
    let (value, vocabulary) = cratefield_value(usage_id, sessions, modules, agents, providers);
    payload::Batch::parse(&value, &vocabulary)
        .expect("a batch built from the closed vocabularies above is inside the declared grammar")
}

/// Gathers a batch from live state. This is the one function both the API and the sender call, which
/// is why the API shows exactly what is sent. Each batch built is also kept in `usage-last.json`,
/// for `colonizer telemetry show` in another process.
pub async fn batch(app: &Shared) -> Batch {
    // The id rides only on a batch that could actually be sent: while the switch is off, or the
    // environment keeps it off, there is nothing here for a dataset to join on.
    let usage_id = app.usage.batch_id().await;
    let sessions = app.sessions.read().await;
    let modules = app.modules.read().await.clone();
    let batch = build(usage_id, &sessions, &modules, &app.agents, app.providers().len());
    drop(sessions);
    keep_last(&app.usage.last, &batch);
    batch
}

/// Keeps the bytes of the last batch built in `usage-last.json`, so `colonizer telemetry show` in
/// another process can print them verbatim without asking a running mothership. Written only when
/// the bytes change, so a Settings page polling the API does not rewrite the file constantly; a
/// failure here is tolerated, the file being a copy rather than the record. Returns whether it wrote.
fn keep_last(path: &Path, batch: &Batch) -> bool {
    let mut bytes = match serde_json::to_vec_pretty(batch) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    bytes.push(b'\n');
    if std::fs::read(path).is_ok_and(|kept| kept == bytes) {
        return false;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    util::write_private(path, &bytes).is_ok()
}

/// The user's answer, kept in `<config>/usage.json`, following the live map's `Choice` in telemetry.rs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    /// `Some(false)` once the user has said no. `None` — never answered — and `Some(true)` both mean
    /// reporting is on: the default is on, and only a no turns it off.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Random per on-period: created the first time a batch is built, or when the switch is turned
    /// on, and forgotten when reporting is switched off, so each on-period is a fresh id.
    /// Deliberately not the live map's `install_id` — a different file and always a different value,
    /// so the two datasets cannot be joined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_id: Option<String>,
    /// When the id was minted, so it can rotate every [`consent::ROTATION_DAYS`] days. An id with no
    /// recorded birthday — a file written before rotation was kept — is treated as overdue and
    /// rotated once, then kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_id_minted_at: Option<DateTime<Utc>>,
    /// Set once the first start has shown the batch on stderr, so later starts are quiet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notice_shown: bool,
}

impl Choice {
    fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        util::write_private(path, &serde_json::to_vec_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }
}

/// The last successful send, kept in `<config>/usage-sent.json`: what the 24-hour cadence compares.
/// Unreadable or absent means never sent, which is also what a fresh install reads.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Sent {
    last_sent_at: DateTime<Utc>,
}

impl Sent {
    fn load(path: &Path) -> Option<Self> {
        std::fs::read(path).ok().and_then(|data| serde_json::from_slice(&data).ok())
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        util::write_private(path, &serde_json::to_vec_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }
}

/// The usage switch, the files it is kept in, and the sender. The answer is re-read from the file
/// before it is consulted, so a choice written by another process takes effect without a restart,
/// and rewritten when the user answers.
pub struct Usage {
    path: PathBuf,
    /// Where the last batch built is kept, for `colonizer telemetry show` in another process.
    last: PathBuf,
    /// Where the last successful send is recorded, for the 24-hour cadence.
    sent: PathBuf,
    /// The collector to post to, from `COLONIZER_TELEMETRY_ENDPOINT`. `None` — the default — means
    /// nothing is ever sent, whatever the switch says.
    endpoint: Option<String>,
    blocked: Option<&'static str>,
    client: reqwest::Client,
    choice: Mutex<Choice>,
    /// The last successful send, in memory as well as in `usage-sent.json`: when the file's write
    /// fails, the memory keeps the cadence honest, so a sent batch is not re-sent on the next tick.
    last_sent: Mutex<Option<DateTime<Utc>>>,
}

impl Usage {
    pub fn new(config_dir: &Path) -> Result<Self> {
        let endpoint = util::env_nonempty(ENDPOINT_ENV);
        Self::with(config_dir.join(CHOICE_FILE), endpoint, disabled_by_env())
    }

    fn with(path: PathBuf, endpoint: Option<String>, blocked: Option<&'static str>) -> Result<Self> {
        let client = reqwest::Client::builder()
            // Short timeouts: a collector that answers slowly must not hold the loop, and the next
            // batch is a day away anyway.
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            last: path.with_file_name(LAST_BATCH_FILE),
            sent: path.with_file_name(SENT_FILE),
            choice: Mutex::new(Choice::load(&path)),
            last_sent: Mutex::new(None),
            path,
            endpoint: endpoint.map(|e| e.trim_end_matches('/').to_string()),
            blocked,
            client,
        })
    }

    /// Re-reads the answer from disk, so a choice written by another process — `colonizer telemetry
    /// off` in a terminal, while the mothership runs — is honoured without a restart. The file is a
    /// few lines, so this is cheap; if it cannot be read, the copy in memory stands in.
    async fn reload(&self) {
        let fresh = std::fs::read(&self.path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok());
        if let Some(fresh) = fresh {
            *self.choice.lock().await = fresh;
        }
    }

    /// Whether a batch could ever be sent from this mothership: on unless the user has said no or
    /// the environment says no. The answer is re-read from disk first, so it is the freshest one.
    async fn active(&self) -> bool {
        self.reload().await;
        self.blocked.is_none() && self.choice.lock().await.enabled != Some(false)
    }

    /// The id the next batch carries: none while reporting is off (the environment or the user), and
    /// otherwise the kept id — minted and persisted here on the very first batch, so an id exists for
    /// the whole on-period whatever switched reporting on — and rotated once it is
    /// [`consent::ROTATION_DAYS`] days old, so no id accumulates longer than that.
    async fn batch_id(&self) -> Option<String> {
        if self.blocked.is_some() {
            return None;
        }
        self.reload().await;
        let mut choice = self.choice.lock().await;
        if choice.enabled == Some(false) {
            return None;
        }
        let overdue = match (choice.usage_id.as_deref(), choice.usage_id_minted_at) {
            (Some(_), Some(minted)) => (Utc::now() - minted).num_days() >= i64::from(consent::ROTATION_DAYS),
            // An id with no birthday is one this code did not mint; it gets a fresh one.
            (Some(_), None) => true,
            (None, _) => true,
        };
        if !overdue {
            return choice.usage_id.clone();
        }
        let id = uuid::Uuid::new_v4().to_string();
        choice.usage_id = Some(id.clone());
        choice.usage_id_minted_at = Some(Utc::now());
        // If this write fails the batch is still built; the id just would not survive a restart.
        let _ = choice.save(&self.path);
        Some(id)
    }

    /// Switches usage reporting on or off and saves the answer. Switching on keeps or creates the
    /// usage id; switching off forgets it and its birthday, so the next period cannot be joined to
    /// this one. This touches no network and needs no round-trip: writing the file here is the whole
    /// of the operation, and the sender, reading the file before every send, stops with it.
    async fn set(&self, enabled: bool) -> Result<()> {
        if let Some(variable) = self.blocked {
            bail!("usage reporting is kept off by {variable} in the mothership's environment");
        }
        self.reload().await;
        let mut choice = self.choice.lock().await;
        match enabled {
            true if choice.usage_id.is_none() => {
                choice.usage_id = Some(uuid::Uuid::new_v4().to_string());
                choice.usage_id_minted_at = Some(Utc::now());
            }
            true => {}
            false => {
                choice.usage_id = None;
                choice.usage_id_minted_at = None;
            }
        }
        choice.enabled = Some(enabled);
        choice.save(&self.path)
    }

    /// Whether the mothership owes a send right now: the switch on, an endpoint named, and the last
    /// successful send — if there was one — older than 24 hours. The last send is the later of the
    /// file's record and this life's memory, so a success whose record could not be written is not
    /// re-sent on the next tick either.
    async fn due(&self) -> bool {
        if !self.active().await || self.endpoint.is_none() {
            return false;
        }
        let last = Sent::load(&self.sent).map(|sent| sent.last_sent_at);
        let last = match *self.last_sent.lock().await {
            Some(memory) => Some(memory.max(last.unwrap_or(memory))),
            None => last,
        };
        match last {
            Some(last) => (Utc::now() - last).num_seconds() >= SEND_EVERY_SECS,
            None => true,
        }
    }

    /// Posts the batch — the same value the API shows — to the configured collector, and records the
    /// time on any 2xx. Both gates are checked here, not only by the loop, so nothing can send with
    /// the switch off, without an endpoint, or with the placeholder install. Anything else is a
    /// dropped batch: the failure is one stderr line naming no payload, and the next attempt waits
    /// for the next due tick, because a sender that retries is an outage amplifier (Cratefield's
    /// client contract: lost counts are the correct loss). Returns whether it sent.
    async fn send(&self, batch: &Batch) -> bool {
        if !self.active().await || batch.install == no_install() {
            return false;
        }
        let Some(endpoint) = self.endpoint.as_deref() else {
            return false;
        };
        match self.client.post(endpoint).json(batch).send().await {
            Ok(response) if response.status().is_success() => {
                let now = Utc::now();
                // The memory first, whatever happens to the file: the cadence reads both, so the
                // send is not repeated on the next tick when the record cannot be written.
                *self.last_sent.lock().await = Some(now);
                if let Err(e) = (Sent { last_sent_at: now }).save(&self.sent) {
                    eprintln!("could not record the usage send: {e:#}");
                }
                true
            }
            Ok(response) => {
                eprintln!("the usage batch was dropped: the collector answered {}", response.status());
                false
            }
            Err(e) => {
                eprintln!("the usage batch was dropped: {e}");
                false
            }
        }
    }

    /// The first-run notice: on the first start where nobody has answered and no environment switch
    /// blocks reporting, show the exact batch on stderr — on the record before anything could ever be
    /// sent — and record that it was shown, so later starts are quiet. Returns whether it printed. If
    /// the record's write fails, the next start shows the notice once more, which is the right
    /// failure mode.
    async fn show_notice(&self, batch: &Batch) -> Result<bool> {
        if self.blocked.is_some() {
            return Ok(false);
        }
        self.reload().await;
        {
            let choice = self.choice.lock().await;
            if choice.enabled.is_some() || choice.notice_shown {
                return Ok(false);
            }
        }
        eprintln!(
            "Anonymous usage reporting is on: the mothership reports counts and bucket labels only, and \
             nothing identifying — no repository, issue or worktree names, no paths, no agent output. This \
             is the exact batch it would send:\n{}\nNothing is sent unless {} names a collector, and then \
             at most one batch a day. Turn it off with `colonizer telemetry off`; the environment \
             variables COLONIZER_TELEMETRY, DO_NOT_TRACK and CI also keep it off.",
            serde_json::to_string_pretty(batch)?,
            ENDPOINT_ENV,
        );
        let mut choice = self.choice.lock().await;
        choice.notice_shown = true;
        let _ = choice.save(&self.path);
        Ok(true)
    }
}

/// The environment switches that keep usage reporting off whatever the stored answer says. Env beats
/// the file, so a machine nobody answers questions on can be locked down in one place.
fn disabled_by_env() -> Option<&'static str> {
    disabled_by_env_values(
        std::env::var("COLONIZER_TELEMETRY").ok().as_deref(),
        std::env::var("DO_NOT_TRACK").ok().as_deref(),
        std::env::var("CI").ok().as_deref(),
    )
}

/// The pure half of [`disabled_by_env`], so the switches can be tested without touching the process
/// environment. First match wins.
fn disabled_by_env_values(
    colonizer_telemetry: Option<&str>,
    do_not_track: Option<&str>,
    ci: Option<&str>,
) -> Option<&'static str> {
    // The app's own switch, read the same way the live map reads it.
    if telemetry::colonizer_telemetry_off(colonizer_telemetry) {
        return Some("COLONIZER_TELEMETRY");
    }
    // The same reading of DO_NOT_TRACK the live map uses (https://consoledonottrack.com): set to
    // anything except empty, `0` or `false`.
    if do_not_track.is_some_and(|v| !matches!(v.trim(), "" | "0" | "false")) {
        return Some("DO_NOT_TRACK");
    }
    // Scope this switch to usage only: reporting is on unless said no of, and a CI machine nobody is
    // sitting at should stay quiet without anyone having thought about it.
    if ci.is_some_and(|v| v.trim().eq_ignore_ascii_case("true")) {
        return Some("CI");
    }
    None
}

/// The body of `GET /api/telemetry/usage`, and of a successful `PUT`. `batch` is exactly what the
/// sender transmits — built by the same [`batch`] function, not a re-derivation.
#[derive(Debug, Serialize, PartialEq)]
pub struct Status {
    pub enabled: bool,
    /// The environment switch holding it off (`COLONIZER_TELEMETRY`, `DO_NOT_TRACK` or `CI`), named
    /// the same as the live map's `blocked_by`.
    pub blocked_by: Option<&'static str>,
    /// The payload schema the batch speaks: Cratefield's [`payload::SCHEMA`], the same value as
    /// `batch.schema`.
    pub payload_version: u32,
    pub batch: Batch,
}

async fn view(app: &Shared) -> Status {
    Status {
        enabled: app.usage.active().await,
        blocked_by: app.usage.blocked,
        payload_version: payload::SCHEMA,
        batch: batch(app).await,
    }
}

/// `GET /api/telemetry/usage` — the switch and the batch that would be sent, shown while the switch is
/// off too, so it can be read before deciding.
pub async fn status(State(app): State<Shared>) -> Json<Status> {
    Json(view(&app).await)
}

#[derive(Deserialize)]
pub struct SetRequest {
    enabled: bool,
}

/// `PUT /api/telemetry/usage` — `{"enabled": true|false}`
pub async fn put(State(app): State<Shared>, Json(body): Json<SetRequest>) -> crate::ApiResult<Status> {
    if app.usage.blocked.is_some() {
        return Err(client_error(
            StatusCode::CONFLICT,
            "usage reporting is kept off by the mothership's environment",
        ));
    }
    app.usage.set(body.enabled).await?;
    Ok(Json(view(&app).await))
}

/// Builds a batch — which also mints the usage id on the first one and keeps the batch for
/// `telemetry show` — then prints the first-run notice with it, if this install still owes one.
/// Every start runs this; the answer's file makes later starts quiet.
pub async fn first_run_notice(app: &Shared) {
    let batch = batch(app).await;
    let _ = app.usage.show_notice(&batch).await;
}

/// `colonizer telemetry show`: print the last batch built — its exact bytes, kept by [`keep_last`] —
/// to stdout, and nothing else. With no batch kept yet (a fresh install, or no mothership started),
/// the empty batch is built instead, so the command always answers the only question it is asked:
/// what exactly would be sent? Anything that is not the batch goes to stderr.
pub fn cli_show(config_dir: &Path) -> Result<()> {
    let kept = config_dir.join(LAST_BATCH_FILE);
    if let Ok(bytes) = std::fs::read(&kept) {
        use std::io::Write as _;
        return std::io::stdout().write_all(&bytes).context("writing to stdout");
    }
    eprintln!("no batch has been built yet; this is the empty batch a fresh install would send");
    let choice = Choice::load(&config_dir.join(CHOICE_FILE));
    let batch = build(choice.usage_id, &[], &ModulesConfig::default(), &[], 0);
    println!("{}", serde_json::to_string_pretty(&batch)?);
    Ok(())
}

/// `colonizer telemetry on|off`: write the answer straight to the file, touching no network and
/// needing no running mothership — the mothership re-reads the file before it consults the answer,
/// so this takes effect in one that is already running. One line of confirmation on stderr, and a
/// note when an environment switch is in force: the choice is still recorded, but it will not be
/// what decides.
pub fn cli_set(config_dir: &Path, enabled: bool) -> Result<()> {
    let path = config_dir.join(CHOICE_FILE);
    let mut choice = Choice::load(&path);
    choice.enabled = Some(enabled);
    match enabled {
        true if choice.usage_id.is_none() => {
            choice.usage_id = Some(uuid::Uuid::new_v4().to_string());
            choice.usage_id_minted_at = Some(Utc::now());
        }
        true => {}
        false => {
            choice.usage_id = None;
            choice.usage_id_minted_at = None;
        }
    }
    choice.save(&path)?;
    match (enabled, disabled_by_env()) {
        (true, Some(variable)) => eprintln!("usage reporting is on, but {variable} in this environment keeps it off"),
        (true, None) => eprintln!("usage reporting is on"),
        (false, Some(variable)) => {
            eprintln!("usage reporting is off; {variable} in this environment would have kept it off anyway")
        }
        (false, None) => eprintln!("usage reporting is off"),
    }
    Ok(())
}

/// The sender's loop: shortly after startup, then once an hour, send the batch if it is due — the
/// switch on, an endpoint named, and the last successful send older than 24 hours. A batch that
/// cannot be sent is dropped, not queued: nothing accumulates anywhere.
pub async fn run(app: Shared) {
    sleep(STARTUP_DELAY).await;
    loop {
        if app.usage.due().await {
            let batch = batch(&app).await;
            app.usage.send(&batch).await;
        }
        sleep(CHECK_EVERY).await;
    }
}

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &crate::Shared) {
    tokio::spawn(run(app.clone()));
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/telemetry/usage", routing::get(status).put(put))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Provider;
    use axum::{Router, extract::State, routing::post};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// A session full of the things a real one carries: names, paths and free text that must never
    /// reach a batch.
    fn session(status: SessionStatus) -> Session {
        let now = Utc::now();
        Session {
            id: "a1b2c3d4".into(),
            repo: "acme-corp/secret-project".into(),
            org: "acme-corp".into(),
            issue: Some(42),
            issue_title: "Fix the login page for customer accounts".into(),
            instructions: "follow our internal style guide at /home/me/STYLE.md".into(),
            status,
            branch: "colonizer/issue-42-a1b2c3d4".into(),
            base: Some("main".into()),
            parent: None,
            stack: false,
            stack_fork: None,
            origin: None,
            launched_by_token: None,
            worktree: "/home/me/.local/share/colonizer/worktrees/acme-corp/secret-project/issue-42-a1b2c3d4".into(),
            git_admin_dir: Some("/home/me/.local/share/colonizer/repos/git-admin".into()),
            sandbox: "colonizer-a1b2c3d4".into(),
            // Added on main while this branch was open; a batch must stay blind to both.
            publish_stage: None,
            publishing_holds_slot: false,
            needs_rebase: false,
            rebase_orphaned: false,
            unseen_failure: false,
            queued_behind: None,
            claim_wait: false,
            verify: None,
            verification: None,
            app_slot: None,
            boot_attempt_started_at: None,
            mesh: None,
            local_port: None,
            agent: "claude-code".into(),
            autopilot: true,
            autofix: None,
            automerge: None,
            fix_for: None,
            pr_url: Some("https://github.com/acme-corp/secret-project/pull/7".into()),
            merged_at: None,
            pr_opened_at: None,
            changed_paths: Vec::new(),
            ci_state: None,
            summary: None,
            error: None,
            cost_usd: Some(0.42),
            routed_cost_usd: None,
            routed_tokens: None,
            host_disk_bytes: None,
            model_usage: None,
            model_tier: None,
            model_override: None,
            subagent_model_override: None,
            claude_account: None,
            model_routing: None,
            allowed_providers: None,
            allowed_models: None,
            sensitivity: None,
            cleaned_up: false,
            keep_worktree: false,
            attention: None,
            suspended: None,
            parked: None,
            agent_session: None,
            pending_answer: None,
            prewarm: None,
            supply_chain: None,
            superseded: None,
            was_suspended: false,
            last_activity_at: Some(now),
            boot_timing: None,
            boot_cpus: None,
            boot_memory: None,
            boot_image: None,
            failure_class: None,
            boot_retries: 0,
            retry_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn agent() -> AgentModule {
        AgentModule::test("claude-code")
            .name("Claude Code")
            .description("Anthropic's coding agent")
            .dir(PathBuf::from("/home/me/.local/share/colonizer/modules/agents/claude-code"))
            .entry(vec!["runner.mjs".into()])
            .needs_claude(true)
            .schema(json!({"type": "object", "properties": {"model": {"type": "string", "default": "sonnet"}}}))
    }

    /// Every event name the batch carries, in order.
    fn names(batch: &Batch) -> Vec<&str> {
        batch.events.iter().map(|event| event.name.as_str()).collect()
    }

    /// The whole closed vocabulary a batch may draw from, spelled out here rather than derived from
    /// the code, so a new string appearing in the payload fails the no-free-text test until it is
    /// added here deliberately. A string in the JSON must be one of these, a machine-shaped field, or
    /// an event name whose `.`-separated parts are all these.
    const TOKENS: &[&str] = &[
        // Field names, Cratefield's grammar's.
        "schema",
        "install",
        "client",
        "kind",
        "version",
        "platform",
        "arch",
        "modules",
        "events",
        "name",
        "outcome",
        "error",
        "duration",
        "count",
        // The one module name a batch declares, and the client shape it names itself with.
        "mothership",
        "server",
        "linux",
        "macos",
        "x86-64",
        "aarch64",
        "other",
        // The neutral values every counted observation carries.
        "ok",
        "none",
        "unknown",
        // Count and duration buckets.
        "0",
        "1",
        "2-3",
        "4-7",
        "8-15",
        "16-63",
        "64+",
        "<1s",
        "1-2s",
        "2-5s",
        "5-15s",
        "15-60s",
        "60s+",
        // Sandbox stacks: `auto` (detection in use, reported as configured), the preset ids, plus
        // the label for one the harness does not know. Booleans, as labels.
        "auto",
        "node",
        "python",
        "rust",
        "go",
        "custom",
        "unknown",
        "true",
        "false",
        // Boot phases, in boot order.
        "issue",
        "git",
        "providers",
        "mesh-start",
        "image-pull",
        "vm-boot",
        "mesh-join",
        "agentd",
        // Event name namespaces: the fields the mapping hangs observations on.
        "colonies",
        "parallel_now",
        "pr_opened",
        "no_changes",
        "stopped",
        "failed",
        "sandbox",
        "preset",
        "image_changed",
        "autopilot",
        "enabled",
        "held",
        "setting",
        "boot",
        "providers",
        "error",
        // Failure kinds: the harness's own names and the attention reasons it sets.
        "agentd_not_ready",
        "harness_restarted",
        "vm_stopped",
        "publish_interrupted",
        "stalled",
        "waiting_for_answer",
        "nudges_exhausted",
        "autopilot_held",
        "model_error",
        "agent_failed",
        // Setting names this install set (schema-declared keys, never values).
        "agent",
        "model",
        "image",
    ];

    /// Every string in the JSON: object keys and string values, recursively.
    fn collect_strings(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, item) in map {
                    out.push(key.clone());
                    collect_strings(item, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect_strings(item, out)),
            Value::String(s) => out.push(s.clone()),
            _ => {}
        }
    }

    /// The two machine-generated strings, recognised by shape rather than by membership: the install
    /// id (32 lowercase hex — a UUID without its dashes, or the all-zero stand-in) and the version
    /// triple. The client shape's values are in `TOKENS`.
    fn is_machine_field(string: &str) -> bool {
        if string.len() == 32 && string.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
            return true;
        }
        string == env!("CARGO_PKG_VERSION")
    }

    #[test]
    fn a_batch_never_carries_free_text_from_a_colony() {
        let mut unnamed = session(SessionStatus::Failed);
        unnamed.error = Some("fatal: could not read Username for 'https://github.com/acme/private.git'".into());
        let mut stalled = session(SessionStatus::Running);
        stalled.attention = Some(json!({"reason": "stalled", "since": Utc::now(), "nudges": 2}));
        let mut injected = session(SessionStatus::Idle);
        injected.attention = Some(json!({"reason": "DROP TABLE sessions; -- /home/me/notes.md", "since": Utc::now()}));
        let mut booted = session(SessionStatus::Idle);
        booted.boot_timing = Some(json!({
            "total_ms": 12_345,
            "phases": [
                {"name": "issue", "ms": 900},
                {"name": "vm-boot", "ms": 2_400},
                {"name": "/home/me/secret-hook", "ms": 5}
            ]
        }));
        let sessions = vec![
            unnamed,
            stalled,
            injected,
            booted,
            session(SessionStatus::PrOpened),
            session(SessionStatus::NoChanges),
        ];

        let mut modules = ModulesConfig::default();
        modules.sandbox.settings = json!({"preset": "node", "image": "ghcr.io/acme-corp/secret-project:v2"})
            .as_object()
            .cloned()
            .unwrap();
        modules.agent.settings = json!({"model": "acme/claude-opus-private-router", "backdoor": "yes"})
            .as_object()
            .cloned()
            .unwrap();

        let providers = [Provider {
            id: "acme-internal".into(),
            name: "Acme internal".into(),
            base_url: "http://llm.corp.acme.internal:8080/v1".into(),
            auth: "bearer".into(),
            wire: Default::default(),
            models: vec!["acme-private-model".into()],
            preset: "custom".into(),
            timeout_secs: None,
            max_concurrent: None,
            queue_timeout_secs: None,
            context_tokens: None,
            fallback_model: None,
            pricing: None,
            model_map: BTreeMap::new(),
            disabled_tools: Vec::new(),
            quota: None,
            normalize_cache_ttl: false,
            trusted: false,
            vetted: false,
            vendor: None,
        }];

        let batch = build(
            Some("0b0c9a8e-4f7d-4a51-9b2e-3c1d5e6f7a8b".into()),
            &sessions,
            &modules,
            &[agent()],
            providers.len(),
        );
        let text = serde_json::to_string(&batch).unwrap();

        // Not one hostile substring anywhere in what a sender would transmit.
        for secret in [
            "acme",
            "secret-project",
            "ghcr.io",
            "could not read Username",
            "https://",
            "private.git",
            "claude-opus-private-router",
            "llm.corp",
            ":8080",
            "/home/me",
            "issue-42",
            "colonizer/",
            "DROP TABLE",
            "secret-hook",
            "backdoor",
            "STYLE.md",
            "pull/7",
            "a1b2c3d4",
        ] {
            assert!(!text.contains(secret), "the batch leaked `{secret}`: {text}");
        }

        // And every string it does carry is accounted for: a closed-vocabulary token, one of the
        // two machine-shaped fields, or an event name assembled wholly from closed tokens.
        let mut strings = Vec::new();
        collect_strings(&serde_json::to_value(&batch).unwrap(), &mut strings);
        strings.sort();
        strings.dedup();
        for string in &strings {
            let parts: Vec<&str> = string.split('.').collect();
            assert!(
                is_machine_field(string)
                    || TOKENS.contains(&string.as_str())
                    || (parts.len() > 1 && parts.iter().all(|part| TOKENS.contains(part))),
                "the batch carries a string outside the closed vocabulary: {string:?}"
            );
        }
    }

    #[test]
    fn a_batch_is_exactly_cratefields_five_fields() {
        let batch = build(None, &[], &ModulesConfig::default(), &[], 0);
        let value = serde_json::to_value(&batch).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["client", "events", "install", "modules", "schema"]);
        assert_eq!(value["schema"], payload::SCHEMA);
        assert_eq!(
            value["install"],
            "0".repeat(32),
            "the all-zero stand-in while no id was minted to ride on — never sent"
        );
        assert_eq!(value["modules"], json!(["mothership"]));
        assert!(
            !value["events"].as_array().unwrap().is_empty(),
            "even the empty install reports its zeros: the grammar requires at least one event"
        );
        let client = &value["client"];
        assert_eq!(client["kind"], json!("server"));
        // The exact platform and arch are whatever this machine is: CI is Linux x86-64, a
        // maintainer's Mac is macOS aarch64. The mapping itself is client_shape's to test.
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(
                [client["platform"].clone(), client["arch"].clone()],
                [json!("linux"), json!("x86-64")],
                "the shape the CI machine reports; the mapping is client_shape's"
            );
        } else {
            assert!(client["platform"].as_str().is_some_and(|s| !s.is_empty()));
            assert!(client["arch"].as_str().is_some_and(|s| !s.is_empty()));
        }
        assert_eq!(client["version"], env!("CARGO_PKG_VERSION"));
    }

    /// A batch dense with everything the vocabulary can carry: every boot phase, every failure kind,
    /// settings on two kinds. The ceiling is part of the grammar — a mapping that could overflow it
    /// would be refused by the collector, so the sum is held here, under it, by construction.
    fn maximal_fixture() -> (Option<String>, Vec<Session>, ModulesConfig, Vec<AgentModule>) {
        let mut booted = session(SessionStatus::Idle);
        booted.boot_timing = Some(json!({
            "total_ms": 12_345,
            "phases": BOOT_PHASES.map(|phase| json!({"name": phase, "ms": 1_000})).to_vec()
        }));
        let mut failed = session(SessionStatus::Failed);
        failed.error = Some(sessions::AGENTD_NOT_READY.into());
        let mut watchdog = session(SessionStatus::WaitingForAnswer);
        watchdog.attention = Some(json!({"reason": "nudges_exhausted", "since": Utc::now(), "nudges": 3}));
        let mut held = session(SessionStatus::Queued);
        held.attention = Some(json!({"reason": AUTOPILOT_HELD, "since": Utc::now()}));
        let mut errored = session(SessionStatus::Running);
        errored.attention = Some(json!({"reason": crate::gateway::MODEL_ERROR_REASON, "since": Utc::now()}));
        let mut runner_gone = session(SessionStatus::Failed);
        runner_gone.attention = Some(json!({"reason": AGENT_FAILED, "since": Utc::now()}));

        let mut modules = ModulesConfig::default();
        modules.sandbox.settings = json!({"preset": "node", "image": "node:24-bookworm"})
            .as_object()
            .cloned()
            .unwrap();
        modules.agent.settings = json!({"model": "sonnet"}).as_object().cloned().unwrap();

        (
            Some("0b0c9a8e-4f7d-4a51-9b2e-3c1d5e6f7a8b".into()),
            vec![booted, failed, watchdog, held, errored, runner_gone],
            modules,
            vec![agent()],
        )
    }

    #[test]
    fn the_batch_parses_under_cratefields_grammar_and_stays_under_the_ceiling() {
        let (id, sessions, modules, agents) = maximal_fixture();
        let (value, vocabulary) = cratefield_value(id.clone(), &sessions, &modules, &agents, 2);

        // What build() makes is exactly what the grammar accepts, and the shown bytes — the API's,
        // usage-last.json's, the notice's — parse again the way the collector would parse them.
        let built = payload::Batch::parse(&value, &vocabulary).unwrap();
        let shown = build(id, &sessions, &modules, &agents, 2);
        assert_eq!(shown, built);
        let reparsed = payload::Batch::parse(&serde_json::to_value(&shown).unwrap(), &vocabulary).unwrap();
        assert_eq!(shown, reparsed);
        assert!(shown.events.len() <= payload::MAX_EVENTS_PER_BATCH);
    }

    #[test]
    fn client_shape_maps_the_live_map_platforms_onto_the_grammar() {
        assert_eq!(client_shape("linux-x86_64"), ("linux", "x86-64"));
        assert_eq!(client_shape("darwin-arm64"), ("macos", "aarch64"));
        assert_eq!(client_shape("other"), ("other", "other"));
        assert_eq!(client_shape("plan9-powerpc"), ("other", "other"));
    }

    #[test]
    fn a_version_that_is_not_a_release_triple_rounds_down() {
        assert_eq!(release_triple("0.1.9"), "0.1.9");
        assert_eq!(
            release_triple("1.0.0-beta+homebrew"),
            "0.0.0",
            "the grammar rejects tags, never trims them"
        );
    }

    #[test]
    fn the_install_id_becomes_the_hex_install_and_garbage_becomes_the_placeholder() {
        assert_eq!(
            install_of(Some("0b0c9a8e-4f7d-4a51-9b2e-3c1d5e6f7a8b".into())),
            "0b0c9a8e4f7d4a519b2e3c1d5e6f7a8b"
        );
        assert_eq!(install_of(None), no_install());
        assert_eq!(install_of(Some("not an id".into())), no_install());
        assert!(consent::install_id_is_valid(&install_of(Some(
            "0b0c9a8e-4f7d-4a51-9b2e-3c1d5e6f7a8b".into()
        ))));
    }

    #[test]
    fn attention_reason_counts_the_runner_never_started_hold() {
        // Setter-owned like autopilot_held: counted, never mistaken for a watchdog reason.
        let mut failed = session(SessionStatus::Idle);
        failed.attention = Some(json!({"reason": "agent_failed", "since": Utc::now(), "nudges": 0}));
        assert_eq!(attention_reason(&failed), Some("agent_failed"));
        let mut held = session(SessionStatus::Idle);
        held.attention = Some(json!({"reason": "autopilot_held", "since": Utc::now(), "nudges": 0}));
        assert_eq!(attention_reason(&held), Some("autopilot_held"));
        let mut unknown = session(SessionStatus::Idle);
        unknown.attention = Some(json!({"reason": "DROP TABLE sessions;", "since": Utc::now()}));
        assert_eq!(attention_reason(&unknown), None);
        assert_eq!(attention_reason(&session(SessionStatus::Idle)), None);
        // And it reaches the batch under its closed label.
        let kinds = error_kinds(&[failed]);
        assert_eq!(kinds.get("agent_failed"), Some(&"1"));
    }

    #[test]
    fn counts_bucket_at_the_label_edges() {
        assert_eq!(bucket_count(0), "0");
        assert_eq!(bucket_count(1), "1");
        assert_eq!(bucket_count(2), "2-3");
        assert_eq!(bucket_count(3), "2-3");
        assert_eq!(bucket_count(4), "4-7");
        assert_eq!(bucket_count(7), "4-7");
        assert_eq!(bucket_count(8), "8-15");
        assert_eq!(bucket_count(15), "8-15");
        assert_eq!(bucket_count(16), "16-63");
        assert_eq!(bucket_count(63), "16-63");
        assert_eq!(bucket_count(64), "64+");
        assert_eq!(bucket_count(10_000), "64+");
    }

    #[test]
    fn durations_bucket_at_the_label_edges() {
        // Edges are in milliseconds: under a second, then bands at 1, 2, 5, 15 and 60 seconds.
        assert_eq!(bucket_ms(0), "<1s");
        assert_eq!(bucket_ms(999), "<1s");
        assert_eq!(bucket_ms(1_000), "1-2s");
        assert_eq!(bucket_ms(1_999), "1-2s");
        assert_eq!(bucket_ms(2_000), "2-5s");
        assert_eq!(bucket_ms(4_999), "2-5s");
        assert_eq!(bucket_ms(5_000), "5-15s");
        assert_eq!(bucket_ms(14_999), "5-15s");
        assert_eq!(bucket_ms(15_000), "15-60s");
        assert_eq!(bucket_ms(59_999), "15-60s");
        assert_eq!(bucket_ms(60_000), "60s+");
    }

    #[test]
    fn each_switch_keeps_usage_off() {
        assert_eq!(disabled_by_env_values(None, None, None), None);
        assert_eq!(disabled_by_env_values(Some("0"), None, None), Some("COLONIZER_TELEMETRY"));
        assert_eq!(disabled_by_env_values(Some("OFF"), None, None), Some("COLONIZER_TELEMETRY"));
        assert_eq!(disabled_by_env_values(Some("false"), None, None), Some("COLONIZER_TELEMETRY"));
        assert_eq!(disabled_by_env_values(Some("No"), None, None), Some("COLONIZER_TELEMETRY"));
        assert_eq!(
            disabled_by_env_values(Some("on"), None, None),
            None,
            "the app switch must name off to block"
        );
        assert_eq!(disabled_by_env_values(None, Some("1"), None), Some("DO_NOT_TRACK"));
        assert_eq!(disabled_by_env_values(None, Some("yes"), None), Some("DO_NOT_TRACK"));
        assert_eq!(disabled_by_env_values(None, Some("0"), None), None);
        assert_eq!(disabled_by_env_values(None, Some(""), None), None);
        assert_eq!(disabled_by_env_values(None, None, Some("true")), Some("CI"));
        assert_eq!(disabled_by_env_values(None, None, Some("TRUE")), Some("CI"));
        assert_eq!(
            disabled_by_env_values(None, None, Some("1")),
            None,
            "CI counts only as `true`"
        );
        // First match wins, but any one of the three is enough.
        assert_eq!(
            disabled_by_env_values(Some("0"), Some("1"), Some("true")),
            Some("COLONIZER_TELEMETRY")
        );
        assert_eq!(disabled_by_env_values(Some("on"), Some("1"), None), Some("DO_NOT_TRACK"));
    }

    #[tokio::test]
    async fn the_environment_beats_a_saved_yes() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let path = dir.join("usage.json");
        Choice {
            enabled: Some(true),
            usage_id: Some(uuid::Uuid::new_v4().to_string()),
            usage_id_minted_at: Some(Utc::now()),
            notice_shown: false,
        }
        .save(&path)
        .unwrap();
        let usage = Usage::with(path, None, Some("CI")).unwrap();
        assert!(!usage.active().await, "an environment block beats a saved yes");
        assert!(usage.set(true).await.is_err(), "a blocked switch cannot be turned on");
        assert!(usage.set(false).await.is_err(), "nor off: it reads as off either way");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn setting_keys_the_schema_does_not_declare_are_dropped() {
        // The API drops undeclared keys on save, but ModulesConfig::load does not re-validate, so a
        // hand-edited modules.json can carry anything. The batch intersects with the schema first.
        let mut modules = ModulesConfig::default();
        modules.sandbox.settings = json!({"image": "node:24-bookworm", "total_secrets": "yes", "backdoor": true})
            .as_object()
            .cloned()
            .unwrap();
        let batch = build(None, &[], &modules, &[], 0);
        assert_eq!(settings_set(&modules, &[]), vec!["sandbox.image"]);
        assert!(
            names(&batch).contains(&"setting.sandbox.image"),
            "a declared setting becomes one event under its name"
        );
        assert!(
            !serde_json::to_string(&batch).unwrap().contains("bookworm"),
            "values are never sent"
        );
    }

    #[tokio::test]
    async fn switching_on_creates_an_id_and_switching_off_forgets_it() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let path = dir.join("usage.json");
        let usage = Usage::with(path.clone(), None, None).unwrap();
        assert!(
            usage.path.ends_with("usage.json"),
            "kept beside the live map's telemetry.json, not in it"
        );

        usage.set(true).await.unwrap();
        let id = usage.choice.lock().await.usage_id.clone().expect("an id once switched on");
        assert!(uuid::Uuid::parse_str(&id).is_ok_and(|u| u.get_version_num() == 4));
        assert_eq!(
            Choice::load(&path),
            Choice {
                enabled: Some(true),
                usage_id: Some(id.clone()),
                usage_id_minted_at: usage.choice.lock().await.usage_id_minted_at,
                notice_shown: false
            }
        );

        usage.set(false).await.unwrap();
        // The file stays — the question has been answered — but the id is gone, so the next period
        // cannot be joined to this one. No network is touched: switching off is a file write.
        assert_eq!(
            Choice::load(&path),
            Choice {
                enabled: Some(false),
                usage_id: None,
                usage_id_minted_at: None,
                notice_shown: false
            }
        );

        usage.set(true).await.unwrap();
        assert_ne!(
            usage.choice.lock().await.usage_id.as_deref(),
            Some(id.as_str()),
            "a new period gets a new id"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn an_install_that_never_answered_reports_by_default() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let path = dir.join("usage.json");
        let usage = Usage::with(path.clone(), None, None).unwrap();
        assert!(usage.active().await, "reporting is on until the user says no");

        // The first batch mints the id and keeps it, so the whole on-period shares one even though
        // nobody ever answered the question.
        let id = usage.batch_id().await.expect("a batch carries an id without an explicit yes");
        assert!(uuid::Uuid::parse_str(&id).is_ok_and(|u| u.get_version_num() == 4));
        assert_eq!(
            Choice::load(&path).usage_id.as_deref(),
            Some(id.as_str()),
            "the id is persisted on first use"
        );

        // A no is still a no, and it forgets the id as before.
        usage.set(false).await.unwrap();
        assert!(!usage.active().await);
        assert_eq!(usage.batch_id().await, None, "no id rides on a batch that could not be sent");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn an_answer_written_behind_a_running_mothership_is_honoured() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let path = dir.join("usage.json");
        let usage = Usage::with(path.clone(), None, None).unwrap();
        assert!(usage.active().await);

        // Another process — `colonizer telemetry off` — writes the file behind the running mothership.
        Choice {
            enabled: Some(false),
            usage_id: None,
            usage_id_minted_at: None,
            notice_shown: false,
        }
        .save(&path)
        .unwrap();
        assert!(!usage.active().await, "the answer is re-read before it is consulted");
        assert_eq!(usage.batch_id().await, None, "so the next batch carries no id");

        // And a yes written out of band works the same way, id included: it is used, not replaced.
        let id = uuid::Uuid::new_v4().to_string();
        Choice {
            enabled: Some(true),
            usage_id: Some(id.clone()),
            usage_id_minted_at: Some(Utc::now()),
            notice_shown: false,
        }
        .save(&path)
        .unwrap();
        assert_eq!(usage.batch_id().await.as_deref(), Some(id.as_str()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn the_install_id_rotates_after_thirty_days_and_not_before() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let path = dir.join("usage.json");
        let usage = Usage::with(path.clone(), None, None).unwrap();
        usage.set(true).await.unwrap();

        // A fresh id is kept, not rotated every look.
        let id = usage.batch_id().await.unwrap();
        assert_eq!(usage.batch_id().await.unwrap(), id, "a fresh id is kept");

        // Backdate its birthday past Cratefield's rotation: the next batch gets a new id, and the
        // new birthday is now.
        let mut choice = usage.choice.lock().await;
        choice.usage_id_minted_at = Some(Utc::now() - chrono::Duration::days(i64::from(consent::ROTATION_DAYS) + 1));
        choice.save(&path).unwrap();
        drop(choice);
        let rotated = usage.batch_id().await.unwrap();
        assert_ne!(rotated, id, "an id older than the rotation becomes a new id");
        let kept = Choice::load(&path);
        assert_eq!(kept.usage_id.as_deref(), Some(rotated.as_str()));
        assert_eq!(
            (Utc::now() - kept.usage_id_minted_at.unwrap()).num_days(),
            0,
            "the new id starts its own thirty days"
        );

        // An id with no recorded birthday — a file from before rotation was kept — is treated as
        // overdue: rotated once, then kept.
        let mut choice = usage.choice.lock().await;
        choice.usage_id_minted_at = None;
        choice.save(&path).unwrap();
        drop(choice);
        let unmarked = usage.batch_id().await.unwrap();
        assert_ne!(unmarked, rotated);
        assert_eq!(
            usage.batch_id().await.unwrap(),
            unmarked,
            "and then it keeps its own birthday"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn the_first_run_notice_is_shown_once_then_quiet() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let batch = build(None, &[], &ModulesConfig::default(), &[], 0);

        let fresh = Usage::with(dir.join("usage.json"), None, None).unwrap();
        assert!(
            fresh.show_notice(&batch).await.unwrap(),
            "the first start shows the batch on stderr"
        );
        assert!(
            Choice::load(&dir.join("usage.json")).notice_shown,
            "shown is recorded in the answer's file, so a restart is quiet"
        );
        assert!(
            !fresh.show_notice(&batch).await.unwrap(),
            "and it is not shown twice in one life either"
        );

        // An install that answered — either way — is never shown it.
        Choice {
            enabled: Some(false),
            usage_id: None,
            usage_id_minted_at: None,
            notice_shown: false,
        }
        .save(&dir.join("answered.json"))
        .unwrap();
        let answered = Usage::with(dir.join("answered.json"), None, None).unwrap();
        assert!(!answered.show_notice(&batch).await.unwrap());

        // Nor one whose environment keeps reporting off.
        let blocked = Usage::with(dir.join("blocked.json"), None, Some("CI")).unwrap();
        assert!(!blocked.show_notice(&batch).await.unwrap());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_last_batch_is_kept_only_when_its_bytes_change() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let last = dir.join(LAST_BATCH_FILE);
        let batch = build(None, &[], &ModulesConfig::default(), &[], 0);
        assert!(keep_last(&last, &batch), "the first batch built is kept");
        let mut expected = serde_json::to_vec_pretty(&batch).unwrap();
        expected.push(b'\n');
        assert_eq!(
            std::fs::read(&last).unwrap(),
            expected,
            "exactly the bytes `telemetry show` should print"
        );
        assert!(
            !keep_last(&last, &batch),
            "an unchanged batch is not written again, however often it is built"
        );

        let other = build(Some(uuid::Uuid::new_v4().to_string()), &[], &ModulesConfig::default(), &[], 0);
        assert!(keep_last(&last, &other), "a changed batch is written");
        assert_ne!(std::fs::read(&last).unwrap(), expected);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_cli_records_the_answer_without_a_running_mothership() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        cli_set(&dir, true).unwrap();
        let id = Choice::load(&dir.join(CHOICE_FILE))
            .usage_id
            .expect("an id, minted by the cli too");
        cli_set(&dir, false).unwrap();
        assert_eq!(
            Choice::load(&dir.join(CHOICE_FILE)),
            Choice {
                enabled: Some(false),
                usage_id: None,
                usage_id_minted_at: None,
                notice_shown: false
            }
        );
        cli_set(&dir, true).unwrap();
        let again = Choice::load(&dir.join(CHOICE_FILE));
        assert_eq!(again.enabled, Some(true));
        assert_ne!(
            again.usage_id.as_deref(),
            Some(id.as_str()),
            "off forgot the id, so on starts a new period"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn error_kinds_name_only_what_the_harness_names() {
        let mut agentd = session(SessionStatus::Failed);
        agentd.error = Some(sessions::AGENTD_NOT_READY.into());
        let mut stopped = session(SessionStatus::Stopped);
        stopped.error = Some(sessions::VM_STOPPED_EARLY.into());
        let mut watchdog = session(SessionStatus::WaitingForAnswer);
        watchdog.attention = Some(json!({"reason": "nudges_exhausted", "since": Utc::now(), "nudges": 3}));
        let mut own_text = session(SessionStatus::Failed);
        own_text.error = Some("git fetch failed: repository 'acme/secret' not found".into());
        let mut injected = session(SessionStatus::Running);
        injected.attention = Some(json!({"reason": "whatever_a_hand_edited_file_says"}));

        let batch = build(
            None,
            &[agentd, stopped, watchdog, own_text, injected],
            &ModulesConfig::default(),
            &[],
            0,
        );
        let batch_names = names(&batch);
        for named in ["error.agentd_not_ready.1", "error.nudges_exhausted.1", "error.vm_stopped.1"] {
            assert!(batch_names.contains(&named), "the batch counts {named}: {batch_names:?}");
        }
        assert!(
            !batch_names.iter().any(|name| name.starts_with("error.autopilot_held")),
            "nothing holds autopilot here, so nothing names it"
        );
        assert!(names(&batch).contains(&"autopilot.held.0"));
        assert!(
            !batch_names.iter().any(|name| name.contains("acme")),
            "a failure the harness did not name contributes no kind, whatever its text says"
        );
    }

    #[test]
    fn the_gateway_model_error_reason_is_a_named_kind() {
        let mut errored = session(SessionStatus::Running);
        errored.attention = Some(json!({"reason": crate::gateway::MODEL_ERROR_REASON, "since": Utc::now()}));
        assert_eq!(
            error_kinds(&[errored]),
            BTreeMap::from([(crate::gateway::MODEL_ERROR_REASON, "1")]),
            "a colony the gateway flagged after an upstream 4xx/5xx buckets under model_error"
        );
    }

    #[test]
    fn a_preset_the_harness_does_not_know_is_reported_as_unknown() {
        assert_eq!(preset_label("node"), "node");
        assert_eq!(preset_label("rust"), "rust");
        assert_eq!(preset_label("custom"), "custom");
        assert_eq!(
            preset_label("auto"),
            "auto",
            "detection is what this field exists to show, so it is never resolved to its fallback"
        );
        assert_eq!(preset_label("acme-private-stack"), "unknown");
    }

    #[test]
    fn the_image_is_reported_only_as_changed_or_not() {
        let modules = ModulesConfig::default();
        let batch = build(None, &[], &modules, &[], 0);
        assert!(
            names(&batch).contains(&"sandbox.preset.auto") && names(&batch).contains(&"sandbox.image_changed.false"),
            "a fresh install on auto boots the fallback image and must not be reported as changed"
        );

        let mut pinned = ModulesConfig::default();
        pinned.sandbox.settings = json!({"preset": "rust", "image": "ghcr.io/acme-corp/secret-project:v2"})
            .as_object()
            .cloned()
            .unwrap();
        let batch = build(None, &[], &pinned, &[], 0);
        assert!(names(&batch).contains(&"sandbox.preset.rust") && names(&batch).contains(&"sandbox.image_changed.true"));
        assert!(
            !serde_json::to_string(&batch).unwrap().contains("ghcr.io"),
            "the image string is not sent"
        );
    }

    #[test]
    fn boot_ms_buckets_the_median_of_each_known_phase() {
        let mut a = session(SessionStatus::Idle);
        a.boot_timing =
            Some(json!({"total_ms": 10_000, "phases": [{"name": "vm-boot", "ms": 1_200}, {"name": "agentd", "ms": 400}]}));
        let mut b = session(SessionStatus::Idle);
        b.boot_timing = Some(json!({"total_ms": 9_000, "phases": [{"name": "vm-boot", "ms": 6_000}]}));
        let mut c = session(SessionStatus::Idle);
        c.boot_timing = Some(json!({"total_ms": 950_000, "phases": [{"name": "vm-boot", "ms": 900_000}]}));
        let batch = build(None, &[a, b, c], &ModulesConfig::default(), &[], 0);
        let batch_names: Vec<&str> = names(&batch).into_iter().filter(|name| name.starts_with("boot.")).collect();
        assert_eq!(
            batch_names,
            ["boot.vm-boot.5-15s", "boot.agentd.<1s"],
            "the median of 1200, 6000 and 900000, in boot order, and an unnamed phase never appears"
        );
    }

    #[test]
    fn boot_ms_leaves_out_a_boot_that_never_finished() {
        let mut booted = session(SessionStatus::Idle);
        booted.boot_timing =
            Some(json!({"total_ms": 1_800, "phases": [{"name": "issue", "ms": 200}, {"name": "vm-boot", "ms": 1_200}]}));
        // Failed after the VM came up, or still starting: a breakdown with no `total_ms`.
        let mut failed = session(SessionStatus::Failed);
        failed.boot_timing = Some(json!({"phases": [{"name": "issue", "ms": 90_000}, {"name": "vm-boot", "ms": 90_000}]}));
        let mut starting = session(SessionStatus::Starting);
        starting.boot_timing = Some(json!({"phases": [{"name": "issue", "ms": 90_000}]}));
        let batch = build(None, &[booted, failed, starting], &ModulesConfig::default(), &[], 0);
        let batch_names: Vec<&str> = names(&batch).into_iter().filter(|name| name.starts_with("boot.")).collect();
        assert_eq!(
            batch_names,
            ["boot.issue.<1s", "boot.vm-boot.1-2s"],
            "only the boot that finished, and so has `total_ms`, is sampled"
        );
    }

    #[test]
    fn colonies_are_counted_by_status() {
        let sessions = vec![
            session(SessionStatus::Running),
            session(SessionStatus::Idle),
            session(SessionStatus::Queued),
            session(SessionStatus::PrOpened),
            session(SessionStatus::PrOpened),
            session(SessionStatus::Failed),
            session(SessionStatus::Stopped),
        ];
        let batch = build(None, &sessions, &ModulesConfig::default(), &[], 3);
        let batch_names = names(&batch);
        for named in [
            "colonies.parallel_now.2-3", // live colonies only; queued holds no microVM
            "colonies.pr_opened.2-3",
            "colonies.no_changes.0",
            "colonies.stopped.1",
            "colonies.failed.1",
            "providers.2-3", // three providers, bucketed like any other count
            "autopilot.enabled.true",
            "autopilot.held.0",
        ] {
            assert!(batch_names.contains(&named), "the batch counts {named}: {batch_names:?}");
        }
        assert!(
            !batch_names.iter().any(|name| name.starts_with("setting.")),
            "an install that set nothing names nothing"
        );
    }

    /// A stand-in collector that records what it receives and answers `status`.
    async fn collector(status: StatusCode) -> (String, Arc<Mutex<Vec<Value>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = received.clone();
        let router = Router::new()
            .route(
                "/v1/telemetry/events",
                post(
                    move |State(log): State<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>| async move {
                        log.lock().await.push(body);
                        status
                    },
                ),
            )
            .with_state(log);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        // The full ingest route, as an operator would name it in COLONIZER_TELEMETRY_ENDPOINT.
        let url = format!("http://{}/v1/telemetry/events", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (url, received)
    }

    #[tokio::test]
    async fn nothing_is_sent_while_the_switch_is_off_or_no_endpoint_is_named() {
        let (url, received) = collector(StatusCode::OK).await;
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let batch = build(Some(uuid::Uuid::new_v4().to_string()), &[], &ModulesConfig::default(), &[], 0);

        // The switch off, an endpoint named: no batch is owed, and a send refuses.
        let path = dir.join("off.json");
        Choice {
            enabled: Some(false),
            usage_id: None,
            usage_id_minted_at: None,
            notice_shown: false,
        }
        .save(&path)
        .unwrap();
        let off = Usage::with(path, Some(url.clone()), None).unwrap();
        assert!(!off.due().await, "the switch is off, so nothing is owed");
        assert!(!off.send(&batch).await, "the send refuses with the switch off");
        assert!(received.lock().await.is_empty(), "the collector saw nothing");

        // The switch on, no endpoint — the default install: nothing is owed, nothing to post to.
        let on = Usage::with(dir.join("on.json"), None, None).unwrap();
        assert!(!on.due().await, "no endpoint, so nothing is owed however new the batch");
        assert!(!on.send(&batch).await, "the send refuses without an endpoint");
        assert!(received.lock().await.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_due_send_posts_exactly_the_shown_batch_and_records_the_success() {
        let (url, received) = collector(StatusCode::OK).await;
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let path = dir.join("usage.json");
        let usage = Usage::with(path.clone(), Some(url), None).unwrap();
        usage.set(true).await.unwrap();

        // A first send is owed with no recorded success.
        assert!(usage.due().await, "never sent, so the batch is owed");

        // The batch built is the one value everything uses — the API's `batch` field, the file
        // `telemetry show` prints, and the bytes the sender posts.
        let batch = build(
            Some(usage.choice.lock().await.usage_id.clone().unwrap()),
            &[],
            &ModulesConfig::default(),
            &[],
            0,
        );
        assert!(usage.send(&batch).await, "the collector answers 200, so the batch went");

        let bodies = received.lock().await;
        assert_eq!(bodies.len(), 1, "one send, not a burst");
        assert_eq!(
            bodies[0],
            serde_json::to_value(&batch).unwrap(),
            "the collector read exactly the shown batch"
        );

        // And what it read is inside the grammar the real collector parses.
        let (_, vocabulary) = cratefield_value(
            usage.choice.lock().await.usage_id.clone(),
            &[],
            &ModulesConfig::default(),
            &[],
            0,
        );
        let parsed = payload::Batch::parse(&bodies[0], &vocabulary).expect("the posted batch parses");
        assert_eq!(parsed, batch);

        drop(bodies);
        assert!(
            Sent::load(&usage.sent).is_some(),
            "the success is recorded, so the cadence can compare against it"
        );
        assert!(!usage.due().await, "just sent, so nothing is owed for the next 24 hours");

        // A failure is dropped, not queued: the collector's rejection is logged and the success is
        // not recorded, so the next due tick tries once more — hourly at the soonest, never in a
        // retry storm.
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_non_2xx_answer_drops_the_batch_and_records_no_success() {
        let (url, received) = collector(StatusCode::BAD_REQUEST).await;
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let path = dir.join("usage.json");
        let usage = Usage::with(path, Some(url), None).unwrap();
        usage.set(true).await.unwrap();
        assert!(usage.due().await);

        let batch = build(Some(uuid::Uuid::new_v4().to_string()), &[], &ModulesConfig::default(), &[], 0);
        assert!(!usage.send(&batch).await, "a 400 is not a send");
        assert_eq!(
            received.lock().await.len(),
            1,
            "the collector did receive it — and answered 400"
        );
        assert!(Sent::load(&usage.sent).is_none(), "a dropped batch records no success");
        assert!(usage.due().await, "still owed: the next tick tries once more, not sooner");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn the_placeholder_install_is_never_posted() {
        let (url, received) = collector(StatusCode::OK).await;
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        // On, but with an id of no shape at all — a hand-edited usage.json: active() is true, and
        // the batch's install is the all-zero stand-in, which the sender refuses to post.
        Choice {
            enabled: Some(true),
            usage_id: Some("not-an-id".into()),
            usage_id_minted_at: Some(Utc::now()),
            notice_shown: true,
        }
        .save(&dir.join("usage.json"))
        .unwrap();
        let usage = Usage::with(dir.join("usage.json"), Some(url), None).unwrap();
        let batch = build(Some("not-an-id".into()), &[], &ModulesConfig::default(), &[], 0);
        assert_eq!(batch.install, no_install());
        assert!(!usage.send(&batch).await, "the placeholder never reaches a collector");
        assert!(received.lock().await.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_success_whose_record_cannot_be_written_is_not_resent_next_tick() {
        let (url, received) = collector(StatusCode::OK).await;
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let usage = Usage::with(dir.join("usage.json"), Some(url), None).unwrap();
        usage.set(true).await.unwrap();
        assert!(usage.due().await);

        // The record's path is taken by a directory, so the write after a 2xx fails.
        std::fs::create_dir(&usage.sent).unwrap();
        let batch = build(Some(uuid::Uuid::new_v4().to_string()), &[], &ModulesConfig::default(), &[], 0);
        assert!(usage.send(&batch).await, "the collector answers 200, so the batch went");
        assert_eq!(received.lock().await.len(), 1);
        assert!(
            !usage.due().await,
            "the in-memory record keeps the 24-hour cadence when the file cannot be written"
        );
        std::fs::remove_dir_all(&usage.sent).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
