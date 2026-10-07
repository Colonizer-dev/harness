//! The watchdog's remediation playbook (issue #1191): a table of known stall signatures, each with
//! the one action that fixes it and a bounded number of tries.
//!
//! Before this, a person (or an outside agent polling every ten minutes) read `harness.jsonl`,
//! recognised a known pattern, and sent the colony the exact fix message. The playbook is that
//! operator step, done by the mothership itself:
//!
//! | Signature | Trigger | Action |
//! |---|---|---|
//! | `placeholder_dotfiles` | `secret-paths` denial naming only a harness placeholder | message |
//! | `pr_md_write` | `writes-outside-repo` denial on `/harness/out/pr.md` | message |
//! | `toolchain_installer` | `script-egress` denial on a toolchain installer | message |
//! | `provider_unavailable` | `unrecognized_model` hold, or a quota flag, with a healthy fallback | switch and resume |
//! | `idle_verified` | idle after `pr.md` with a confirmed verification | publish |
//!
//! A signature that comes back after its tries are spent (the colony is looping) stops the colony
//! and flags it `looping`. Rows handled elsewhere stay there: contradicted verification is #1186,
//! a redacted `pr.md` is #1175, a question left open is #1189.
//!
//! The table is data. The defaults are compiled in ([`defaults`]); `<config>/playbook.toml`
//! adds entries, replaces a default by naming the same `signature`, or switches one off with
//! `enabled = false` (docs/colonies.md, "Self-healing"). A new pattern needs no release.
//!
//! What the playbook never does: release a security hold. A colony carrying a control-defeat flag
//! (or any attention reason that names a secret, a redaction or a defeat) is left alone, and no
//! action here clears an attention flag it did not set.

use crate::{
    Shared,
    boundary::Boundary,
    protocol::Origin,
    sessions::{Runtime, Session, SessionStatus},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

/// The override file, in the config directory.
pub const PLAYBOOK_FILE: &str = "playbook.toml";
/// How many auto-fix records a colony keeps (the tries are counted from them).
pub const KEPT_FIXES: usize = 32;
/// The attention reason a looping stop raises.
pub const LOOPING_REASON: &str = "looping";
/// The signature a looping stop is recorded under; the record's `detail` names the looping one.
pub const LOOPING_SIGNATURE: &str = "looping";
/// How long after a fix the same signature is not acted on again: the agent needs time to read the
/// message, so a denial already in flight is not "the same denial after the playbook message".
pub const DEFAULT_SETTLE_SECS: u64 = 120;
/// Minutes a colony sits idle, verified, with a `pr.md`, before the playbook publishes it.
pub const DEFAULT_IDLE_MINUTES: u64 = 10;

/// One thing the playbook did for a colony: the cockpit's "auto-fixed: …" line and the try counter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutoFix {
    pub signature: String,
    pub action: String,
    pub at: DateTime<Utc>,
    /// One line for a person: what was sent, switched to, or stopped. Never a secret.
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Send the colony `message` as a user message, the path every watchdog message takes.
    SendMessage,
    /// Publish the colony's work (the verified, idle case).
    Publish,
    /// Move the colony to the failing provider's fallback model and resume it.
    SwitchFallbackAndResume,
    /// Stop the colony, free its slot and flag it `looping`.
    StopLooping,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::SendMessage => "send_message",
            Action::Publish => "publish",
            Action::SwitchFallbackAndResume => "switch_fallback_and_resume",
            Action::StopLooping => "stop_looping",
        }
    }
}

/// What kind of evidence an entry reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// A `boundary` event, as it arrives.
    #[default]
    Denial,
    /// A colony held on a turn error, or flagged for quota, with the provider named.
    ProviderFailure,
    /// An idle colony with a confirmed verification and a `pr.md`.
    IdleVerified,
}

/// The conditions of an entry. Every field that is set must hold; text is matched case-insensitively.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Matcher {
    /// The boundary kind; `exec_policy_deny` when absent.
    pub kind: Option<String>,
    /// A substring of the boundary's control (`secret-paths` for `exec_policy:secret-paths`).
    pub control: Option<String>,
    /// At least one of these appears in the boundary's detail or target.
    pub text_any: Vec<String>,
    /// None of these appears there.
    pub text_none: Vec<String>,
    /// The target is a relative path whose file name is one of these (the harness placeholders).
    pub target_in: Vec<String>,
    /// For a provider failure: at least one appears in the hold's detail (`unrecognized_model`).
    pub error_any: Vec<String>,
    /// For a provider failure: a quota flag matches too.
    pub quota: bool,
}

fn one() -> u32 {
    1
}
fn settle() -> u64 {
    DEFAULT_SETTLE_SECS
}
fn idle() -> u64 {
    DEFAULT_IDLE_MINUTES
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Entry {
    pub signature: String,
    #[serde(default)]
    pub trigger: Trigger,
    pub action: Action,
    /// The message `send_message` sends. Written for the agent: no secrets, no host paths.
    #[serde(default)]
    pub message: String,
    #[serde(default = "one")]
    pub max_tries: u32,
    #[serde(default = "settle")]
    pub settle_secs: u64,
    /// When the tries are spent and the signature comes back, stop the colony (`looping`).
    #[serde(default)]
    pub stop_when_exhausted: bool,
    /// For `idle_verified`: minutes idle before acting.
    #[serde(default = "idle")]
    pub idle_minutes: u64,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, rename = "when")]
    pub when: Matcher,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct File {
    /// Drop every compiled-in entry; the file is then the whole table.
    replace: bool,
    entry: Vec<Entry>,
}

const PLACEHOLDER_MESSAGE: &str = "Playbook: the empty `.env`-style files in your worktree are placeholders the harness \
    mounts on purpose, so the real secrets stay out of reach. Leave them alone (do not read, write or delete them) and \
    continue with the issue.";
const PR_MD_MESSAGE: &str = "Playbook: writing /harness/out/pr.md through the shell was refused. Write that file with \
    your file-write tool instead (not a redirect, cp or tee), then continue.";
const TOOLCHAIN_MESSAGE: &str = "Playbook: installing a toolchain (rustup, swift and the like) is refused here and will \
    stay refused. Do not install toolchains. Finish what you can without them, and say in /harness/out/pr.md which \
    checks you could not compile or run.";

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The compiled-in table.
pub fn defaults() -> Vec<Entry> {
    let denial = |signature: &str, control: &str, message: &str, when: Matcher| Entry {
        signature: signature.into(),
        trigger: Trigger::Denial,
        action: Action::SendMessage,
        message: message.into(),
        max_tries: 1,
        settle_secs: DEFAULT_SETTLE_SECS,
        stop_when_exhausted: true,
        idle_minutes: DEFAULT_IDLE_MINUTES,
        enabled: true,
        when: Matcher {
            control: Some(control.into()),
            ..when
        },
    };
    vec![
        denial(
            "placeholder_dotfiles",
            "secret-paths",
            PLACEHOLDER_MESSAGE,
            Matcher {
                target_in: strings(&[
                    ".env",
                    ".env.local",
                    ".env.development",
                    ".env.production",
                    ".env.test",
                    ".envrc",
                    ".netrc",
                    "_netrc",
                    ".npmrc",
                    ".pypirc",
                    ".git-credentials",
                    ".pgpass",
                ]),
                // Anything that also names a real credential location is not "only placeholders".
                text_none: strings(&[
                    ".ssh",
                    "id_rsa",
                    "id_ed25519",
                    ".aws",
                    ".gnupg",
                    ".kube",
                    "/etc/",
                    "/root",
                    "~/",
                    "$home",
                ]),
                ..Matcher::default()
            },
        ),
        denial(
            "pr_md_write",
            "writes-outside-repo",
            PR_MD_MESSAGE,
            Matcher {
                text_any: strings(&["/harness/out/pr.md"]),
                ..Matcher::default()
            },
        ),
        denial(
            "toolchain_installer",
            "script-egress",
            TOOLCHAIN_MESSAGE,
            Matcher {
                text_any: strings(&[
                    "rustup",
                    "sh.rustup.rs",
                    "swift.org",
                    "swiftly",
                    "ghcup",
                    "get.sdkman.io",
                    "dotnet-install",
                    "pyenv-installer",
                    "get.docker.com",
                ]),
                ..Matcher::default()
            },
        ),
        Entry {
            signature: "provider_unavailable".into(),
            trigger: Trigger::ProviderFailure,
            action: Action::SwitchFallbackAndResume,
            message: String::new(),
            max_tries: 2,
            settle_secs: 300,
            stop_when_exhausted: false,
            idle_minutes: DEFAULT_IDLE_MINUTES,
            enabled: true,
            when: Matcher {
                error_any: strings(&["unrecognized_model"]),
                quota: true,
                ..Matcher::default()
            },
        },
        Entry {
            signature: "idle_verified".into(),
            trigger: Trigger::IdleVerified,
            action: Action::Publish,
            message: String::new(),
            max_tries: 1,
            settle_secs: 600,
            stop_when_exhausted: false,
            idle_minutes: DEFAULT_IDLE_MINUTES,
            enabled: true,
            when: Matcher::default(),
        },
    ]
}

/// The compiled-in table with `file`'s entries applied: a same-named entry replaces the default,
/// a new name is added, and `replace = true` starts from nothing. Disabled entries are dropped.
fn merge(file: File) -> Vec<Entry> {
    let mut table = if file.replace { Vec::new() } else { defaults() };
    for entry in file.entry {
        match table.iter().position(|e| e.signature == entry.signature) {
            Some(at) => table[at] = entry,
            None => table.push(entry),
        }
    }
    table.retain(|e| e.enabled);
    table
}

/// Reads the table: the defaults and `<config_dir>/playbook.toml` over them. A file that cannot be
/// read or parsed leaves the defaults standing; the second value says why, for a log line.
pub fn load(config_dir: &Path) -> (Vec<Entry>, Option<String>) {
    let path = config_dir.join(PLAYBOOK_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (merge(File::default()), None),
        Err(e) => return (merge(File::default()), Some(format!("{}: {e}", path.display()))),
    };
    parse(&text)
}

fn parse(text: &str) -> (Vec<Entry>, Option<String>) {
    match toml::from_str::<File>(text) {
        Ok(file) => (merge(file), None),
        Err(e) => (merge(File::default()), Some(format!("{PLAYBOOK_FILE}: {e}"))),
    }
}

/* ------------------------------------------------------------------ matching */

/// Whether a boundary event is this entry's signature. Pure.
pub fn matches_denial(entry: &Entry, boundary: &Boundary) -> bool {
    if entry.trigger != Trigger::Denial || !entry.enabled {
        return false;
    }
    let when = &entry.when;
    if boundary.kind != when.kind.as_deref().unwrap_or("exec_policy_deny") {
        return false;
    }
    if let Some(control) = &when.control
        && !boundary.control.to_lowercase().contains(&control.to_lowercase())
    {
        return false;
    }
    let text = format!("{} {}", boundary.detail, boundary.target.as_deref().unwrap_or_default()).to_lowercase();
    let has = |needle: &String| text.contains(&needle.to_lowercase());
    if !when.text_any.is_empty() && !when.text_any.iter().any(has) {
        return false;
    }
    if when.text_none.iter().any(has) {
        return false;
    }
    if !when.target_in.is_empty() {
        let Some(target) = boundary.target.as_deref() else {
            return false;
        };
        let target = target.strip_prefix("./").unwrap_or(target);
        if target.starts_with('/') || target.starts_with('~') || target.split('/').any(|part| part == "..") {
            return false;
        }
        let name = target.rsplit('/').next().unwrap_or(target);
        if !when.target_in.iter().any(|n| n == name) {
            return false;
        }
    }
    true
}

/// Whether a hold or flag on a colony is this entry's provider failure, and the provider it names.
/// `ids` are the configured provider ids. Pure.
pub fn failing_provider(entry: &Entry, s: &Session, ids: &[String]) -> Option<String> {
    if entry.trigger != Trigger::ProviderFailure || !entry.enabled {
        return None;
    }
    let attention = s.attention.as_ref()?;
    let reason = attention["reason"].as_str()?;
    if reason == crate::provider_quota::QUOTA_EXHAUSTED_REASON {
        return entry
            .when
            .quota
            .then(|| crate::quota_cards::flagged_provider(s, ids))
            .flatten();
    }
    if reason == crate::queue::AUTOPILOT_HELD_REASON && attention["cause"] == crate::events::TURN_ERROR_CAUSE {
        let detail = attention["detail"].as_str().unwrap_or_default().to_lowercase();
        if !entry.when.error_any.iter().any(|e| detail.contains(&e.to_lowercase())) {
            return None;
        }
        return crate::provider_quota::mentioned_provider(&detail, ids, &[]).or_else(|| {
            crate::provider_quota::routed_provider(None, s.allowed_providers.as_deref(), s.model_usage.as_ref(), ids)
        });
    }
    None
}

/// The model to move a colony to when `provider` fails: its `<provider>/<model>` fallback, if that
/// provider is configured and not itself out of quota. Pure over the gateway's verdict.
pub fn healthy_fallback(
    provider: &str,
    providers: &[crate::providers::Provider],
    exhausted: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let failing = providers.iter().find(|p| p.id == provider)?;
    let (fallback_provider, _) = failing.provider_fallback()?;
    if fallback_provider == provider || exhausted(fallback_provider) || !providers.iter().any(|p| p.id == fallback_provider) {
        return None;
    }
    failing.fallback_model.clone()
}

/// Whether an attention flag is a security hold the playbook must leave to a person.
pub fn is_security_hold(attention: Option<&Value>) -> bool {
    let Some(reason) = attention.and_then(|a| a["reason"].as_str()) else {
        return false;
    };
    reason == crate::watchdog::CONTROL_DEFEAT_REASON
        || ["secret", "redact", "defeat", "security", "credential"]
            .iter()
            .any(|word| reason.contains(word))
}

/// What the table says to do about a signature that matched now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Do it; this is try number `attempt`.
    Act { attempt: u32 },
    /// The last fix was too recent for the colony to have seen it.
    Settling,
    /// The tries are spent and the signature is back: the colony is looping.
    Stop,
    /// The tries are spent and nothing more is asked of the playbook.
    Spent,
}

/// Bounded tries: the entry's own fixes since the colony last had a looping stop for it, counted
/// against `max_tries`, with `settle_secs` between them. Pure, with the clock passed in.
pub fn step(entry: &Entry, fixes: &[AutoFix], now: DateTime<Utc>) -> Step {
    let reset = fixes
        .iter()
        .rev()
        .find(|f| f.signature == LOOPING_SIGNATURE && f.detail == entry.signature)
        .map(|f| f.at);
    let mine: Vec<&AutoFix> = fixes
        .iter()
        .filter(|f| f.signature == entry.signature && reset.is_none_or(|at| f.at > at))
        .collect();
    if let Some(last) = mine.last()
        && now - last.at < Duration::seconds(entry.settle_secs as i64)
    {
        return Step::Settling;
    }
    let used = mine.len() as u32;
    if used < entry.max_tries {
        Step::Act { attempt: used + 1 }
    } else if entry.stop_when_exhausted {
        Step::Stop
    } else {
        Step::Spent
    }
}

/// Whether an idle colony is ready for the playbook's publish: autopilot on, idle with nothing
/// flagged, no pull request or publish yet, a `pr.md` written, a confirmed verification, and quiet
/// for `idle_minutes`. Pure.
pub fn idle_publish_ready(
    s: &Session,
    pr_written: bool,
    last_progress: DateTime<Utc>,
    now: DateTime<Utc>,
    idle_minutes: u64,
) -> bool {
    s.status == SessionStatus::Idle
        && s.autopilot
        && s.attention.is_none()
        && s.suspended.is_none()
        && s.pr_url.is_none()
        && s.publish_stage.is_none()
        && pr_written
        && s.verification
            .as_ref()
            .is_some_and(|v| v.verdict == crate::verify::Verdict::Confirmed)
        && now - last_progress >= Duration::minutes(idle_minutes as i64)
}

/* ---------------------------------------------------------------------- glue */

fn table(app: &Shared) -> Vec<Entry> {
    let (entries, warning) = load(&app.cfg.config_dir);
    if let Some(warning) = warning {
        eprintln!("playbook: ignoring {warning}; using the compiled-in table");
    }
    entries
}

/// Records a fix on the colony and says so in its log: `auto-fixed: <signature>`.
async fn record(app: &Shared, id: &str, signature: &str, action: Action, detail: String) {
    let fix = AutoFix {
        signature: signature.into(),
        action: action.as_str().into(),
        at: Utc::now(),
        detail: crate::util::truncate(&detail, 240),
    };
    let line = format!("auto-fixed: {signature}");
    app.update_session(id, |x| {
        x.auto_fixes.push(fix);
        let extra = x.auto_fixes.len().saturating_sub(KEPT_FIXES);
        x.auto_fixes.drain(..extra);
    })
    .await;
    app.session_log_as(Origin::Watchdog, id, "info", format!("{line} ({})", action.as_str()))
        .await;
}

/// A denial arrived (called from `boundary::observe` once the control-defeat signature has had its
/// say). Matches it against the table and acts: the entry's message the first time, nothing while
/// the colony is still reading it, a stop when the same denial comes back after the tries are spent.
pub(crate) async fn on_boundary(app: &Shared, id: &str, boundary: &Boundary) {
    if boundary.kind != "exec_policy_deny" && boundary.kind != "egress_denied" && boundary.kind != "path_policy_denied" {
        return;
    }
    let Some(s) = app.session(id).await else { return };
    if !s.status.is_live() || s.suspended.is_some() || is_security_hold(s.attention.as_ref()) {
        return;
    }
    let entries = table(app);
    let Some(entry) = entries.iter().find(|e| matches_denial(e, boundary)) else {
        return;
    };
    match step(entry, &s.auto_fixes, Utc::now()) {
        Step::Act { attempt } => {
            let rt = app.runtime(id).await;
            perform(app, &s, &rt, entry, attempt).await;
        }
        Step::Stop => stop_looping(app, &s, &entry.signature).await,
        Step::Settling | Step::Spent => {}
    }
}

/// The once-a-minute pass over colonies in play: the provider-failure rows, for live and parked
/// colonies alike (a quota park is exactly where a fallback helps).
pub(crate) async fn tick_providers(app: &Shared, s: &Session, now: DateTime<Utc>) -> bool {
    if s.cleaned_up
        || s.suspended.is_some()
        || !(s.status.is_live() || s.status == SessionStatus::Parked)
        || is_security_hold(s.attention.as_ref())
    {
        return false;
    }
    let entries = table(app);
    let providers = app.providers();
    let ids: Vec<String> = providers.iter().map(|p| p.id.clone()).collect();
    for entry in &entries {
        let Some(provider) = failing_provider(entry, s, &ids) else {
            continue;
        };
        let Some(model) = healthy_fallback(&provider, &providers, &|p| app.gateway.is_quota_exhausted(p)) else {
            continue;
        };
        if let Step::Act { attempt } = step(entry, &s.auto_fixes, now) {
            let rt = app.runtime(&s.id).await;
            switch_fallback(app, s, &rt, entry, attempt, &provider, &model).await;
        }
        return true;
    }
    false
}

/// The once-a-minute pass over a live colony: the idle-verified publish row. Returns whether the
/// playbook claimed the colony this tick (it is then not an unmatched stall).
pub(crate) async fn tick_idle(app: &Shared, s: &Session, rt: &Arc<Runtime>, now: DateTime<Utc>) -> bool {
    if !s.status.is_live() || is_security_hold(s.attention.as_ref()) {
        return false;
    }
    let entries = table(app);
    for entry in entries.iter().filter(|e| e.trigger == Trigger::IdleVerified && e.enabled) {
        let last = rt.activity.lock().await.progress_reference();
        let pr_written = matches!(app.store().read_file(&s.id, "out/pr.md").await, Ok(Some(bytes)) if !bytes.is_empty());
        if !idle_publish_ready(s, pr_written, last, now, entry.idle_minutes) {
            continue;
        }
        if let Step::Act { attempt } = step(entry, &s.auto_fixes, now) {
            perform(app, s, rt, entry, attempt).await;
        }
        return true;
    }
    false
}

/// Where a stall nobody matched goes (issue #1192): the watchdog calls this when it is about to
/// send its generic nudge and no playbook row claimed the colony. Today it does nothing, so the
/// generic nudge and the flag run as they always have; the operator agent plugs in here.
pub(crate) async fn on_unmatched_stall(_app: &Shared, _session: &Session) {}

async fn perform(app: &Shared, s: &Session, rt: &Arc<Runtime>, entry: &Entry, attempt: u32) {
    match entry.action {
        Action::SendMessage => {
            if entry.message.trim().is_empty() {
                return;
            }
            crate::recovery::send_user_message(rt, "playbook", &entry.message);
            record(
                app,
                &s.id,
                &entry.signature,
                entry.action,
                format!("sent the playbook message (try {attempt} of {})", entry.max_tries),
            )
            .await;
        }
        Action::Publish => match publish_verified(app, s).await {
            Ok(()) => {
                record(
                    app,
                    &s.id,
                    &entry.signature,
                    entry.action,
                    "published the verified work".to_string(),
                )
                .await
            }
            Err(why) => {
                app.session_log_as(
                    Origin::Watchdog,
                    &s.id,
                    "info",
                    format!("playbook: {} not applied: {why}", entry.signature),
                )
                .await
            }
        },
        Action::StopLooping => stop_looping(app, s, &entry.signature).await,
        // Needs the failing provider, which only the provider-failure pass knows.
        Action::SwitchFallbackAndResume => {}
    }
}

/// Publishes a colony whose verification confirmed its claim, the way autopilot's own confirmed
/// verdict does: the host verifier approves, bound to the tree it would commit now. Refuses when
/// the tree is no longer the one that was verified, or the kill-switch or GitHub's breaker is up.
async fn publish_verified(app: &Shared, s: &Session) -> Result<(), String> {
    if crate::authority::external_writes_blocked() {
        return Err("external writes are blocked".into());
    }
    if crate::github_breaker::hold_publish(app, &s.id).is_some() {
        return Err("GitHub is refusing the account; the publish is held".into());
    }
    let admin = s.git_admin_dir.as_deref().ok_or("no worktree")?;
    let snapshot = s
        .verification
        .as_ref()
        .and_then(|v| v.snapshot.clone())
        .ok_or("no verified snapshot")?;
    let verified = crate::util::exec(app.git(Path::new(admin)).args(["rev-parse", &format!("{snapshot}^{{tree}}")]))
        .await
        .map_err(|e| format!("could not read the verified tree: {e:#}"))?;
    let now = crate::github::approval_candidate_tree(app, s)
        .await
        .map_err(|e| format!("could not bind the publish: {e:#}"))?;
    if verified.trim() != now.trim() {
        return Err("the work changed after it was verified".into());
    }
    let grant = crate::publish::mint_publish_grant(app, s, "host-verifier")
        .await
        .map_err(|e| format!("could not bind the publish approval: {e:#}"))?;
    app.update_session(&s.id, |x| x.clear_hold_cause()).await;
    crate::publish::spawn_publish(app.clone(), s.id.clone(), grant);
    Ok(())
}

async fn switch_fallback(
    app: &Shared,
    s: &Session,
    _rt: &Arc<Runtime>,
    entry: &Entry,
    attempt: u32,
    provider: &str,
    model: &str,
) {
    // Recorded first: a resume that fails half way must not be retried without bound.
    record(
        app,
        &s.id,
        &entry.signature,
        entry.action,
        format!(
            "{provider} unavailable; switched to {model} and resumed (try {attempt} of {})",
            entry.max_tries
        ),
    )
    .await;
    if let Err(e) = crate::quota_cards::switch_colony(app, provider, model, s).await {
        app.session_log_as(
            Origin::Watchdog,
            &s.id,
            "warn",
            format!("playbook: switching to {model} did not complete: {e}"),
        )
        .await;
    }
}

/// Stops a looping colony, frees its slot, and flags it `looping` so it reads as needing a person.
async fn stop_looping(app: &Shared, s: &Session, signature: &str) {
    let warn = format!("playbook: `{signature}` came back after the playbook message; stopping the colony so its slot is free");
    let stopped = crate::lifecycle::stop_colony(
        app,
        s,
        |x| x.status.is_live() && !is_security_hold(x.attention.as_ref()),
        format!("looping on {signature}"),
        warn,
    )
    .await;
    if !stopped {
        return;
    }
    let since = Utc::now();
    app.update_session(&s.id, |x| {
        x.attention = Some(json!({
            "reason": LOOPING_REASON,
            "since": since,
            "nudges": 0,
            "signature": signature,
            "detail": format!("`{signature}` kept happening after the playbook told the agent what to do"),
        }));
    })
    .await;
    record(app, &s.id, LOOPING_SIGNATURE, Action::StopLooping, signature.to_string()).await;
}

#[cfg(test)]
mod tests;
