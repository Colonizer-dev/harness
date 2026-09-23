//! Anonymous usage reporting: the batch that would be sent, and the switch that says whether one may
//! ever leave the machine. This build ships the local half only — no sender, no endpoint constant, no
//! background loop — because a client will be composed from Cratefield's `module-telemetry` crate when
//! that exists (Cratefield/harness#413). Until then the batch is built and shown — at
//! `GET /api/telemetry/usage`, at `colonizer telemetry show`, and once on stderr at the first
//! start — so a user can always read exactly what is reported, before answering or after.
//!
//! Reporting is on unless the user says no: `colonizer telemetry on|off` writes the answer straight
//! to `<config>/usage.json`, with no network and no running mothership needed, so switching off —
//! like switching on — is nothing but a file write.
//!
//! Everything in a batch comes from a closed vocabulary, enforced by the test at the bottom of this
//! file: counts and durations as buckets, settings as schema-declared names without values, boot
//! phases and failures as labels the harness itself defines.

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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use tokio::sync::Mutex;

/// Bumped when the batch's shape or vocabulary changes, so a future sender can tell periods apart.
const PAYLOAD_VERSION: u32 = 1;

/// The file the answer is kept in, beside the live map's telemetry.json in the config dir.
const CHOICE_FILE: &str = "usage.json";
/// The file the last batch built is kept in, so `colonizer telemetry show` in another process can
/// print its exact bytes.
const LAST_BATCH_FILE: &str = "usage-last.json";

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
];

/// One anonymous usage batch — exactly what a sender would transmit, built by [`batch`]. There is no
/// sender in this build; see the module docs.
///
/// Nothing here is sourced from inside a colony: no agent output, no terminal output, no repository,
/// branch, issue or worktree names, no paths, no diffs, no prompts, no tokens, no URLs, no model or
/// image strings. Every string is a label from a closed, compile-time set — bucket labels, preset
/// ids, boot phase names, failure kinds, module kinds and schema-declared setting names — except
/// three machine-generated fields: `usage_id` (a random UUID, separate from the live map's
/// `install_id` so the two datasets cannot be joined), `harness_version` (the crate's own version)
/// and `platform` (the same closed string the live map sends). A colony's free-text error
/// (`Session.error`) never appears: failures are bucketed under the fixed messages the harness
/// writes itself, and a failure the harness did not name contributes nothing.
#[derive(Debug, Serialize, PartialEq)]
pub struct Batch {
    pub payload_version: u32,
    /// Random per on-period, `null` while the switch is off. Never the live map's install id.
    pub usage_id: Option<String>,
    pub harness_version: &'static str,
    pub platform: &'static str,
    pub colonies: Colonies,
    pub sandbox: Sandbox,
    pub autopilot: Autopilot,
    /// `<kind>.<key>` for every setting an install carries that its schema declares, sorted. Names
    /// only, never values.
    pub settings_set: Vec<String>,
    /// Where boot time went, per phase, across the colonies this install has booted.
    pub boot_ms: Vec<BootPhase>,
    pub providers: &'static str,
    pub error_kinds: BTreeMap<&'static str, &'static str>,
}

/// How busy this mothership is, and how its finished colonies came out.
#[derive(Debug, Serialize, PartialEq)]
pub struct Colonies {
    /// Colonies with a running microVM right now; queued ones hold none, so they are not counted.
    pub parallel_now: &'static str,
    /// How finished colonies came out, by the status they ended in.
    pub terminal: Terminal,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Terminal {
    pub pr_opened: &'static str,
    pub no_changes: &'static str,
    pub stopped: &'static str,
    pub failed: &'static str,
}

/// The sandbox stack, without the image string itself, which is user free text.
#[derive(Debug, Serialize, PartialEq)]
pub struct Sandbox {
    /// The stack as configured: `auto` when detection is in use, a preset id otherwise, or `unknown`
    /// if the configured preset is not one the harness knows.
    pub preset: &'static str,
    /// Whether the image a colony actually boots differs from the one the resolved stack names.
    pub image_changed_from_default: bool,
}

/// Autopilot: the publish module's default for new colonies, and how many colonies it is holding.
#[derive(Debug, Serialize, PartialEq)]
pub struct Autopilot {
    pub enabled: bool,
    pub held: &'static str,
}

/// One boot phase and where its median duration lands.
#[derive(Debug, Serialize, PartialEq)]
pub struct BootPhase {
    pub phase: &'static str,
    pub bucket: &'static str,
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
fn boot_ms(sessions: &[Session]) -> Vec<BootPhase> {
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
            BootPhase {
                phase,
                bucket: bucket_ms(samples[samples.len() / 2]),
            }
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
/// (`Session.error`) is free text and never becomes a kind, so this map can sum to less than
/// `colonies.terminal.failed` — the unnamed failures are simply not named here.
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

/// The pure half of [`batch`]: plain values in, batch out, so tests can build one from fixtures
/// without an `App`.
fn build(
    usage_id: Option<String>,
    sessions: &[Session],
    modules: &ModulesConfig,
    agents: &[AgentModule],
    providers: usize,
) -> Batch {
    let sandbox_schema = schema_for("sandbox", &modules.sandbox.provider, agents);
    let preset = setting_str(&modules.sandbox, &sandbox_schema, "preset");
    let held = sessions
        .iter()
        .filter(|s| attention_reason(s) == Some(AUTOPILOT_HELD))
        .count();
    Batch {
        payload_version: PAYLOAD_VERSION,
        usage_id,
        harness_version: env!("CARGO_PKG_VERSION"),
        platform: telemetry::platform(),
        colonies: Colonies {
            parallel_now: bucket_count(sessions.iter().filter(|s| s.status.is_live()).count()),
            terminal: Terminal {
                pr_opened: terminal_count(sessions, SessionStatus::PrOpened),
                no_changes: terminal_count(sessions, SessionStatus::NoChanges),
                stopped: terminal_count(sessions, SessionStatus::Stopped),
                failed: terminal_count(sessions, SessionStatus::Failed),
            },
        },
        sandbox: Sandbox {
            preset: preset_label(&preset),
            // Only the comparison is sent: the image string itself is user free text. The configured
            // preset, not any colony's detected one — telemetry reports what the install does, and
            // this code has no repository in hand to detect from.
            image_changed_from_default: sessions::colony_image(agents, modules, &preset)
                != image_baseline(&preset, &sandbox_schema),
        },
        autopilot: Autopilot {
            enabled: sessions::autopilot_default(agents, modules),
            held: bucket_count(held),
        },
        settings_set: settings_set(modules, agents),
        boot_ms: boot_ms(sessions),
        providers: bucket_count(providers),
        error_kinds: error_kinds(sessions),
    }
}

/// Gathers a batch from live state. This is the one function a sender would call (and then serialise
/// with `serde_json`), which is why the API shows exactly its output. Each batch built is also kept
/// in `usage-last.json`, for `colonizer telemetry show` in another process.
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

/// The usage switch and the files it is kept in. There is no sender to wake, so unlike the live
/// map's `Telemetry` this holds no client and no loop: the answer is re-read from the file before it
/// is consulted, so a choice written by another process takes effect without a restart, and
/// rewritten when the user answers.
pub struct Usage {
    path: PathBuf,
    /// Where the last batch built is kept, for `colonizer telemetry show` in another process.
    last: PathBuf,
    blocked: Option<&'static str>,
    choice: Mutex<Choice>,
}

impl Usage {
    pub fn new(config_dir: &Path) -> Self {
        Self::with(config_dir.join(CHOICE_FILE), disabled_by_env())
    }

    fn with(path: PathBuf, blocked: Option<&'static str>) -> Self {
        Self {
            last: path.with_file_name(LAST_BATCH_FILE),
            choice: Mutex::new(Choice::load(&path)),
            path,
            blocked,
        }
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
    /// the whole on-period whatever switched reporting on.
    async fn batch_id(&self) -> Option<String> {
        if self.blocked.is_some() {
            return None;
        }
        self.reload().await;
        let mut choice = self.choice.lock().await;
        if choice.enabled == Some(false) {
            return None;
        }
        if let Some(id) = choice.usage_id.clone() {
            return Some(id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        choice.usage_id = Some(id.clone());
        // If this write fails the batch is still built; the id just would not survive a restart.
        let _ = choice.save(&self.path);
        Some(id)
    }

    /// Switches usage reporting on or off and saves the answer. Switching on keeps or creates the
    /// usage id; switching off forgets it, so the next period cannot be joined to this one. This
    /// touches no network and needs no round-trip: there is no sender in this build, so writing the
    /// file here is the whole of the operation.
    async fn set(&self, enabled: bool) -> Result<()> {
        if let Some(variable) = self.blocked {
            bail!("usage reporting is kept off by {variable} in the mothership's environment");
        }
        self.reload().await;
        let mut choice = self.choice.lock().await;
        choice.usage_id = match enabled {
            true => Some(choice.usage_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string())),
            false => None,
        };
        choice.enabled = Some(enabled);
        choice.save(&self.path)
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
             is the exact batch it would send:\n{}\nTurn it off with `colonizer telemetry off`; the \
             environment variables COLONIZER_TELEMETRY, DO_NOT_TRACK and CI also keep it off.",
            serde_json::to_string_pretty(batch)?
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

/// The body of `GET /api/telemetry/usage`, and of a successful `PUT`. `batch` is exactly what a sender
/// would transmit — built by the same [`batch`] function, not a re-derivation.
#[derive(Debug, Serialize, PartialEq)]
pub struct Status {
    pub enabled: bool,
    /// The environment switch holding it off (`COLONIZER_TELEMETRY`, `DO_NOT_TRACK` or `CI`), named
    /// the same as the live map's `blocked_by`.
    pub blocked_by: Option<&'static str>,
    pub payload_version: u32,
    pub batch: Batch,
}

async fn view(app: &Shared) -> Status {
    Status {
        enabled: app.usage.active().await,
        blocked_by: app.usage.blocked,
        payload_version: PAYLOAD_VERSION,
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
    choice.usage_id = match enabled {
        true => Some(choice.usage_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string())),
        false => None,
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Provider;
    use chrono::Utc;
    use serde_json::json;
    use std::path::PathBuf;

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
            worktree: "/home/me/.local/share/colonizer/worktrees/acme-corp/secret-project/issue-42-a1b2c3d4".into(),
            git_admin_dir: Some("/home/me/.local/share/colonizer/repos/git-admin".into()),
            sandbox: "colonizer-a1b2c3d4".into(),
            // Added on main while this branch was open; a batch must stay blind to both.
            publish_stage: None,
            publishing_holds_slot: false,
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
            error: None,
            cost_usd: Some(0.42),
            routed_cost_usd: None,
            host_disk_bytes: None,
            model_usage: None,
            model_tier: None,
            claude_account: None,
            model_routing: None,
            cleaned_up: false,
            keep_worktree: false,
            attention: None,
            last_activity_at: Some(now),
            boot_timing: None,
            boot_cpus: None,
            boot_memory: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn agent() -> AgentModule {
        AgentModule {
            id: "claude-code".into(),
            name: "Claude Code".into(),
            description: "Anthropic's coding agent".into(),
            dir: PathBuf::from("/home/me/.local/share/colonizer/modules/agents/claude-code"),
            entry: vec!["runner.mjs".into()],
            needs_claude: true,
            schema: json!({"type": "object", "properties": {"model": {"type": "string", "default": "sonnet"}}}),
        }
    }

    /// The whole closed vocabulary a batch may draw from, spelled out here rather than derived from
    /// the code, so a new string appearing in the payload fails the no-free-text test until it is
    /// added here deliberately.
    const ALLOWED: &[&str] = &[
        // Field names.
        "payload_version",
        "usage_id",
        "harness_version",
        "platform",
        "colonies",
        "parallel_now",
        "terminal",
        "pr_opened",
        "no_changes",
        "stopped",
        "failed",
        "sandbox",
        "preset",
        "image_changed_from_default",
        "autopilot",
        "enabled",
        "held",
        "settings_set",
        "boot_ms",
        "phase",
        "bucket",
        "providers",
        "error_kinds",
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
        // the label for one the harness does not know.
        "auto",
        "node",
        "python",
        "rust",
        "go",
        "custom",
        "unknown",
        // Boot phases, in boot order.
        "issue",
        "git",
        "providers",
        "mesh-start",
        "image-pull",
        "vm-boot",
        "mesh-join",
        "agentd",
        // Failure kinds: the harness's own names and the attention reasons it sets.
        "agentd_not_ready",
        "harness_restarted",
        "vm_stopped",
        "publish_interrupted",
        "stalled",
        "waiting_for_answer",
        "nudges_exhausted",
        "autopilot_held",
        "agent_failed",
        // Setting names this install set (schema-declared keys, never values).
        "agent.model",
        "sandbox.image",
        "sandbox.preset",
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

    /// The three machine-generated strings, recognised by shape rather than by membership.
    fn is_machine_field(string: &str) -> bool {
        if uuid::Uuid::parse_str(string).is_ok_and(|u| u.get_version_num() == 4) {
            return true;
        }
        if string == env!("CARGO_PKG_VERSION") {
            return true;
        }
        // The closed platform set the live map sends.
        matches!(string, "linux-x86_64" | "darwin-arm64" | "other")
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
            normalize_cache_ttl: false,
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

        // And every string it does carry is accounted for: a closed-vocabulary label, or one of the
        // three machine-generated fields recognised by shape.
        let mut strings = Vec::new();
        collect_strings(&serde_json::to_value(&batch).unwrap(), &mut strings);
        strings.sort();
        strings.dedup();
        for string in &strings {
            assert!(
                is_machine_field(string) || ALLOWED.contains(&string.as_str()),
                "the batch carries a string outside the closed vocabulary: {string:?}"
            );
        }
    }

    #[test]
    fn a_batch_is_exactly_eleven_fields() {
        let batch = build(None, &[], &ModulesConfig::default(), &[], 0);
        let value = serde_json::to_value(&batch).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "autopilot",
                "boot_ms",
                "colonies",
                "error_kinds",
                "harness_version",
                "payload_version",
                "platform",
                "providers",
                "sandbox",
                "settings_set",
                "usage_id"
            ]
        );
        assert_eq!(value["payload_version"], 1);
        assert_eq!(value["usage_id"], Value::Null, "no id when none was minted to ride on");
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
            notice_shown: false,
        }
        .save(&path)
        .unwrap();
        let usage = Usage::with(path, Some("CI"));
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
        assert_eq!(batch.settings_set, vec!["sandbox.image"]);
        assert!(
            !serde_json::to_string(&batch).unwrap().contains("bookworm"),
            "values are never sent"
        );
    }

    #[tokio::test]
    async fn switching_on_creates_an_id_and_switching_off_forgets_it() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let path = dir.join("usage.json");
        let usage = Usage::with(path.clone(), None);
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
                notice_shown: false
            }
        );

        usage.set(false).await.unwrap();
        // The file stays — the question has been answered — but the id is gone, so the next period
        // cannot be joined to this one. No network is touched: there is no sender to tell.
        assert_eq!(
            Choice::load(&path),
            Choice {
                enabled: Some(false),
                usage_id: None,
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
        let usage = Usage::with(path.clone(), None);
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
        let usage = Usage::with(path.clone(), None);
        assert!(usage.active().await);

        // Another process — `colonizer telemetry off` — writes the file behind the running mothership.
        Choice {
            enabled: Some(false),
            usage_id: None,
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
            notice_shown: false,
        }
        .save(&path)
        .unwrap();
        assert_eq!(usage.batch_id().await.as_deref(), Some(id.as_str()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn the_first_run_notice_is_shown_once_then_quiet() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let batch = build(None, &[], &ModulesConfig::default(), &[], 0);

        let fresh = Usage::with(dir.join("usage.json"), None);
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
            notice_shown: false,
        }
        .save(&dir.join("answered.json"))
        .unwrap();
        let answered = Usage::with(dir.join("answered.json"), None);
        assert!(!answered.show_notice(&batch).await.unwrap());

        // Nor one whose environment keeps reporting off.
        let blocked = Usage::with(dir.join("blocked.json"), Some("CI"));
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
        assert_eq!(
            batch.error_kinds,
            BTreeMap::from([("agentd_not_ready", "1"), ("nudges_exhausted", "1"), ("vm_stopped", "1")]),
            "a failure or reason the harness did not name contributes no kind"
        );
        assert_eq!(batch.autopilot.held, "0");
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
        assert_eq!(
            batch.sandbox,
            Sandbox {
                preset: "auto",
                image_changed_from_default: false
            }
        );
        assert!(
            !batch.sandbox.image_changed_from_default,
            "a fresh install on auto boots the fallback image and must not be reported as changed"
        );

        let mut pinned = ModulesConfig::default();
        pinned.sandbox.settings = json!({"preset": "rust", "image": "ghcr.io/acme-corp/secret-project:v2"})
            .as_object()
            .cloned()
            .unwrap();
        let batch = build(None, &[], &pinned, &[], 0);
        assert_eq!(
            batch.sandbox,
            Sandbox {
                preset: "rust",
                image_changed_from_default: true
            }
        );
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
        assert_eq!(
            batch.boot_ms,
            vec![
                BootPhase {
                    phase: "vm-boot",
                    bucket: "5-15s"
                },
                BootPhase {
                    phase: "agentd",
                    bucket: "<1s"
                }
            ],
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
        assert_eq!(
            batch.boot_ms,
            vec![
                BootPhase {
                    phase: "issue",
                    bucket: "<1s"
                },
                BootPhase {
                    phase: "vm-boot",
                    bucket: "1-2s"
                }
            ],
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
        assert_eq!(
            batch.colonies.parallel_now, "2-3",
            "live colonies only; queued holds no microVM"
        );
        assert_eq!(batch.colonies.terminal.pr_opened, "2-3");
        assert_eq!(batch.colonies.terminal.no_changes, "0");
        assert_eq!(batch.colonies.terminal.stopped, "1");
        assert_eq!(batch.colonies.terminal.failed, "1");
        assert_eq!(batch.providers, "2-3", "three providers, bucketed like any other count");
        // publish.autopilot's schema default is on, and nothing in this install overrides it.
        assert_eq!(
            batch.autopilot,
            Autopilot {
                enabled: true,
                held: "0"
            }
        );
        assert_eq!(
            batch.settings_set,
            Vec::<String>::new(),
            "an install that set nothing names nothing"
        );
    }
}
