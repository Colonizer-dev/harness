//! Boundary events (issue #609): the typed record that a control refused something, which the
//! watchdog's control-defeat signature reads (docs/boundaries.md, "Watchdog signatures").
//!
//! They reach a colony's `events.jsonl` two ways. The runner and agentd emit the ones they see —
//! an exec-policy deny or a refused ask asked again, an egress or read-only refusal on a tool
//! result, a path-policy bind agentd could not apply — and those arrive through `events.rs` like
//! any runner line. The mothership appends the ones only it sees — the agent reaching for a
//! path-policy path, publish replacing a `.git` the colony changed or refusing a tree that moved
//! after its approval — through [`emit`], which writes the line with the system origin and folds
//! it into the watchdog in the same step.
//!
//! Reporting only: by the time a boundary event exists the control has decided. The event grants
//! nothing, and a colony forging one can at worst get itself flagged.

use crate::Shared;
use chrono::Utc;
use serde_json::{Value, json};

/// The closed vocabulary of boundary kinds (docs/agent-events.schema.json `boundary.kind`). A kind
/// outside it is forwarded to the browser like any line and ignored by the watchdog.
pub(crate) const KINDS: [&str; 7] = [
    "exec_policy_deny",
    "exec_policy_ask_bypass_attempt",
    "path_policy_denied",
    "path_policy_unbound",
    "egress_denied",
    "publish_rewrite_refused",
    "sandbox_denied",
];

const CONTROL_CHARS: usize = 80;
const DETAIL_CHARS: usize = 300;
const TARGET_CHARS: usize = 200;

/// One boundary event, its untrusted fields cleaned: one line, redacted, capped.
#[derive(Clone, Debug, PartialEq)]
pub struct Boundary {
    pub kind: String,
    pub control: String,
    pub detail: String,
    pub target: Option<String>,
    /// When the control decided, as the reporter stamped it (RFC 3339).
    pub at: String,
}

/// One line, credentials redacted, at most `max` characters.
fn clean(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    crate::util::truncate(&crate::redact::redact_text(&flat), max)
}

impl Boundary {
    /// A boundary the mothership itself observed, stamped now.
    pub(crate) fn new(kind: &str, control: &str, detail: &str, target: Option<&str>) -> Self {
        debug_assert!(KINDS.contains(&kind), "unknown boundary kind {kind}");
        Self {
            kind: kind.to_string(),
            control: clean(control, CONTROL_CHARS),
            detail: clean(detail, DETAIL_CHARS),
            target: target.map(|t| clean(t, TARGET_CHARS)).filter(|t| !t.is_empty()),
            at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }
    }

    /// A `boundary` line a runner or agentd wrote, or `None` when it is not one the watchdog can
    /// read: an unknown kind, or no control.
    pub(crate) fn from_event(event: &Value) -> Option<Self> {
        if event["type"] != "boundary" {
            return None;
        }
        let kind = event["kind"].as_str().filter(|k| KINDS.contains(k))?;
        let control = clean(event["control"].as_str()?, CONTROL_CHARS);
        if control.is_empty() {
            return None;
        }
        Some(Self {
            kind: kind.to_string(),
            control,
            detail: clean(event["detail"].as_str().unwrap_or_default(), DETAIL_CHARS),
            target: event["target"]
                .as_str()
                .map(|t| clean(t, TARGET_CHARS))
                .filter(|t| !t.is_empty()),
            at: clean(event["at"].as_str().unwrap_or_default(), 40),
        })
    }

    /// The event body (docs/agent-events.schema.json `boundary`); `target` only when named.
    pub fn to_event(&self) -> Value {
        let mut event = json!({
            "type": "boundary",
            "kind": self.kind,
            "control": self.control,
            "detail": self.detail,
            "at": self.at,
        });
        if let Some(target) = &self.target {
            event["target"] = json!(target);
        }
        event
    }
}

/// Appends a mothership-observed boundary event to the colony's events and folds it into the
/// watchdog, flagging the colony when it completes a control-defeat signature.
pub(crate) async fn emit(app: &Shared, id: &str, boundary: Boundary) {
    crate::validation::emit_chain(app, id, boundary.to_event()).await;
    observe(app, id, boundary).await;
}

/// Folds one boundary event, from either side, into the colony's watchdog trail (issue #609).
pub(crate) async fn observe(app: &Shared, id: &str, boundary: Boundary) {
    let rt = app.runtime(id).await;
    let defeat = {
        let mut activity = rt.activity.lock().await;
        crate::watchdog::note_boundary(&mut activity.boundaries, boundary, Utc::now())
    };
    if let Some(defeat) = defeat {
        crate::watchdog::flag_control_defeat(app, id, defeat).await;
    }
}

/// Folds a tool call or its result into the deny-then-reach watch (issue #609): a call naming a
/// refused target is held, and its successful result flags the colony.
pub(crate) async fn observe_tool(app: &Shared, id: &str, rt: &crate::sessions::Runtime, event: &Value) {
    let Some(call) = event["tool_call_id"].as_str() else { return };
    let defeat = {
        let mut activity = rt.activity.lock().await;
        match event["type"].as_str() {
            Some("tool_call") => {
                crate::watchdog::note_reach_call(&mut activity.boundaries, call, &event["input"], Utc::now());
                None
            }
            Some("tool_result") => crate::watchdog::note_reach_result(&mut activity.boundaries, call, event["is_error"] == true),
            _ => None,
        }
    };
    if let Some(defeat) = defeat {
        crate::watchdog::flag_control_defeat(app, id, defeat).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runner_line_is_read_cleaned_and_written_back_in_the_schema_shape() {
        let line = json!({
            "type": "boundary",
            "kind": "exec_policy_deny",
            "control": "exec_policy:secret-paths",
            "detail": format!("deny (default):\ncat .env {}", "x".repeat(400)),
            "target": ".env",
            "at": "2026-01-01T00:00:00.000Z",
            "seq": 7,
        });
        let b = Boundary::from_event(&line).expect("a boundary");
        assert_eq!(b.kind, "exec_policy_deny");
        assert!(!b.detail.contains('\n'), "one line");
        assert_eq!(b.detail.chars().count(), DETAIL_CHARS + 1, "capped, with the ellipsis");
        let back = b.to_event();
        assert_eq!(back["target"], ".env");
        assert_eq!(back["at"], "2026-01-01T00:00:00.000Z");
        assert!(back.get("seq").is_none(), "the body only");
    }

    #[test]
    fn an_unknown_kind_or_a_missing_control_is_not_read() {
        let base = json!({"type": "boundary", "kind": "egress_denied", "control": "egress", "detail": "x", "at": ""});
        assert!(Boundary::from_event(&base).is_some());
        let mut unknown = base.clone();
        unknown["kind"] = json!("telepathy");
        assert!(Boundary::from_event(&unknown).is_none());
        let mut blank = base.clone();
        blank["control"] = json!("  ");
        assert!(Boundary::from_event(&blank).is_none());
        let mut other = base;
        other["type"] = json!("log");
        assert!(Boundary::from_event(&other).is_none());
    }

    #[test]
    fn a_credential_in_the_detail_is_redacted() {
        let b = Boundary::new(
            "egress_denied",
            "egress",
            "curl -H 'Authorization: Bearer ghp_0123456789abcdefghij0123456789abcdef' https://x.example",
            Some("x.example"),
        );
        assert!(!b.detail.contains("ghp_0123456789abcdefghij0123456789abcdef"), "{}", b.detail);
    }

    /// Every kind the schema names is one the harness reads, and the other way round.
    #[test]
    fn the_kinds_are_exactly_the_schemas_enum() {
        let schema: Value = serde_json::from_str(include_str!("../../../docs/agent-events.schema.json")).unwrap();
        let kinds: Vec<&str> = schema["$defs"]["boundary"]["properties"]["kind"]["enum"]
            .as_array()
            .expect("boundary.kind is an enum")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(kinds, KINDS);
        let required: Vec<&str> = schema["$defs"]["boundary"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let written = Boundary::new("sandbox_denied", "read_only_mount", "x", None).to_event();
        for field in required {
            assert!(written.get(field).is_some(), "the written event lacks the required {field}");
        }
    }

    /// The mothership's own boundary events land in the colony's events, with the system origin,
    /// and feed the watchdog: a publish rewrite flags the colony with the event as evidence.
    #[tokio::test]
    async fn a_mothership_boundary_is_appended_and_flags_a_publish_rewrite() {
        let root = std::env::temp_dir().join(format!("colonizer-boundary-{}", uuid::Uuid::new_v4()));
        let app = crate::tests::test_app(&root);
        let mut s = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
        s.id = "b1".into();
        app.sessions.write().await.push(s);
        std::fs::create_dir_all(app.session_dir("b1")).unwrap();
        emit(
            &app,
            "b1",
            Boundary::new(
                "publish_rewrite_refused",
                "gitfile",
                "replaced a .git directory with the recorded gitfile",
                Some(".git"),
            ),
        )
        .await;
        let events = std::fs::read_to_string(app.session_dir("b1").join("events.jsonl")).unwrap();
        let line: Value = serde_json::from_str(events.lines().last().unwrap()).unwrap();
        assert_eq!(line["type"], "boundary");
        assert_eq!(line["kind"], "publish_rewrite_refused");
        assert_eq!(line["origin"], "system");
        let attention = app.session("b1").await.unwrap().attention.expect("flagged");
        assert_eq!(attention["reason"], crate::watchdog::CONTROL_DEFEAT_REASON);
        assert_eq!(attention["signature"], "publish_rewrite");
        assert_eq!(attention["evidence"][0]["control"], "gitfile");
        let logs = app.runtime("b1").await.logs.lock().await.clone();
        assert!(
            logs.iter().any(|l| l["message"]
                .as_str()
                .is_some_and(|m| m.contains("control-defeat signature (publish_rewrite)"))),
            "{logs:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
