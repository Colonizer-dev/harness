//! The agent event contract as a Rust type. `docs/protocol.md` §2 keeps the prose and
//! `docs/agent-events.schema.json` the machine-readable schema for all fourteen event types; this
//! enum is the slice of that contract the harness itself acts on (#73 item 4), and the committed
//! fixture `modules/agents/claude-code/test/fixtures/events.jsonl` proves the runner's real output
//! deserialises into it.
//!
//! Events the harness only forwards to the browser (`assistant_text_delta`, `assistant_text`,
//! `thinking`, `tool_call`, `tool_result`, `log`, `model_changed`) are deliberately not variants:
//! together with any type a newer runner adds they land on [`AgentEvent::Other`], so a new event
//! type can never make a line fail to deserialise. That matters because the browser receives every
//! line regardless — pass-through happens before this dispatch (`events.rs`).

use serde::Deserialize;
use serde_json::{Value, json};

/// The `state` of a `status` event (docs/protocol.md §2). A state a newer runner knows still
/// deserialises, as [`AgentState::Unknown`], so a status change can never be a contract error —
/// it is just no news for this build.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentState {
    Idle,
    Working,
    WaitingForAnswer,
    Error,
    Exited,
    #[serde(other)]
    Unknown,
}

impl AgentState {
    /// The wire spelling, for messages such as `agent exited: exit code 1`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::WaitingForAnswer => "waiting_for_answer",
            Self::Error => "error",
            Self::Exited => "exited",
            Self::Unknown => "unknown",
        }
    }
}

/// The risk class a runner stamps on a `question` (§2), ordered lowest to highest: the autonomy
/// judge answers a question only at or below its ceiling. A question with no field — an older
/// runner's — reads as [`QuestionRisk::WorkspaceWrite`], the default ceiling, and a value outside
/// the vocabulary — a future runner's — reads as [`QuestionRisk::Unknown`], which is ordered above
/// every known class and so is never answered automatically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QuestionRisk {
    ReadOnly,
    WorkspaceWrite,
    PublishAffecting,
    CredentialAdjacent,
    #[serde(other)]
    Unknown,
}

impl QuestionRisk {
    /// The wire spelling, for log lines.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::PublishAffecting => "publish_affecting",
            Self::CredentialAdjacent => "credential_adjacent",
            Self::Unknown => "unknown",
        }
    }

    /// The class a question on the wire is treated as: its `risk` value. Absent or null — an older
    /// runner's question — counts as a workspace write; anything else outside the vocabulary, a
    /// string this build does not know or a value that is not a string at all, counts as
    /// [`QuestionRisk::Unknown`], above every ceiling. The restart replay (`sessions.rs`) reads
    /// the raw stored event, so it folds through here too: never more permissively than the live
    /// parse (`events.rs`), which drops a body it cannot parse outright.
    pub(crate) fn from_wire(risk: Option<&Value>) -> Self {
        match risk {
            None | Some(Value::Null) => Self::WorkspaceWrite,
            Some(risk) => serde_json::from_value(risk.clone()).unwrap_or(Self::Unknown),
        }
    }
}

/// A runner event the harness acts on, tagged on its `type` field exactly as the runner writes it.
/// Fields the harness only forwards — a question's `questions`, the subagent's `agent` ref, a
/// finding's prose — are still modelled so the type is the whole contract for these events, not
/// just the fields this dispatch happens to read (the browser is their consumer, §4/§5).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AgentEvent {
    /// The agent's own state machine; agentd emits the same shape when the runner exits (§3).
    Status {
        state: AgentState,
        /// Optional human detail: the pre-flight scan's block reason, an exit code.
        #[serde(default)]
        detail: Option<String>,
    },
    /// The echo of an accepted message; the watchdog recognises its own nudges by their `watchdog-` id (§6.3).
    UserMessage { id: String, text: String },
    /// A question that needs the user's answer; the innards of `questions` are the choice card's
    /// business (§5) and the schema pins them, the harness only opens the question. The risk class
    /// travels with it: autonomous mode answers only at or below its ceiling (§6.2b).
    Question {
        question_id: String,
        #[serde(default)]
        questions: Vec<Value>,
        #[serde(default)]
        message_id: Option<String>,
        #[serde(default)]
        risk: Option<QuestionRisk>,
    },
    /// The user's answer travelled the four hops back (§2); the harness only closes the question.
    QuestionAnswered {
        question_id: String,
        answers: Value,
        #[serde(default)]
        response: Option<String>,
    },
    /// A turn ended: the trigger for cost accounting and the autopilot's publish decision (§6.3).
    TurnEnd {
        is_error: bool,
        result: Option<String>,
        cost_usd: Option<f64>,
        duration_ms: Option<f64>,
        /// Tokens per model of the last turn (§2 rules); only objects are stored, as before.
        #[serde(default)]
        model_usage: Option<Value>,
    },
    /// A proposed shared-memory note (§6.2). An absent or null `scope` means `repo`, the schema's
    /// default, and absent `tags` mean none.
    MemoryProposal {
        #[serde(default)]
        scope: Option<String>,
        title: String,
        content: String,
        #[serde(default)]
        tags: Vec<String>,
    },
    /// A confirmed problem outside the task (§6.6). The harness files it on the host; validation
    /// and every outcome's log line stay in `findings.rs`, which still reads the raw event.
    Finding { title: String, body: String, evidence: String },
    /// A self-paced loop's colony names its next run (loops.rs): minutes from now, and why.
    LoopNext {
        delay_minutes: u64,
        #[serde(default)]
        reason: String,
    },
    /// A loop's colony ends its loop (loops.rs).
    LoopStop {
        #[serde(default)]
        reason: String,
    },
    /// Everything the harness only forwards, and any type a newer runner adds (§2: unknown types
    /// must be ignored). A known body with broken fields lands here too: it was forwarded, it just
    /// triggers no side effects.
    #[serde(other)]
    Other,
}

impl AgentEvent {
    /// Whether a wire `type` tag names one of the variants above, i.e. a type this build acts on.
    /// The enum is that set's single source — no second list of tags to keep in step with the
    /// variants: a body carrying only the tag deserialises to a variant when the tag is known (to
    /// the variant itself when all its fields are optional, otherwise to an error over the fields
    /// still required) and to [`AgentEvent::Other`] only when it names no variant. So a variant
    /// added later is picked up here without touching this helper.
    pub(crate) fn is_acted_on(tag: &str) -> bool {
        !matches!(serde_json::from_value::<Self>(json!({"type": tag})), Ok(Self::Other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runner's contract fixture: every line must deserialise and land on the variant the
    /// harness dispatches on — or, for the forwarded-only types, deliberately on the catch-all.
    /// This is the seam between the JS runner and this enum, so drift fails a test on both sides.
    #[test]
    fn every_fixture_event_deserialises_and_lands_on_its_variant() {
        let fixture = include_str!("../../../modules/agents/claude-code/test/fixtures/events.jsonl");
        let events: Vec<AgentEvent> = fixture
            .lines()
            .map(|line| serde_json::from_str(line).expect("fixture line satisfies docs/agent-events.schema.json"))
            .collect();

        // The events the harness acts on, with the fields the dispatch reads.
        assert_eq!(
            events[0],
            AgentEvent::Status {
                state: AgentState::Idle,
                detail: None
            }
        );
        assert_eq!(
            events[1],
            AgentEvent::UserMessage {
                id: "initial".into(),
                text: "Fix the issue".into()
            }
        );
        assert!(matches!(
            &events[2],
            AgentEvent::Status {
                state: AgentState::Working,
                detail: None
            }
        ));
        assert!(matches!(
            &events[8],
            AgentEvent::Question { question_id, questions, message_id: Some(message_id), .. }
                if question_id == "toolu_ask" && message_id == "msg_1" && questions.len() == 1
        ));
        assert!(matches!(
            &events[9],
            AgentEvent::Status {
                state: AgentState::WaitingForAnswer,
                detail: None
            }
        ));
        assert!(matches!(
            &events[10],
            AgentEvent::QuestionAnswered { question_id, response: None, .. } if question_id == "toolu_ask"
        ));
        assert!(matches!(
            &events[15],
            AgentEvent::MemoryProposal { scope: Some(scope), tags, .. }
                if scope == "repo" && tags == &["workspace".to_string()]
        ));
        assert!(matches!(
            &events[16],
            AgentEvent::Finding { title, evidence, .. } if !title.is_empty() && !evidence.is_empty()
        ));
        assert!(matches!(
            &events[18],
            AgentEvent::Status {
                state: AgentState::Idle,
                detail: None
            }
        ));
        assert!(matches!(
            &events[19],
            AgentEvent::Status {
                state: AgentState::Exited,
                detail: None
            }
        ));
        match &events[17] {
            AgentEvent::TurnEnd {
                is_error,
                cost_usd,
                model_usage: Some(usage),
                ..
            } => {
                assert!(!is_error);
                assert!(cost_usd.is_some_and(|cost| cost > 0.0));
                assert!(usage.is_object());
            }
            other => panic!("the turn that ends the fixture is a turn_end, got {other:?}"),
        }

        // The forwarded-only types land on the catch-all on purpose: the browser is their consumer.
        assert_eq!(events[3], AgentEvent::Other, "log");
        assert_eq!(events[4], AgentEvent::Other, "model_changed");
        assert_eq!(events[5], AgentEvent::Other, "assistant_text_delta");
        assert_eq!(events[7], AgentEvent::Other, "assistant_text");
        assert_eq!(events[12], AgentEvent::Other, "thinking");
        assert_eq!(events[13], AgentEvent::Other, "tool_call");
        assert_eq!(events[14], AgentEvent::Other, "tool_result");
    }

    /// The regression guard for browser pass-through: a type a newer runner adds, or a known body
    /// with broken fields, must land on the catch-all rather than error out, so dispatching can
    /// never drop the line — it was already forwarded to the browser before this point (§2: unknown
    /// types must be ignored).
    #[test]
    fn an_event_outside_the_contract_lands_on_the_catch_all_rather_than_erroring() {
        assert_eq!(
            serde_json::from_str::<AgentEvent>(r#"{"type":"brand_new","payload":{}}"#).unwrap(),
            AgentEvent::Other
        );
        assert_eq!(
            serde_json::from_str::<AgentEvent>(r#"{"type":"status"}"#).unwrap_or(AgentEvent::Other),
            AgentEvent::Other
        );
    }

    /// The acted-on set is read off the enum, so the forwarded-only types and anything a newer
    /// runner adds are not acted on — even though the browser forwards every one of them — and a
    /// variant added later is acted on without a second tag list being edited.
    #[test]
    fn the_acted_on_set_is_read_off_the_enum_not_a_second_tag_list() {
        for tag in [
            "status",
            "user_message",
            "question",
            "question_answered",
            "turn_end",
            "memory_proposal",
            "finding",
            "loop_next",
            "loop_stop",
        ] {
            assert!(AgentEvent::is_acted_on(tag), "{tag} is a variant of this enum");
        }
        for tag in [
            "log",
            "assistant_text",
            "assistant_text_delta",
            "thinking",
            "tool_call",
            "tool_result",
            "model_changed",
            "brand_new",
        ] {
            assert!(!AgentEvent::is_acted_on(tag), "{tag} is forwarded only, or not known at all");
        }
    }

    /// A `status` whose state this build does not know is still a `Status`: the watchdog must keep
    /// treating it as no progress news, exactly as it treats the states it knows.
    #[test]
    fn a_status_with_an_unknown_state_is_still_a_status() {
        let event = serde_json::from_str::<AgentEvent>(r#"{"type":"status","state":"teleporting"}"#).unwrap();
        assert_eq!(
            event,
            AgentEvent::Status {
                state: AgentState::Unknown,
                detail: None
            }
        );
    }

    /// agentd stamps `seq`/`ts` onto every event (crates/colonizer-agentd/src/store.rs) and
    /// subagent events carry `agent`; neither is part of the runner contract body (§2), so neither
    /// may change what the body deserialises to.
    #[test]
    fn agentds_envelope_around_the_body_does_not_change_the_variant() {
        let stored = r#"{"type":"turn_end","is_error":false,"result":"done","cost_usd":0.42,"duration_ms":81234,
            "model_usage":{"claude-opus-5":{"input_tokens":1200,"output_tokens":300,"cache_read_tokens":90000,"cache_write_tokens":8000}},
            "agent":{"id":"toolu_1","name":"Explore","description":null},"seq":41,"ts":"2026-09-18T10:00:00.000Z"}"#;
        match serde_json::from_str::<AgentEvent>(stored).unwrap() {
            AgentEvent::TurnEnd {
                cost_usd,
                model_usage: Some(usage),
                ..
            } => {
                assert_eq!(cost_usd, Some(0.42));
                assert!(usage.is_object());
            }
            other => panic!("the envelope must not change the variant, got {other:?}"),
        }
    }

    /// The risk vocabulary is ordered lowest to highest, a question without the field counts as a
    /// workspace write (an older runner's), and a value outside the vocabulary still deserialises —
    /// as the class above every ceiling, never answered automatically. The wire parse and the
    /// replay parse (`QuestionRisk::from_wire`) must agree, since a restart moves a question
    /// between them.
    #[test]
    fn a_question_risk_is_ordered_and_tolerant_of_the_field_being_absent_or_unknown() {
        let risk = |body: &str| match serde_json::from_str::<AgentEvent>(body).unwrap() {
            AgentEvent::Question { risk, .. } => risk.unwrap_or(QuestionRisk::WorkspaceWrite),
            other => panic!("a question, got {other:?}"),
        };
        // A question without the field — or with it null — is an older runner's: a workspace write.
        assert_eq!(risk(r#"{"type":"question","question_id":"q"}"#), QuestionRisk::WorkspaceWrite);
        assert_eq!(
            risk(r#"{"type":"question","question_id":"q","risk":null}"#),
            QuestionRisk::WorkspaceWrite
        );
        // A value outside the vocabulary — a future runner's — is not a contract error: it is the
        // class above every ceiling, never answered automatically.
        assert_eq!(
            risk(r#"{"type":"question","question_id":"q","risk":"teleport_the_repo"}"#),
            QuestionRisk::Unknown
        );
        // The replay parse (sessions.rs, over the raw log lines) lands in the same places — and a
        // risk that is not even a string is never read as absent, which would answer it.
        assert_eq!(QuestionRisk::from_wire(None), QuestionRisk::WorkspaceWrite);
        assert_eq!(QuestionRisk::from_wire(Some(&Value::Null)), QuestionRisk::WorkspaceWrite);
        assert_eq!(
            QuestionRisk::from_wire(Some(&json!("teleport_the_repo"))),
            QuestionRisk::Unknown
        );
        assert_eq!(QuestionRisk::from_wire(Some(&json!(3))), QuestionRisk::Unknown);
        assert_eq!(
            QuestionRisk::from_wire(Some(&json!("credential_adjacent"))),
            QuestionRisk::CredentialAdjacent
        );

        // The whole point of the order: a class a newer runner knows sits above every known
        // ceiling, so the at-or-below check the judge reads never answers it by accident.
        assert!(QuestionRisk::ReadOnly < QuestionRisk::WorkspaceWrite);
        assert!(QuestionRisk::WorkspaceWrite < QuestionRisk::PublishAffecting);
        assert!(QuestionRisk::PublishAffecting < QuestionRisk::CredentialAdjacent);
        assert!(QuestionRisk::CredentialAdjacent < QuestionRisk::Unknown);
    }

    /// The schema's defaults, held by the type: a proposal without `scope` or `tags` proposes for
    /// the repository with no tags, and `detail` may be absent or null on a `status`.
    #[test]
    fn optional_contract_fields_are_tolerated_absent_or_null() {
        let bare = serde_json::from_str::<AgentEvent>(r#"{"type":"memory_proposal","title":"t","content":"c"}"#).unwrap();
        assert_eq!(
            bare,
            AgentEvent::MemoryProposal {
                scope: None,
                title: "t".into(),
                content: "c".into(),
                tags: vec![]
            }
        );
        let nulled =
            serde_json::from_str::<AgentEvent>(r#"{"type":"memory_proposal","scope":null,"title":"t","content":"c"}"#).unwrap();
        assert!(matches!(nulled, AgentEvent::MemoryProposal { scope: None, tags, .. } if tags.is_empty()));
        let status = serde_json::from_str::<AgentEvent>(r#"{"type":"status","state":"error","detail":null}"#).unwrap();
        assert_eq!(
            status,
            AgentEvent::Status {
                state: AgentState::Error,
                detail: None
            }
        );
    }
}
