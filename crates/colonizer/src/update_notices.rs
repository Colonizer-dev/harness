//! Why an update matters here, and the colonies it left on the old version (issue #1097).
//!
//! Three things sit on top of the update check (`version.rs`) and the in-place update (`update.rs`):
//!
//! * **Notices.** A changelog fragment can be marked `critical` or `fixes-running`, with one line
//!   written for the operator (`changelog.d/README.md`). The release workflow puts those lines into
//!   the release body twice: as a visible list, and as a JSON block inside an HTML comment that
//!   starts with [`MARKER`], which [`parse`] reads. The block also carries the notices of the
//!   releases before it, each with the version it shipped in, so a mothership two releases behind
//!   still hears about a fix in the one between. `/api/update` answers the ones newer than this
//!   build as `notices`, and the cockpit shows them as a banner rather than a quiet badge.
//! * **"Affected here" probes.** A notice can name a probe: a read-only check this mothership runs
//!   on its own disk to count the colonies the fix is for. Probes are compiled into the harness
//!   ([`PROBES`]); a release cannot send code to run, only the id of a check the running build
//!   already has, and an id this build does not know is answered as `affected: null`. Nothing a
//!   probe reads leaves the machine. The first one, [`MSB_BODY_SECRET_VIOLATION`], is issue #1096:
//!   msb 0.7.3's credential scanner blocking a request whose body it misread.
//! * **Colonies still on the previous version.** A colony's microVM keeps the vendored components
//!   (msb, plugins, the agent modules) of the app slot it booted from (`Session::app_slot`) until it
//!   is stopped and resumed, so after an update a colony stuck on a fixed bug stays stuck. `behind`
//!   lists them, and `POST /api/update/restart` restarts them on the new version with the same stop
//!   then resume the cockpit's own buttons use, which keeps the worktree and the conversation.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{
    Shared,
    sessions::{Session, SessionStatus},
    util, version,
};

/// Opens the machine-readable block in a release body; the JSON array runs up to the next `-->`.
pub const MARKER: &str = "<!-- colonizer:notices";

/// No release body is allowed to make the mothership hold or show more than this.
const MAX_NOTICES: usize = 50;
const MAX_LINE: usize = 300;

/// How much of a `runtime.log` a probe reads: its tail, so a long-lived sandbox costs the same.
const LOG_TAIL: u64 = 1 << 20;

/// How long a probe's answer is reused. The Settings pane polls `/api/update` every couple of
/// seconds while an update runs; reading every sandbox's log on each poll would be waste.
const PROBE_TTL: Duration = Duration::from_secs(30);

/// Issue #1096: msb 0.7.3 sometimes scans header bytes as body, finds the credential placeholder of
/// the colony's own `Authorization` header there, and blocks the request; the colony's
/// `runtime.log` records it as `secret violation … location=body`.
pub const MSB_BODY_SECRET_VIOLATION: &str = "msb-body-secret-violation";

/// Every probe this build can run. A notice naming anything else is shown without a count.
pub const PROBES: &[&str] = &[MSB_BODY_SECRET_VIOLATION];

/// What switches a development or source build over to releases, when the in-place update cannot.
pub const INSTALL_COMMAND: &str = "curl -fsSL https://colonizer.dev/install.sh | sh";

/* ----------------------------------------------------------------- notices */

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Fixes something that loses work or blocks colonies outright.
    Critical,
    /// Fixes a bug that colonies already running may be hitting; they need a restart to get it.
    FixesRunning,
}

/// One operator-facing line a release carries about itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    /// The release the fix shipped in.
    pub version: String,
    pub severity: Severity,
    /// "Fixes colonies failing with UND_ERR_SOCKET (sandbox credential scanner)".
    pub line: String,
    /// The id of a probe in [`PROBES`] that counts the colonies this fix is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<u64>,
}

fn valid_probe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The notices in a release body, or none. Tolerant: a release written before notices existed has
/// no block, and an entry this build cannot read is skipped rather than failing the whole check.
pub fn parse(body: &str) -> Vec<Notice> {
    let Some(start) = body.find(MARKER) else {
        return Vec::new();
    };
    let rest = &body[start + MARKER.len()..];
    let Some(end) = rest.find("-->") else {
        return Vec::new();
    };
    let Ok(items) = serde_json::from_str::<Vec<Value>>(rest[..end].trim()) else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| serde_json::from_value::<Notice>(item).ok())
        .filter(|n| version::Semver::parse(&n.version).is_some() && !n.line.trim().is_empty())
        .map(|mut n| {
            n.line = util::truncate(n.line.trim(), MAX_LINE);
            n.probe = n.probe.filter(|p| valid_probe_id(p));
            n
        })
        .take(MAX_NOTICES)
        .collect()
}

/// The release body without the machine-readable block, for showing as notes.
pub fn strip(body: &str) -> String {
    let Some(start) = body.find(MARKER) else {
        return body.to_string();
    };
    let tail = &body[start..];
    let end = tail.find("-->").map(|i| start + i + 3).unwrap_or(body.len());
    format!("{}{}", &body[..start], &body[end..]).trim().to_string()
}

/// Whether a notice is news to this build: shipped after the release it contains, and no later
/// than the latest release (a block only ever names releases up to its own, but a bad one must not
/// promise a fix nothing installable has).
pub fn pending(notice: &Notice, installed_release: Option<&str>, latest: &str) -> bool {
    version::is_newer(installed_release, &notice.version) && !version::is_newer(Some(latest), &notice.version)
}

/* ------------------------------------------------------------------ probes */

/// Whether this colony has a microVM running right now, booted from whatever slot it names: a live
/// one, or one parked with its microVM kept (issue #213). `Starting` is left out: a boot in flight
/// records the slot it is booting from when it gets there, and one interrupted by the update's
/// restart is requeued onto the new version anyway.
fn runs_a_vm(s: &Session) -> bool {
    match s.status {
        SessionStatus::Starting => false,
        status if status.is_live() => true,
        SessionStatus::Parked => s.parked.as_ref().is_some_and(|p| p.vm_kept),
        _ => false,
    }
}

/// `sandboxes/<name>/logs/runtime.log` under msb's home, for a name that is one plain path segment.
fn runtime_log(msb_home: &Path, sandbox: &str) -> Option<PathBuf> {
    let plain = !sandbox.is_empty() && sandbox != "." && sandbox != ".." && !sandbox.contains(['/', '\\', '\0']);
    plain.then(|| msb_home.join("sandboxes").join(sandbox).join("logs").join("runtime.log"))
}

/// The last [`LOG_TAIL`] bytes of a file, lossily decoded; `None` when it cannot be read.
fn read_tail(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > LOG_TAIL {
        file.seek(SeekFrom::Start(len - LOG_TAIL)).ok()?;
    }
    let mut bytes = Vec::new();
    file.take(LOG_TAIL).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Issue #1096's signature: a `secret violation` that msb placed in the request body.
pub fn msb_blocked_a_body(log: &str) -> bool {
    log.lines()
        .any(|line| line.contains("secret violation") && line.contains("location=body"))
}

/// Runs probe `id` over `targets` (colony id, sandbox name): the colonies it matches, or `None` for
/// a probe this build does not have. Read-only and local.
pub fn run_probe(id: &str, msb_home: Option<&Path>, targets: &[(String, String)]) -> Option<Vec<String>> {
    match id {
        MSB_BODY_SECRET_VIOLATION => {
            let Some(home) = msb_home else { return Some(Vec::new()) };
            Some(
                targets
                    .iter()
                    .filter(|(_, sandbox)| {
                        runtime_log(home, sandbox)
                            .and_then(|log| read_tail(&log))
                            .is_some_and(|log| msb_blocked_a_body(&log))
                    })
                    .map(|(colony, _)| colony.clone())
                    .collect(),
            )
        }
        _ => None,
    }
}

/* -------------------------------------------------------------- the state */

struct Cached {
    at: Instant,
    key: String,
    hits: BTreeMap<String, Vec<String>>,
}

/// The restarts asked for through `POST /api/update/restart`, while they run and when one failed.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Restarts {
    pub restarting: BTreeSet<String>,
    pub failed: BTreeMap<String, String>,
}

/// Held by `update::Updater`, so it lives as long as the process: a probe cache and the restarts.
#[derive(Default)]
pub struct NoticeState {
    probes: Mutex<Option<Cached>>,
    restarts: Mutex<Restarts>,
}

/// Every probe the notices name that this build has, run over the colonies with a microVM:
/// probe id → the colonies it matched. Cached for [`PROBE_TTL`] per set of probes and colonies.
async fn run_probes(app: &Shared, notices: &[Notice], sessions: &[Session]) -> BTreeMap<String, Vec<String>> {
    let probes: BTreeSet<String> = notices
        .iter()
        .filter_map(|n| n.probe.clone())
        .filter(|p| PROBES.contains(&p.as_str()))
        .collect();
    if probes.is_empty() {
        return BTreeMap::new();
    }
    let targets: Vec<(String, String)> = sessions
        .iter()
        .filter(|s| runs_a_vm(s))
        .map(|s| (s.id.clone(), s.sandbox.clone()))
        .collect();
    let key = format!("{probes:?}|{targets:?}");
    let state = &app.updater.notices;
    {
        let cached = state.probes.lock().await;
        if let Some(cached) = cached.as_ref()
            && cached.key == key
            && cached.at.elapsed() < PROBE_TTL
        {
            return cached.hits.clone();
        }
    }
    let hits = tokio::task::spawn_blocking(move || {
        let home = crate::reclaim::microsandbox_home();
        probes
            .into_iter()
            .filter_map(|probe| run_probe(&probe, home.as_deref(), &targets).map(|hits| (probe, hits)))
            .collect::<BTreeMap<_, _>>()
    })
    .await
    .unwrap_or_default();
    *state.probes.lock().await = Some(Cached {
        at: Instant::now(),
        key,
        hits: hits.clone(),
    });
    hits
}

/* ------------------------------------------------------ behind and switch */

fn resolved(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The colonies whose microVM still runs on an app slot other than `current`, the one this process
/// runs from. A record with no slot (booted before the field existed, or with no app directory) is
/// not guessed at; with no `current` either, as in a build run from a checkout, nothing is behind.
pub fn behind<'a>(sessions: &'a [Session], current: Option<&Path>) -> Vec<&'a Session> {
    let Some(current) = current.map(resolved) else {
        return Vec::new();
    };
    sessions
        .iter()
        .filter(|s| runs_a_vm(s))
        .filter(|s| s.app_slot.as_deref().is_some_and(|slot| resolved(Path::new(slot)) != current))
        .collect()
}

/// Why the cockpit cannot update this build in place, and the command that switches it to releases.
///
/// `blocked` is `update::blocker`'s answer. A build with no installer or no app symlink (a checkout,
/// `cargo install`) needs the release installer once; a development build that does sit in an
/// installed app can be replaced by the latest release with `colonizer update --force`.
pub fn switch_to_releases(build: &version::Build, blocked: Option<&str>) -> Option<Value> {
    if let Some(reason) = blocked {
        return Some(json!({
            "reason": reason,
            "command": INSTALL_COMMAND,
            "then": "Then start Colonizer from ~/.local/bin/colonizer. A release install updates itself from here.",
        }));
    }
    if build.development {
        return Some(json!({
            "reason": format!("{} is a development build, which holds work no release contains", build.version),
            "command": "colonizer update --force",
            "then": "It installs the latest release over this build, backing sessions.json up first. Later releases then update from here.",
        }));
    }
    None
}

/// The parts of `/api/update` this module adds: `notices`, `behind`, `restarts` and
/// `switch_to_releases`. Called from `version::full_status`.
pub async fn extend(app: &Shared, view: &mut Value) {
    let latest = app.updates.latest().await;
    let notices = latest.as_ref().map(|l| l.notices.clone()).unwrap_or_default();
    let build = version::build();
    let sessions = app.sessions.read().await.clone();
    let hits = run_probes(app, &notices, &sessions).await;

    let pending: Vec<Value> = match latest.as_ref() {
        Some(latest) => notices
            .iter()
            .filter(|n| pending(n, build.release.as_deref(), &latest.version))
            .map(|n| {
                let mut out = serde_json::to_value(n).unwrap_or(Value::Null);
                out["affected"] = match n.probe.as_ref().and_then(|p| hits.get(p)) {
                    Some(ids) => json!({ "count": ids.len(), "colonies": ids }),
                    None => Value::Null,
                };
                out
            })
            .collect(),
        None => Vec::new(),
    };

    let behind: Vec<Value> = behind(&sessions, app.cfg.assets.as_deref())
        .into_iter()
        .map(|s| {
            // Every notice whose probe matched this colony, pending or already installed: after the
            // update the fix is here, and these are the colonies that still need the restart for it.
            let affected_by: BTreeSet<&str> = notices
                .iter()
                .filter(|n| {
                    n.probe
                        .as_ref()
                        .and_then(|p| hits.get(p))
                        .is_some_and(|ids| ids.contains(&s.id))
                })
                .map(|n| n.line.as_str())
                .collect();
            json!({
                "id": s.id,
                "repo": s.repo,
                "status": s.status,
                "slot": s.app_slot,
                "affected_by": affected_by,
            })
        })
        .collect();

    let blocked = crate::update::blocker(app.cfg.assets.as_deref());
    view["notices"] = Value::Array(pending);
    view["behind"] = Value::Array(behind);
    view["restarts"] = serde_json::to_value(app.updater.notices.restarts.lock().await.clone()).unwrap_or(Value::Null);
    view["switch_to_releases"] = switch_to_releases(build, blocked.as_deref()).unwrap_or(Value::Null);
}

/* ------------------------------------------------------------------ route */

#[derive(Default, Deserialize)]
pub struct RestartRequest {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    all: bool,
}

/// Which of the colonies asked for can be restarted on the new version, and why the rest cannot.
fn restart_targets(behind: &[&Session], request: &RestartRequest, running: &BTreeSet<String>) -> (Vec<String>, Vec<Value>) {
    let behind_ids: BTreeSet<&str> = behind.iter().map(|s| s.id.as_str()).collect();
    let asked: Vec<String> = if request.all {
        behind.iter().map(|s| s.id.clone()).collect()
    } else {
        let mut seen = BTreeSet::new();
        request.ids.iter().filter(|id| seen.insert(id.as_str())).cloned().collect()
    };
    let mut targets = Vec::new();
    let mut skipped = Vec::new();
    for id in asked {
        if running.contains(&id) {
            skipped.push(json!({ "id": id, "reason": "already restarting" }));
        } else if !behind_ids.contains(id.as_str()) {
            skipped.push(json!({ "id": id, "reason": "not running on a previous version" }));
        } else {
            targets.push(id);
        }
    }
    (targets, skipped)
}

/// `POST /api/update/restart` — `{"ids": [...]}` or `{"all": true}`: restart the colonies still on a
/// previous app slot so they boot on this version's components. Each is stopped (its microVM taken
/// down, the worktree kept) and resumed, one at a time, in the background; the answer names the ones
/// started and the ones skipped, and `GET /api/update` follows them as `restarts`.
pub async fn restart(State(app): State<Shared>, Json(request): Json<RestartRequest>) -> crate::ApiResult<Value> {
    if !request.all && request.ids.is_empty() {
        return Err(crate::client_error(
            StatusCode::BAD_REQUEST,
            "name the colonies to restart as {\"ids\": [...]}, or send {\"all\": true}",
        ));
    }
    if crate::update::applying(&app).await {
        return Err(crate::client_error(
            StatusCode::CONFLICT,
            "an update is being applied; restart colonies once the new version is running",
        ));
    }
    let sessions = app.sessions.read().await.clone();
    let behind = behind(&sessions, app.cfg.assets.as_deref());
    let (targets, skipped) = {
        let mut restarts = app.updater.notices.restarts.lock().await;
        let (targets, skipped) = restart_targets(&behind, &request, &restarts.restarting);
        for id in &targets {
            restarts.restarting.insert(id.clone());
            restarts.failed.remove(id);
        }
        (targets, skipped)
    };

    let background = app.clone();
    let queue = targets.clone();
    tokio::spawn(async move {
        for id in queue {
            let outcome = crate::quota_cards::restart(&background, &id).await;
            let mut restarts = background.updater.notices.restarts.lock().await;
            restarts.restarting.remove(&id);
            match outcome {
                Ok(()) => {
                    background
                        .session_log(&id, "info", "restarted on the new version after an update".into())
                        .await
                }
                Err(e) => {
                    restarts.failed.insert(id.clone(), util::truncate(&e, 500));
                }
            }
        }
    });

    Ok(Json(json!({ "restarting": targets, "skipped": skipped })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{Park, tests::colony};

    fn notice(version: &str, probe: Option<&str>) -> Notice {
        Notice {
            version: version.into(),
            severity: Severity::Critical,
            line: format!("fixed in {version}"),
            probe: probe.map(str::to_string),
            issue: Some(1096),
        }
    }

    fn body(json: &str) -> String {
        format!("## What's new\n\nSome notes.\n\n{MARKER}\n{json}\n-->\n\nPrebuilt Colonizer for Linux.")
    }

    #[test]
    fn a_release_body_without_a_block_has_no_notices() {
        assert!(parse("").is_empty());
        assert!(parse("Prebuilt Colonizer for Linux x86_64").is_empty());
        // An unterminated or malformed block is no notice, never an error.
        assert!(parse(&format!("{MARKER} [{{\"version\":\"v1.0.0\"")).is_empty());
        assert!(parse(&body("not json")).is_empty());
    }

    #[test]
    fn the_block_is_read_and_bad_entries_are_skipped() {
        let text = body(
            r#"[
              {"version":"v0.2.7","severity":"critical","line":" Fixes colonies failing with UND_ERR_SOCKET > ","probe":"msb-body-secret-violation","issue":1096},
              {"version":"v0.2.6","severity":"fixes-running","line":"Fixes a stuck merge train"},
              {"version":"v0.2.6","severity":"whatever","line":"unknown severity"},
              {"version":"latest","severity":"critical","line":"no version"},
              {"version":"v0.2.5","severity":"critical","line":"   "},
              {"version":"v0.2.5","severity":"critical","line":"bad probe","probe":"../../etc"}
            ]"#,
        );
        let notices = parse(&text);
        assert_eq!(notices.len(), 3, "{notices:?}");
        assert_eq!(notices[0].line, "Fixes colonies failing with UND_ERR_SOCKET >");
        assert_eq!(notices[0].probe.as_deref(), Some(MSB_BODY_SECRET_VIOLATION));
        assert_eq!(notices[0].issue, Some(1096));
        assert_eq!(notices[1].severity, Severity::FixesRunning);
        assert_eq!(notices[2].probe, None, "a probe id that is not one plain name is dropped");
    }

    #[test]
    fn the_notes_shown_leave_the_block_out() {
        let text = body(r#"[{"version":"v0.2.7","severity":"critical","line":"x"}]"#);
        let notes = strip(&text);
        assert!(!notes.contains("colonizer:notices"), "{notes}");
        assert!(
            notes.starts_with("## What's new") && notes.ends_with("Prebuilt Colonizer for Linux."),
            "{notes}"
        );
        assert_eq!(strip("plain"), "plain");
    }

    #[test]
    fn only_notices_after_this_build_and_up_to_the_latest_are_pending() {
        let installed = Some("v0.2.5");
        assert!(pending(&notice("v0.2.6", None), installed, "v0.2.7"), "a release in between");
        assert!(pending(&notice("v0.2.7", None), installed, "v0.2.7"), "the latest itself");
        assert!(!pending(&notice("v0.2.5", None), installed, "v0.2.7"), "already installed");
        assert!(
            !pending(&notice("v0.2.8", None), installed, "v0.2.7"),
            "newer than anything installable"
        );
        assert!(
            !pending(&notice("v0.2.6", None), None, "v0.2.7"),
            "an unplaceable build is not told"
        );
    }

    #[test]
    fn the_msb_probe_matches_a_body_violation_and_nothing_else() {
        let blocked = "2026-10-05T10:00:00Z WARN secret violation: placeholder detected for disallowed host action=block-and-log secret_env_var=CLAUDE_CODE_OAUTH_TOKEN sni=api.anthropic.com location=body match_form=percent_decoded";
        assert!(msb_blocked_a_body(&format!("boot ok\n{blocked}\n")));
        assert!(!msb_blocked_a_body("secret violation: placeholder detected location=header"));
        assert!(!msb_blocked_a_body("location=body without the violation"));
        assert!(!msb_blocked_a_body(""));
    }

    #[test]
    fn the_probe_reads_each_sandboxs_runtime_log() {
        let home = std::env::temp_dir().join(format!("colonizer-probe-{}", util::short_id()));
        let write = |sandbox: &str, text: &str| {
            let logs = home.join("sandboxes").join(sandbox).join("logs");
            std::fs::create_dir_all(&logs).unwrap();
            std::fs::write(logs.join("runtime.log"), text).unwrap();
        };
        write(
            "colony-a",
            "x secret violation: placeholder action=block-and-log location=body\n",
        );
        write("colony-b", "all quiet\n");
        // A long log whose only match is near its end is still found: the tail is what is read.
        write(
            "colony-c",
            &format!("{}\nsecret violation location=body\n", "filler line\n".repeat(200_000)),
        );
        let targets: Vec<(String, String)> = [
            ("a", "colony-a"),
            ("b", "colony-b"),
            ("c", "colony-c"),
            ("d", "missing"),
            ("e", "../colony-a"),
        ]
        .iter()
        .map(|(id, sandbox)| (id.to_string(), sandbox.to_string()))
        .collect();
        let hits = run_probe(MSB_BODY_SECRET_VIOLATION, Some(&home), &targets).unwrap();
        assert_eq!(
            hits,
            vec!["a".to_string(), "c".to_string()],
            "a path-like sandbox name is never followed"
        );
        assert_eq!(
            run_probe("not-a-probe", Some(&home), &targets),
            None,
            "an unknown probe has no answer"
        );
        assert_eq!(run_probe(MSB_BODY_SECRET_VIOLATION, None, &targets), Some(Vec::new()));
        std::fs::remove_dir_all(&home).ok();
    }

    fn on_slot(id: &str, status: SessionStatus, slot: Option<&Path>) -> Session {
        let mut s = colony("acme", status);
        s.id = id.into();
        s.sandbox = format!("colony-{id}");
        s.app_slot = slot.map(|p| p.display().to_string());
        s
    }

    #[test]
    fn colonies_running_on_another_slot_are_behind() {
        let root = std::env::temp_dir().join(format!("colonizer-behind-{}", util::short_id()));
        let (old, new) = (root.join("app-a"), root.join("app-b"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        use SessionStatus::*;
        let mut parked_warm = on_slot("parked-warm", Parked, Some(&old));
        parked_warm.parked = Some(Park {
            at: chrono::Utc::now(),
            reason: "hold_timeout".into(),
            resets_at: None,
            vm_kept: true,
            question_risk: None,
        });
        let mut parked_cold = parked_warm.clone();
        parked_cold.id = "parked-cold".into();
        parked_cold.parked.as_mut().unwrap().vm_kept = false;
        let sessions = vec![
            on_slot("old-running", Running, Some(&old)),
            on_slot("old-idle", Idle, Some(&old)),
            on_slot("new-running", Running, Some(&new)),
            on_slot("old-stopped", Stopped, Some(&old)),
            on_slot("old-starting", Starting, Some(&old)),
            on_slot("no-slot", Running, None),
            parked_warm,
            parked_cold,
        ];
        let ids: Vec<&str> = behind(&sessions, Some(&new)).iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["old-running", "old-idle", "parked-warm"]);
        assert!(
            behind(&sessions, None).is_empty(),
            "a build with no app directory has no slot to compare"
        );
        // A slot reached through a symlink is the same slot.
        std::os::unix::fs::symlink(&new, root.join("app")).unwrap();
        let ids: Vec<&str> = behind(&sessions, Some(&root.join("app")))
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, ["old-running", "old-idle", "parked-warm"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn restart_takes_only_colonies_that_are_behind() {
        let old = PathBuf::from("/nowhere/app-a");
        let sessions = [
            on_slot("a", SessionStatus::Running, Some(&old)),
            on_slot("b", SessionStatus::Running, Some(&old)),
        ];
        let behind: Vec<&Session> = sessions.iter().collect();
        let running = BTreeSet::from(["b".to_string()]);

        let all = RestartRequest {
            all: true,
            ..Default::default()
        };
        let (targets, skipped) = restart_targets(&behind, &all, &running);
        assert_eq!(targets, ["a"]);
        assert_eq!(skipped[0]["reason"], "already restarting");

        let some = RestartRequest {
            ids: vec!["a".into(), "a".into(), "zzz".into()],
            all: false,
        };
        let (targets, skipped) = restart_targets(&behind, &some, &BTreeSet::new());
        assert_eq!(targets, ["a"], "a repeated id is restarted once");
        assert_eq!(
            skipped,
            vec![json!({"id": "zzz", "reason": "not running on a previous version"})]
        );
    }

    #[test]
    fn a_source_build_is_told_how_to_switch_to_releases() {
        let build = |development: bool| version::Build {
            version: if development { "v0.2.6-3-gabc1234" } else { "v0.2.6" }.into(),
            commit: None,
            dirty: false,
            built_at: chrono::Utc::now(),
            release: Some("v0.2.6".into()),
            development,
        };
        let blocked = switch_to_releases(&build(true), Some("no installer")).unwrap();
        assert_eq!(blocked["command"], INSTALL_COMMAND);
        assert_eq!(blocked["reason"], "no installer");
        let dev = switch_to_releases(&build(true), None).unwrap();
        assert_eq!(dev["command"], "colonizer update --force");
        assert!(dev["reason"].as_str().unwrap().contains("v0.2.6-3-gabc1234"));
        assert_eq!(switch_to_releases(&build(false), None), None, "a release updates in place");
    }

    #[tokio::test]
    async fn the_status_carries_the_new_fields() {
        let dir = std::env::temp_dir().join(format!("colonizer-notices-{}", util::short_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let app = crate::tests::test_app(&dir);
        let mut view = json!({});
        extend(&app, &mut view).await;
        assert_eq!(view["notices"], json!([]), "no release known, nothing pending");
        assert_eq!(view["behind"], json!([]));
        assert_eq!(view["restarts"]["restarting"], json!([]));
        // The test app has no app directory, so it is a build that cannot update in place.
        assert_eq!(view["switch_to_releases"]["command"], INSTALL_COMMAND);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn restarting_needs_a_target_and_skips_colonies_that_are_not_behind() {
        let dir = std::env::temp_dir().join(format!("colonizer-notices-restart-{}", util::short_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let app = crate::tests::test_app(&dir);
        let empty = restart(State(app.clone()), Json(RestartRequest::default())).await;
        assert_eq!(empty.err().map(|e| e.status()), Some(StatusCode::BAD_REQUEST));
        // Nothing is behind in an app with no slot, so an id is skipped rather than restarted.
        let answer = restart(
            State(app.clone()),
            Json(RestartRequest {
                ids: vec!["abc".into()],
                all: false,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(answer["restarting"], json!([]));
        assert_eq!(answer["skipped"][0]["id"], "abc");
        std::fs::remove_dir_all(&dir).ok();
    }
}
