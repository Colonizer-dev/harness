//! Switching a colony's agent module mid-task (issue #737): `POST /api/sessions/{id}/switch-agent`
//! with `{"module": "codex"}` converts the stored conversation from the module the colony is on to
//! another one and reboots the colony on it.
//!
//! The conversion is delegated to [txcript], which maps each harness through its canonical model.
//! The source runner — if it is live — is stopped first so its transcript is settled, the target
//! transcript is written where the target runner resumes from, the colony's `agent`, `agent_session`
//! and `switch_note` are updated, and the colony is resumed: the boot treats `switch_note` as a
//! resume trigger and its first turn is the note telling the new agent it is continuing someone
//! else's session (boot.rs). Only two pairs are supported — claude-code ↔ codex — because those are
//! the modules whose transcript formats txcript maps both ways; anything else is a 400.
//!
//! A restricted colony is refused unless the target module's provider is one the gateway would let
//! it reach (`restricted_target_allowed`), so a switch is never laxer than launching the colony on
//! the target module would have been.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use txcript::harness::claude_code::{ClaudeCode, ClaudeStore};
use txcript::harness::codex::{Codex, CodexStore};
use txcript::{Store, TextCodec, Transcript, convert};

use crate::{
    ApiResult, Shared,
    app::client_error,
    lifecycle,
    modules::{AgentModule, VENDOR_KEYS},
    providers::Provider,
    sensitivity::{self, ProviderMark, Sensitivity, SensitivityOverrides},
    sessions::{Session, SessionStatus},
};

/// The agent pairs a switch may cross, as `(from, to)`. txcript maps the claude-code and codex
/// transcript formats both ways; no other installed module has a reciprocal mapping, so every other
/// pair is refused as unsupported.
pub(crate) const SUPPORTED_PAIRS: &[(&str, &str)] = &[("claude-code", "codex"), ("codex", "claude-code")];

/// `POST /api/sessions/{id}/switch-agent` body.
#[derive(Debug, Deserialize)]
pub struct SwitchRequest {
    /// The agent module id to switch the colony to.
    pub module: String,
}

/// The API routes this module serves.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/sessions/{id}/switch-agent", routing::post(switch_agent))
}

/// This module's feature descriptor (`features.rs`): its route, scoped-token rule, activity rule
/// and kind, read through `features::ALL` by `server`, `api_tokens` and `activity`.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "switch_agent",
    routes,
    token_scope: Some(token_scope),
    activity: ACTIVITY,
    kinds: &["colony.switch_agent"],
    start_tasks: None,
};

/// The activity line a switch records, against the colony it switched.
const ACTIVITY: &[crate::activity::Rule] = &[crate::activity::rule(
    "POST",
    "/api/sessions/{id}/switch-agent",
    "colony.switch_agent",
    crate::activity::Target::Colony,
)];

/// What a scoped token needs to switch a colony's agent: operate on that colony, like answering,
/// stopping or resuming it (`api_tokens`).
fn token_scope<'a>(method: &axum::http::Method, segs: &[&'a str]) -> Option<crate::api_tokens::Need<'a>> {
    match segs {
        ["api", "sessions", id, "switch-agent"] if *method == axum::http::Method::POST && !id.is_empty() => {
            Some(crate::api_tokens::Need::Session {
                id,
                at_least: crate::api_tokens::Scope::Operate,
            })
        }
        _ => None,
    }
}

/// Whether a colony in this state may be switched. Live colonies are stopped and booted again on the
/// target module; stopped, failed and parked ones are only booted. A queued colony has nothing to
/// convert yet, `publishing` has a push in flight, and the terminal states (a pull request opened,
/// merged or closed, or nothing to push) are over for good.
fn switchable(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Starting
            | SessionStatus::Running
            | SessionStatus::WaitingForAnswer
            | SessionStatus::Idle
            | SessionStatus::Stopped
            | SessionStatus::Failed
            | SessionStatus::Parked
    )
}

/// `POST /api/sessions/{id}/switch-agent`: convert the colony's stored session to `module` and boot
/// it there mid-task. Refused for a colony already over, a queued one, one mid-publish, one waiting
/// on a held answer, one the resume that ends the switch would itself refuse (no worktree, cleaned
/// up, superseded unkept), an unsupported pair or a target module with nowhere to resume from, and a
/// restricted colony whose target module reaches only ineligible providers.
pub async fn switch_agent(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<SwitchRequest>,
) -> ApiResult<Session> {
    let s = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let from = s.agent.clone();
    let to = req.module.trim().to_string();
    if !SUPPORTED_PAIRS.iter().any(|(a, b)| *a == from && *b == to) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("unsupported: a colony on \"{from}\" can only be switched to codex or claude-code"),
        ));
    }
    // The target module has to be installed for the boot to resolve it (`boot` looks it up by id).
    let target = app
        .agents
        .iter()
        .find(|a| a.id == to)
        .cloned()
        .ok_or_else(|| client_error(StatusCode::BAD_REQUEST, &format!("agent module \"{to}\" is not installed")))?;
    // The converted transcript has to land where the target runner resumes from, which is the
    // module's declared `session_resume.dir` mounted over the host transcript directory. A module
    // that names none has nowhere to read the converted session, so the switch cannot carry over.
    if target.resume_dir.is_none() {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "agent module \"{to}\" does not declare a session_resume.dir, so a converted transcript has nowhere to land"
            ),
        ));
    }
    if !switchable(s.status) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "this colony can't be switched now: it is over, queued, mid-publish, or has nothing running to convert",
        ));
    }
    // A colony suspended while it waits on the user holds an answer a stop would discard, and the
    // switch stops it. Refuse rather than lose the answer; answer or resume it first.
    if lifecycle::suspended_waiting(&s) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "this colony is waiting on an answer, which a switch would discard; answer or resume it first",
        ));
    }
    // The switch ends by resuming the colony, so the resume's own gates must hold *now*, before
    // anything is stopped: the worktree on disk and not cleaned up, and the colony not superseded
    // unkept. `can_resume`'s status half is met by construction — a live or parked colony is stopped
    // to `stopped` below, the other switchable states are ones it accepts — so only its data half
    // can fail here, checked against the post-stop status. Without this a colony with no worktree
    // would be stopped, converted and only then refused by `resume`, leaving it mutated.
    if !lifecycle::can_resume(SessionStatus::Stopped, s.cleaned_up, s.git_admin_dir.is_some()) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "this colony has no worktree on disk (or was cleaned up), so there is nothing to boot the target module on",
        ));
    }
    if let Some(superseded) = s.superseded.as_ref().filter(|superseded| !superseded.kept) {
        return Err(client_error(
            StatusCode::CONFLICT,
            &crate::supersede::blocked_message(superseded),
        ));
    }
    // The sensitivity guard: a restricted colony may only move to a module whose provider the
    // gateway would let it reach. Checked before anything is stopped, so a refusal changes nothing.
    let overrides = app.org_settings(&s.org).sensitivity;
    let providers = app.providers();
    if !restricted_target_allowed(
        s.sensitivity.as_deref(),
        &target,
        &providers,
        s.allowed_providers.as_deref(),
        overrides.as_ref(),
    ) {
        return Err(client_error(
            StatusCode::FORBIDDEN,
            &format!(
                "this colony's task touches restricted paths and no provider for the \"{to}\" module is eligible; mark one trusted (or vetted) in providers.json, or pin its vendor, before switching"
            ),
        ));
    }

    // A live runner is stopped first so its transcript is settled before it is read. A parked colony
    // whose park kept its microVM is stopped too (issue #213, #737): a resume would otherwise take
    // the warm path and prompt the *old* runner in the machine it never left, delivering neither the
    // converted transcript nor the switch note. Stopping it hands the kept machine in and clears the
    // park record, so the resume below takes the cold boot path. The stop takes the colony's
    // lifecycle lock itself, so this handler holds no lock across it (nor across the resume below) —
    // the same shape as `quota_cards`'s restart.
    let parked_kept = s.parked.as_ref().is_some_and(|park| park.vm_kept);
    if s.status.is_live() || parked_kept {
        let _ = lifecycle::stop(State(app.clone()), Path(id.clone())).await.map_err(|e| {
            client_error(
                StatusCode::CONFLICT,
                &format!("could not stop the running agent: {}", e.message()),
            )
        })?;
    }

    // Re-read: the stop above cleared a suspension, and the agent session id is what names the
    // source transcript.
    let s = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let Some(source_id) = s.agent_session.clone() else {
        return Err(client_error(
            StatusCode::CONFLICT,
            "this colony has no stored agent session to convert; there is nothing to carry over",
        ));
    };
    // The id is the agent's own event stream's session id (events.rs), so it is guest-supplied and
    // untrusted, and it names the source file under the mounted transcript directory. Refuse
    // anything that is not a bare file-name component, so a hostile id cannot traverse out of it.
    if !safe_session_id(&source_id) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "this colony's stored agent session id is not usable as a transcript file name; refusing to convert",
        ));
    }
    let transcripts = app.session_dir(&id).join("transcripts");
    let converted = match convert_transcript(&from, &to, &transcripts, &source_id) {
        Ok(converted) => converted,
        Err(e) => {
            // The runner was stopped above; the conversion decides whether the colony can carry on
            // at all, so a failure here leaves it stopped on its original agent. Say so on the
            // event log, or the switch would leave no trace but a 409.
            app.session_log(
                &id,
                "error",
                format!("switching the agent to {to} failed: {e:#}; the colony is stopped on its original agent ({from})"),
            )
            .await;
            return Err(client_error(
                StatusCode::CONFLICT,
                &format!("could not convert the session: {e:#}"),
            ));
        }
    };

    // The note the resumed runner is first told: it must re-read the worktree, because the
    // conversion keeps the conversation but not everything that shaped it.
    let note = format!(
        "You are continuing a session that another agent ({from}) started; the conversation so far was \
         converted from its transcript and may be missing some details ({loss}). Re-read the worktree \
         state before acting, then carry on with the task.",
        loss = converted.loss
    );
    // What a failed resume below puts back. The source session id is unchanged by the switch (the
    // target gets a fresh one), so the original is just the one read before the update.
    let previous_agent_session = s.agent_session.clone();
    let Some((updated, ())) = app
        .update_session(&id, |x| {
            x.agent = to.clone();
            x.agent_session = Some(converted.id.clone());
            x.switch_note = Some(note.clone());
        })
        .await
    else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such session"));
    };
    app.persist_and_broadcast(&updated).await;
    app.session_log(
        &id,
        "info",
        format!(
            "switched agent: {from} -> {to}; the session was converted to {to} and the colony boots on it ({}); {}",
            converted.path.display(),
            converted.loss
        ),
    )
    .await;

    // Boot on the target module: `resume` claims the colony and its boot reads the new `agent`,
    // `agent_session` and `switch_note` (COLONIZER_RESUME_SESSION plus the note as the first turn).
    // A resume that still loses — a concurrent stop or resume won the lifecycle lock between the
    // update above and here, or the machine could not be claimed — must not leave the colony
    // mutated onto a module it was never booted on, so put the record back and drop the converted
    // file, then answer the error.
    match lifecycle::resume(State(app.clone()), Path(id.clone()), None).await {
        Ok(_) => Ok(Json(app.session(&id).await.unwrap_or(updated))),
        Err(e) => {
            if let Some((s, ())) = app
                .update_session(&id, |x| {
                    x.agent = from.clone();
                    x.agent_session = previous_agent_session.clone();
                    x.switch_note = None;
                })
                .await
            {
                app.persist_and_broadcast(&s).await;
            }
            let _ = std::fs::remove_file(&converted.path);
            app.session_log(
                &id,
                "error",
                format!(
                    "switching the agent to {to} failed: {}; the colony was put back on its original agent ({from})",
                    e.message()
                ),
            )
            .await;
            Err(e)
        }
    }
}

/// The outcome of one transcript conversion: the id the target runner resumes by, where the
/// converted file landed, and a heuristic note on what the conversion could not carry over.
struct Converted {
    id: String,
    path: std::path::PathBuf,
    loss: String,
}

/// Converts a colony's stored session from one agent module to another, writing the target
/// transcript where the target runner resumes from.
///
/// The host `transcripts` directory is mounted over the module's `session_resume.dir` (boot.rs): for
/// claude-code that is `/root/.claude/projects`, so a Claude transcript lives at
/// `<transcripts>/<encode_project_dir(cwd)>/<id>.jsonl` (the guest cwd is `/workspace`, hence
/// `-workspace`); for codex it is `/root/.codex`, so a rollout lives at
/// `<transcripts>/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl` and `codex exec resume <id>` finds it
/// by the id in its filename. The target gets a fresh id, so a switch does not overwrite the
/// transcript the source module left behind.
///
/// txcript reports no loss figure, so the note is honest by construction: it counts the source and
/// target native records and adds the fixed, per-direction caveat, never a precise claim.
fn convert_transcript(from: &str, to: &str, transcripts: &std::path::Path, source_id: &str) -> anyhow::Result<Converted> {
    let target_id = uuid::Uuid::new_v4().to_string();
    match (from, to) {
        ("claude-code", "codex") => {
            let source = transcripts.join("-workspace").join(format!("{source_id}.jsonl"));
            // The stored path is not echoed: `source_id` is guest-supplied, and the message reaches
            // the caller, so only the failure is named, never the resolved host path.
            let text = std::fs::read_to_string(&source)
                .map_err(|e| anyhow::anyhow!("the claude-code transcript for session {source_id} could not be read ({e})"))?;
            let mut claude = ClaudeCode::from_text(&text)?;
            let source_records = claude.body.len();
            // The target new id and cwd ride the canonical meta through the conversion: each codec
            // renders the id from `meta` when it builds its records, so setting it here — before
            // `convert` — is what makes the codex `session_meta` line carry the new id rather than
            // the source's (codex resumes by the id in that line and in the filename).
            claude.meta.id = target_id.clone();
            claude.meta.cwd = Some("/workspace".into());
            let codex: Transcript<Codex> = convert::<ClaudeCode, Codex>(&claude)?;
            let target_records = codex.body.len();
            // Written through the store so the file lands exactly where the runner resumes from.
            let saved = CodexStore::new(transcripts.join("sessions")).save(&codex)?;
            let loss = format!(
                "converted {source_records} claude-code records into {target_records} codex records; txcript cannot report an exact figure, and codex does not model Claude Code's thinking blocks, per-message token usage or subagent side-chains"
            );
            Ok(Converted {
                id: saved.id,
                path: saved.reference,
                loss,
            })
        }
        ("codex", "claude-code") => {
            let store = CodexStore::new(transcripts.join("sessions"));
            let discovered = store
                .discover()?
                .into_iter()
                .find(|d| d.meta.id == source_id)
                .ok_or_else(|| anyhow::anyhow!("the codex rollout for session {source_id} was not found"))?;
            let mut codex = store.load(&discovered.reference)?;
            let source_records = codex.body.len();
            // Set on the source, before `convert`: the claude-code codec stamps each entry's
            // `sessionId` from `meta.id`, so the new id has to be in place when the records are
            // built. The guest cwd is /workspace, which puts the file under `-workspace`, where the
            // claude-code runner resumes from (`session_resume.dir` is /root/.claude/projects).
            codex.meta.id = target_id.clone();
            codex.meta.cwd = Some("/workspace".into());
            let claude: Transcript<ClaudeCode> = convert::<Codex, ClaudeCode>(&codex)?;
            let target_records = claude.body.len();
            let saved = ClaudeStore::new(transcripts).save(&claude)?;
            let loss = format!(
                "converted {source_records} codex records into {target_records} claude-code records; txcript cannot report an exact figure, and Claude Code's transcript format does not model Codex reasoning summaries or its token-usage records"
            );
            Ok(Converted {
                id: saved.id,
                path: saved.reference,
                loss,
            })
        }
        _ => anyhow::bail!("unsupported pair: \"{from}\" -> \"{to}\""),
    }
}

/// The hosts a module's agent reaches for its model: its declared egress API hosts plus the hosts of
/// its declared vendor secrets. These are the hosts the gateway sees on the wire, so they name the
/// providers a colony on this module could reach.
fn module_reach_hosts(module: &AgentModule) -> Vec<String> {
    let mut hosts: Vec<String> = module.egress.as_ref().map(|e| e.api.clone()).unwrap_or_default();
    for secret in &module.vendor_secrets {
        hosts.extend(secret.hosts.iter().cloned());
    }
    hosts.sort();
    hosts.dedup();
    hosts
}

/// Whether a configured provider serves one of the module's reach hosts: its `base_url` host matches
/// head-on, or the module declares a [`VENDOR_KEYS`] host that names this provider's id.
fn provider_covers(hosts: &[String], provider: &Provider) -> bool {
    if let Some((_, host, _, _)) = crate::providers::split_url(&provider.base_url)
        && hosts.iter().any(|h| h.eq_ignore_ascii_case(&host))
    {
        return true;
    }
    VENDOR_KEYS
        .iter()
        .any(|(known, id, _)| *id == provider.id && hosts.iter().any(|h| h.eq_ignore_ascii_case(known)))
}

/// Whether an id reported by the guest may be used as a bare file-name component under the mounted
/// transcript directory. The id is the agent's own event stream's session id (events.rs), so it is
/// untrusted, and it names the source transcript file. Only the shape a session id has is allowed —
/// ASCII alphanumerics, `-` and `_` — so a separator, `.`/`..`, a NUL or any other control character
/// is refused and a hostile id cannot traverse out of the directory it names.
pub(crate) fn safe_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 200 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Whether a colony of this sensitivity may be switched onto `module` at all.
///
/// The gateway refuses a restricted colony's requests to any provider the task's class does not
/// clear (`gateway.rs` → [`sensitivity::eligible`]), and it also refuses any provider the session's
/// model settings did not route to (`Session::allowed_providers`) — a colony spends only on the
/// providers it was given. The switch must be no laxer than that, so it is refused up front rather
/// than failing mid-task. The candidates are the providers the target module would reach
/// (`provider_covers`), narrowed to the session's routed set when it pins one; the switch is allowed
/// only when there is at least one candidate and *every* one of them is eligible — an untrusted
/// provider sharing a reachable host with a trusted one can still be the one picked, so it refuses.
/// Every class below restricted clears the `any` mark, so it is always allowed. A restricted colony
/// whose target module resolves to no candidate at all is refused: "runs somewhere, unrecorded" is
/// the one case the gateway cannot vouch for.
pub(crate) fn restricted_target_allowed(
    sensitivity: Option<&str>,
    module: &AgentModule,
    providers: &[Provider],
    allowed: Option<&[String]>,
    overrides: Option<&SensitivityOverrides>,
) -> bool {
    let Some(Sensitivity::Restricted) = sensitivity.and_then(Sensitivity::parse) else {
        return true;
    };
    let hosts = module_reach_hosts(module);
    if hosts.is_empty() {
        return false;
    }
    let candidates: Vec<&Provider> = providers
        .iter()
        .filter(|provider| provider_covers(&hosts, provider))
        .filter(|provider| allowed.is_none_or(|allowed| allowed.contains(&provider.id)))
        .collect();
    if candidates.is_empty() {
        return false;
    }
    candidates.iter().all(|provider| {
        let mark = ProviderMark::of(provider.trusted, provider.vetted);
        sensitivity::eligible(Sensitivity::Restricted, mark, provider.vendor.as_deref(), overrides)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::{DeclaredSecret, Egress};
    use crate::util::short_id;
    use serde_json::json;

    // ----- the transcript conversions -----

    /// A small claude-code transcript: one user turn, one assistant turn, as the runner writes it
    /// (one JSON object per line). The guest cwd is /workspace, so the store reads it from
    /// `<transcripts>/-workspace/<id>.jsonl`.
    const CLAUDE_FIXTURE: &str = concat!(
        r#"{"type":"user","uuid":"u1","sessionId":"11111111-1111-4111-8111-111111111111","timestamp":"2026-01-02T03:04:05.000Z","cwd":"/workspace","message":{"role":"user","content":"add a regression test for the parser"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"11111111-1111-4111-8111-111111111111","timestamp":"2026-01-02T03:04:06.000Z","cwd":"/workspace","message":{"role":"assistant","model":"claude-sonnet-5","content":[{"type":"text","text":"I will read the parser first."}]}}"#,
        "\n",
    );

    /// A small codex rollout: a session_meta line then two event lines, as the runner keeps it under
    /// `<transcripts>/sessions/YYYY/MM/DD/rollout-*-<id>.jsonl`.
    const CODEX_FIXTURE: &str = concat!(
        r#"{"timestamp":"2026-01-02T03:04:05.000Z","type":"session_meta","payload":{"id":"22222222-2222-4222-8222-222222222222","cwd":"/workspace","originator":"codex_cli_rs","cli_version":"0.156.1"}}"#,
        "\n",
        r#"{"timestamp":"2026-01-02T03:04:06.000Z","type":"event_msg","payload":{"type":"user_message","message":"add a regression test for the parser"}}"#,
        "\n",
        r#"{"timestamp":"2026-01-02T03:04:07.000Z","type":"event_msg","payload":{"type":"agent_message","message":"I will read the parser first."}}"#,
        "\n",
    );

    fn temp_transcripts() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-switch-{}", short_id()));
        let transcripts = dir.join("transcripts");
        std::fs::create_dir_all(&transcripts).unwrap();
        transcripts
    }

    fn claude_source_path(transcripts: &std::path::Path, id: &str) -> std::path::PathBuf {
        transcripts.join("-workspace").join(format!("{id}.jsonl"))
    }

    #[test]
    fn claude_code_converts_to_codex_where_the_codex_runner_resumes() {
        let transcripts = temp_transcripts();
        let source_id = "11111111-1111-4111-8111-111111111111";
        let source = claude_source_path(&transcripts, source_id);
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, CLAUDE_FIXTURE).unwrap();

        let out = convert_transcript("claude-code", "codex", &transcripts, source_id).unwrap();
        assert_ne!(out.id, source_id, "the target gets a fresh id, not the source's");

        // The file lands exactly where `codex exec resume <id>` looks: under the mounted CODEX_HOME
        // at sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl, with the id in the filename.
        let expected_dir = transcripts.join("sessions/2026/01/02");
        assert_eq!(out.path.parent().unwrap(), expected_dir.as_path());
        let name = out.path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("rollout-"), "{name}");
        assert!(name.ends_with(&format!("-{}.jsonl", out.id)), "{name}");
        assert!(out.path.is_file());

        // The returned id is the one COLONIZER_RESUME_SESSION will carry: the target codec parses
        // the file back and reads the same id.
        let text = std::fs::read_to_string(&out.path).unwrap();
        let codex = Codex::from_text(&text).unwrap();
        assert_eq!(codex.meta.id, out.id);
        assert_eq!(codex.meta.cwd.as_deref(), Some("/workspace"));
        assert!(out.loss.contains("claude-code records"), "{}", out.loss);
        let _ = std::fs::remove_dir_all(transcripts.parent().unwrap());
    }

    #[test]
    fn codex_converts_to_claude_code_where_the_claude_runner_resumes() {
        let transcripts = temp_transcripts();
        let source_id = "22222222-2222-4222-8222-222222222222";
        // Codex rollouts live under sessions/YYYY/MM/DD, keyed by the id in the filename.
        let source = transcripts
            .join("sessions/2026/01/02")
            .join(format!("rollout-2026-01-02T03-04-05-{source_id}.jsonl"));
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, CODEX_FIXTURE).unwrap();

        let out = convert_transcript("codex", "claude-code", &transcripts, source_id).unwrap();
        assert_ne!(out.id, source_id);

        // The file lands exactly where the claude-code runner resumes from: under the mounted
        // projects root at -workspace/<id>.jsonl (guest cwd /workspace).
        assert_eq!(out.path, claude_source_path(&transcripts, &out.id));
        assert!(out.path.is_file());

        // Loaded through the store, as the runner does: the id comes back as itself (the file is
        // under `-workspace`, the path assertion above, which is where the runner resumes from).
        let claude = ClaudeStore::new(&transcripts).load(&out.path).unwrap();
        assert_eq!(claude.meta.id, out.id);
        assert!(out.loss.contains("codex records"), "{}", out.loss);
        let _ = std::fs::remove_dir_all(transcripts.parent().unwrap());
    }

    // ----- the restricted-sensitivity guard -----

    fn provider(value: serde_json::Value) -> Provider {
        serde_json::from_value(value).unwrap()
    }

    /// The codex module as installed: it declares the OpenAI API host.
    fn codex_module() -> AgentModule {
        AgentModule::test("codex")
            .egress(Some(Egress {
                api: vec!["api.openai.com".into()],
                auth: Vec::new(),
                telemetry: Vec::new(),
                extra: Vec::new(),
            }))
            .vendor_secrets(vec![DeclaredSecret {
                env: vec!["CODEX_API_KEY".into()],
                hosts: vec!["api.openai.com".into()],
            }])
    }

    #[test]
    fn restricted_is_allowed_when_a_target_provider_is_trusted() {
        let providers = vec![provider(
            json!({"id":"openai","name":"OpenAI","base_url":"https://api.openai.com/v1","auth":"bearer","trusted":true}),
        )];
        assert!(restricted_target_allowed(
            Some("restricted"),
            &codex_module(),
            &providers,
            None,
            None
        ));
    }

    #[test]
    fn restricted_is_refused_when_no_target_provider_is_eligible() {
        // Configured, but not marked trusted or vetted: the gateway would refuse it mid-task, so
        // the switch is refused up front.
        let providers = vec![provider(
            json!({"id":"openai","name":"OpenAI","base_url":"https://api.openai.com/v1","auth":"bearer"}),
        )];
        assert!(!restricted_target_allowed(
            Some("restricted"),
            &codex_module(),
            &providers,
            None,
            None
        ));
        // And refused when no provider serves the module's host at all.
        let elsewhere = vec![provider(
            json!({"id":"other","name":"Other","base_url":"https://api.example.com/v1","auth":"bearer","trusted":true}),
        )];
        assert!(!restricted_target_allowed(
            Some("restricted"),
            &codex_module(),
            &elsewhere,
            None,
            None
        ));
    }

    #[test]
    fn an_untrusted_provider_sharing_a_reachable_host_refuses_the_switch() {
        // Both providers reach the module's host; only one is trusted. Which one the gateway picks
        // is not knowable up front, so the switch is conservative and refuses.
        let providers = vec![
            provider(
                json!({"id":"openai","name":"OpenAI","base_url":"https://api.openai.com/v1","auth":"bearer","trusted":true}),
            ),
            provider(json!({"id":"proxy","name":"Proxy","base_url":"https://api.openai.com/v1","auth":"bearer"})),
        ];
        assert!(!restricted_target_allowed(
            Some("restricted"),
            &codex_module(),
            &providers,
            None,
            None
        ));
    }

    #[test]
    fn restricted_narrows_to_the_sessions_routed_providers() {
        let providers = vec![
            provider(
                json!({"id":"openai","name":"OpenAI","base_url":"https://api.openai.com/v1","auth":"bearer","trusted":true}),
            ),
            provider(json!({"id":"second","name":"Second","base_url":"https://api.openai.com/v1","auth":"bearer"})),
        ];
        // Routed only to the trusted one: the untrusted sibling cannot be picked, so it is allowed.
        let routed = vec!["openai".to_string()];
        assert!(restricted_target_allowed(
            Some("restricted"),
            &codex_module(),
            &providers,
            Some(&routed),
            None
        ));
        // Routed narrowly to the untrusted one: no eligible candidate, refused.
        let untrusted = vec!["second".to_string()];
        assert!(!restricted_target_allowed(
            Some("restricted"),
            &codex_module(),
            &providers,
            Some(&untrusted),
            None
        ));
        // Routed to a provider that does not reach the module's host: no candidate, refused.
        let elsewhere = vec!["nowhere".to_string()];
        assert!(!restricted_target_allowed(
            Some("restricted"),
            &codex_module(),
            &providers,
            Some(&elsewhere),
            None
        ));
    }

    #[test]
    fn non_restricted_is_always_allowed_and_module_without_hosts_is_refused_when_restricted() {
        let unmarked = vec![provider(
            json!({"id":"openai","name":"OpenAI","base_url":"https://api.openai.com/v1","auth":"bearer"}),
        )];
        for class in ["open", "standard", "vetted", "custom", ""] {
            assert!(
                restricted_target_allowed(Some(class), &codex_module(), &unmarked, None, None),
                "{class} should not be gated"
            );
        }
        assert!(restricted_target_allowed(None, &codex_module(), &unmarked, None, None));
        // A module that names no host cannot be resolved to a provider, so restricted refuses.
        let bare = AgentModule::test("mystery");
        assert!(!restricted_target_allowed(Some("restricted"), &bare, &unmarked, None, None));
    }

    #[test]
    fn a_session_id_must_be_a_bare_file_name() {
        assert!(safe_session_id("11111111-1111-4111-8111-111111111111"));
        assert!(safe_session_id("rollout_abc-123"));
        assert!(!safe_session_id(""));
        assert!(!safe_session_id("../escape"));
        assert!(!safe_session_id("a/b"));
        assert!(!safe_session_id("a\\b"));
        assert!(!safe_session_id(".."));
        assert!(!safe_session_id("a.jsonl"));
        assert!(!safe_session_id("a\0b"));
        assert!(!safe_session_id("a\nb"));
    }

    // ----- the endpoint -----

    /// The install a switch needs: both agent modules, each declaring where its runner resumes from.
    /// The codex module also declares its egress host, so the sensitivity guard can resolve it to a
    /// provider.
    fn switchable_app(root: &std::path::Path) -> Shared {
        crate::tests::test_app_with_agents(
            root,
            vec![
                AgentModule::test("claude-code").resume_dir(Some("/root/.claude/projects".into())),
                codex_module().resume_dir(Some("/root/.codex".into())),
            ],
            |_| {},
        )
    }

    /// Writes `providers.json`, which [`App::providers`] reads on every call.
    fn write_providers(app: &Shared, rows: serde_json::Value) {
        let path = app.cfg.config_dir.join("providers.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rows.to_string()).unwrap();
    }

    /// A small claude-code transcript on disk for `source_id`, under `colony`'s transcript
    /// directory, and the path it was written to.
    fn write_claude_source(app: &Shared, colony: &str, source_id: &str) -> std::path::PathBuf {
        let source = app
            .session_dir(colony)
            .join("transcripts/-workspace")
            .join(format!("{source_id}.jsonl"));
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, CLAUDE_FIXTURE).unwrap();
        source
    }

    fn switch_call(app: &Shared, id: &str, module: &str) -> impl std::future::Future<Output = ApiResult<Session>> {
        let app = app.clone();
        let id = id.to_string();
        let module = module.to_string();
        async move { switch_agent(State(app), Path(id), Json(SwitchRequest { module })).await }
    }

    /// A stopped claude-code colony with a transcript on disk switches to codex: the record ends on
    /// `agent == "codex"` with the fresh session id, the note set, the converted rollout landable by
    /// the codex store, and a harness_log line naming the switch.
    #[tokio::test]
    async fn a_stopped_colony_switches_to_codex_and_reboots_on_it() {
        use crate::sessions::tests::stopped_colony_with_worktree;

        let root = std::env::temp_dir().join(format!("colonizer-switch-endpoint-{}", short_id()));
        let app = switchable_app(&root);
        let source_id = "11111111-1111-4111-8111-111111111111";
        let mut s = stopped_colony_with_worktree("acme", "c1".into());
        s.agent = "claude-code".into();
        s.agent_session = Some(source_id.into());
        app.sessions.write().await.push(s);
        write_claude_source(&app, "c1", source_id);

        let switched = switch_call(&app, "c1", "codex").await.unwrap().0;
        assert_eq!(switched.agent, "codex");
        let new_id = switched.agent_session.clone().expect("a new agent session id");
        assert_ne!(new_id, source_id);
        let note = switched.switch_note.clone().expect("the switch note is set for the boot");
        assert!(note.contains("another agent"), "{note}");

        // The converted rollout is where the codex runner resumes from, discoverable by its new id.
        let transcripts = app.session_dir("c1").join("transcripts");
        let store = CodexStore::new(transcripts.join("sessions"));
        assert!(
            store.discover().unwrap().iter().any(|d| d.meta.id == new_id),
            "the codex store finds the converted rollout by the new session id"
        );
        // The switch is on the colony's event log the cockpit renders (the harness log lines).
        let log = std::fs::read_to_string(app.session_dir("c1").join("harness.jsonl")).unwrap();
        assert!(log.contains("switched agent"), "{log}");
        assert!(log.contains("claude-code -> codex"), "{log}");

        let _ = std::fs::remove_dir_all(root);
    }

    /// The other direction: a stopped codex colony switches to claude-code, and the converted file
    /// lands under `-workspace/<new-id>.jsonl`, where the claude-code runner resumes from.
    #[tokio::test]
    async fn a_stopped_codex_colony_switches_to_claude_code() {
        use crate::sessions::tests::stopped_colony_with_worktree;

        let root = std::env::temp_dir().join(format!("colonizer-switch-endpoint-{}", short_id()));
        let app = switchable_app(&root);
        let source_id = "22222222-2222-4222-8222-222222222222";
        let mut s = stopped_colony_with_worktree("acme", "c1".into());
        s.agent = "codex".into();
        s.agent_session = Some(source_id.into());
        app.sessions.write().await.push(s);
        let source = app
            .session_dir("c1")
            .join("transcripts/sessions/2026/01/02")
            .join(format!("rollout-2026-01-02T03-04-05-{source_id}.jsonl"));
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, CODEX_FIXTURE).unwrap();

        let switched = switch_call(&app, "c1", "claude-code").await.unwrap().0;
        assert_eq!(switched.agent, "claude-code");
        let new_id = switched.agent_session.clone().expect("a new agent session id");
        assert_ne!(new_id, source_id);
        assert!(switched.switch_note.is_some());
        let target = app
            .session_dir("c1")
            .join("transcripts/-workspace")
            .join(format!("{new_id}.jsonl"));
        assert!(
            target.is_file(),
            "the converted session lands where the claude runner resumes"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    /// A restricted colony whose only reachable provider is not eligible is refused (403) before
    /// anything changes: the record stays on its original agent and no converted file is written.
    #[tokio::test]
    async fn a_restricted_colony_is_refused_when_the_target_provider_is_not_eligible() {
        use crate::sessions::tests::stopped_colony_with_worktree;

        let root = std::env::temp_dir().join(format!("colonizer-switch-guard-{}", short_id()));
        let app = switchable_app(&root);
        // Configured but neither trusted nor vetted: the gateway would refuse it mid-task.
        write_providers(
            &app,
            json!([{"id":"openai","name":"OpenAI","base_url":"https://api.openai.com/v1","auth":"bearer"}]),
        );
        let source_id = "11111111-1111-4111-8111-111111111111";
        let mut s = stopped_colony_with_worktree("acme", "c1".into());
        s.agent = "claude-code".into();
        s.agent_session = Some(source_id.into());
        s.sensitivity = Some("restricted".into());
        s.allowed_providers = Some(vec!["openai".into()]);
        app.sessions.write().await.push(s);
        write_claude_source(&app, "c1", source_id);

        let err = switch_call(&app, "c1", "codex").await.unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN, "{}", err.message());
        let s = app.session("c1").await.unwrap();
        assert_eq!(s.agent, "claude-code", "the record is untouched by the refusal");
        assert_eq!(s.agent_session.as_deref(), Some(source_id));
        assert!(s.switch_note.is_none());

        let _ = std::fs::remove_dir_all(root);
    }

    /// A colony whose worktree is gone cannot be resumed, so the switch is refused up front (409)
    /// and the record is left exactly as it was — not stopped, not converted, not mutated.
    #[tokio::test]
    async fn a_colony_with_no_worktree_is_refused_up_front_and_left_unchanged() {
        use crate::sessions::tests::stopped_colony_with_worktree;

        let root = std::env::temp_dir().join(format!("colonizer-switch-noworktree-{}", short_id()));
        let app = switchable_app(&root);
        let source_id = "11111111-1111-4111-8111-111111111111";
        let mut s = stopped_colony_with_worktree("acme", "c1".into());
        s.agent = "claude-code".into();
        s.agent_session = Some(source_id.into());
        s.git_admin_dir = None; // the worktree is gone: `resume` would refuse it
        app.sessions.write().await.push(s);
        write_claude_source(&app, "c1", source_id);

        let err = switch_call(&app, "c1", "codex").await.unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT, "{}", err.message());
        let s = app.session("c1").await.unwrap();
        assert_eq!(s.agent, "claude-code");
        assert_eq!(s.agent_session.as_deref(), Some(source_id));
        assert!(s.switch_note.is_none());
        // Nothing was written: the codex store finds no rollout for the colony.
        let store = CodexStore::new(app.session_dir("c1").join("transcripts/sessions"));
        assert!(store.discover().map(|d| d.is_empty()).unwrap_or(true));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_vendor_key_host_maps_a_provider_by_id_when_its_base_url_differs() {
        // The provider's base_url is a proxy; the module's declared host still names it by id.
        let providers = vec![provider(
            json!({"id":"openai","name":"OpenAI","base_url":"https://proxy.internal/openai","auth":"bearer","trusted":true}),
        )];
        assert!(restricted_target_allowed(
            Some("restricted"),
            &codex_module(),
            &providers,
            None,
            None
        ));
    }
}
