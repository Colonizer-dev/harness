//! `colonizer mcp`: this harness as an MCP server over stdio, built on the official Rust SDK
//! (rmcp). The server is a client of the mothership's HTTP API — the same `--host` and token
//! resolution as the CLI commands, through [`Machine`] — and the tool set follows the token's
//! scope: a scoped token lists exactly what it may do, and an owner token — which may do
//! everything — defaults to read-only unless `colonizer mcp --scope` raises it (issue #508: MCP
//! defaults to read unless the configured token grants more).
//!
//! Everything an MCP client sends or reads travels on stdin/stdout as the protocol; this module's
//! own notes go to stderr only.

use crate::api_tokens::Scope;
use crate::cli::{Fail, Machine, Resolved, resolve_answer};
use anyhow::{Context as _, Result};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig},
    schemars::JsonSchema,
    service::ServiceExt,
    tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// Serves the harness to one MCP client until the transport closes. Resolves the token's standing
/// first (`GET /api/tokens/self`), because the tool set is the scope: building the router without
/// it would list tools every call of which the mothership would refuse.
pub async fn run(machine: Machine, requested: Option<Scope>) -> Result<()> {
    let who = machine
        .get("/api/tokens/self")
        .await
        .map_err(|e| anyhow::anyhow!("cannot read this token's standing from the mothership: {e}"))?;
    let effective = effective_scope(&who, requested);
    let server = ColonyServer::new(machine, effective);
    eprintln!(
        "colonizer mcp: serving {} of {} tools at scope {}",
        ROLES.len() - withheld(effective).len(),
        ROLES.len(),
        effective.as_str()
    );
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .context("the MCP stdio transport failed before the first message")?;
    running.waiting().await.context("the MCP stdio transport failed")?;
    Ok(())
}

/// The scope this server serves at: a scoped token's own scope (possibly lowered by `--scope`), or
/// read for an owner — the owner token may do everything, but an MCP client's default exposure is
/// watching unless the operator asked for more.
pub(crate) fn effective_scope(who: &Value, requested: Option<Scope>) -> Scope {
    match who["owner"].as_bool() {
        // An owner may do everything, so `--scope` only chooses how much this MCP session
        // exposes, read being the default.
        Some(true) => requested.unwrap_or(Scope::Read),
        // A scoped token never rises above its own scope; `--scope` can only lower it. An
        // unknown spelling reads as the narrowest, the same direction the registry refuses in.
        _ => {
            let granted = parse_scope(who["scope"].as_str().unwrap_or("read"));
            requested.map_or(granted, |asked| asked.min(granted))
        }
    }
}

/// Every tool with its least scope, in one table: the router is pruned by it, and so is the count
/// the startup line prints.
const ROLES: &[(&str, Scope)] = &[
    ("list_colonies", Scope::Read),
    ("colony_status", Scope::Read),
    ("colony_question", Scope::Read),
    ("colony_pr", Scope::Read),
    ("answer_colony", Scope::Operate),
    ("stop_colony", Scope::Operate),
    ("resume_colony", Scope::Operate),
    ("launch_colony", Scope::Launch),
];

/// The tools `scope` does not reach.
fn withheld(scope: Scope) -> Vec<&'static str> {
    ROLES
        .iter()
        .filter(|(_, at_least)| scope < *at_least)
        .map(|(name, _)| *name)
        .collect()
}

/// Reads the wire spelling of a scope; anything else is the narrowest.
fn parse_scope(raw: &str) -> Scope {
    match raw {
        "operate" => Scope::Operate,
        "launch" => Scope::Launch,
        _ => Scope::Read,
    }
}

// ---------------------------------------------------------------------------
// The server itself.
// ---------------------------------------------------------------------------

pub struct ColonyServer {
    machine: Machine,
    tool_router: ToolRouter<Self>,
}

/// The tool parameters, as the MCP client sends them.
#[derive(Deserialize, JsonSchema)]
struct ListColonies {
    /// Only colonies of this org (the repository owner)
    org: Option<String>,
    /// Only colonies in this state: queued, running, waiting_for_answer, pr_opened, ...
    status: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ColonyId {
    /// The colony id, as `list_colonies` or the cockpit shows it
    id: String,
}

#[derive(Deserialize, JsonSchema)]
struct AnswerColony {
    /// The colony id whose question is being answered
    id: String,
    /// An option's 1-based number, its label, or free text for the agent to read
    answer: String,
}

#[derive(Deserialize, JsonSchema)]
struct LaunchColony {
    /// The repository to work in, as owner/repo
    repo: String,
    /// Work one issue instead of the repository's own backlog
    issue: Option<u64>,
    /// The task, when the issue alone does not say it
    task: Option<String>,
    /// Run the orchestrator on this model instead of what routing would pick
    model: Option<String>,
    /// Answer routine questions itself; omitted uses the install's default
    autopilot: Option<bool>,
}

#[tool_router]
impl ColonyServer {
    fn new(machine: Machine, scope: Scope) -> Self {
        let mut tool_router = Self::tool_router();
        // The scope is the tool set: routes the token does not reach are disabled, which hides them
        // from tools/list and refuses calls with "tool not found", so a client never sees a tool it
        // would only be refused.
        for name in withheld(scope) {
            tool_router.disable_route(name);
        }
        Self { machine, tool_router }
    }

    /// The colonies this token may see, newest first, one record per colony.
    #[tool(description = "List colonies (agent sessions) this token may see, newest first")]
    async fn list_colonies(&self, Parameters(params): Parameters<ListColonies>) -> Result<CallToolResult, McpError> {
        let sessions = self.machine.get("/api/sessions").await;
        Self::rendered(sessions, |sessions| {
            let records = sessions
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|s| match &params.org {
                    None => true,
                    Some(org) => {
                        s["org"].as_str() == Some(org.as_str())
                            || s["repo"].as_str().unwrap_or_default().split('/').next() == Some(org.as_str())
                    }
                })
                .filter(|s| match &params.status {
                    None => true,
                    Some(status) => s["status"].as_str().is_some_and(|s| s.eq_ignore_ascii_case(status)),
                })
                .map(colony_record)
                .collect::<Vec<_>>();
            Value::Array(records).to_string()
        })
    }

    /// One colony: where it stands and what it is doing right now.
    #[tool(description = "Show one colony: status, branch, pull request, cost, and what it is doing now")]
    async fn colony_status(&self, Parameters(params): Parameters<ColonyId>) -> Result<CallToolResult, McpError> {
        let detail = self.machine.get(&format!("/api/sessions/{}", params.id)).await;
        Self::rendered(detail, |detail| detail.to_string())
    }

    /// The question a colony is waiting on, with its options.
    #[tool(description = "Read the question a colony is waiting on, with its numbered options and risk")]
    async fn colony_question(&self, Parameters(params): Parameters<ColonyId>) -> Result<CallToolResult, McpError> {
        match self
            .machine
            .get_optional(&format!("/api/sessions/{}/question", params.id))
            .await
        {
            // The 204 of a colony that is not asking is an answer, not an error.
            Ok(Some(question)) => text_result(question.to_string()),
            Ok(None) => text_result("no question is pending for this colony"),
            Err(fail) => is_error(fail),
        }
    }

    /// A colony's pull request, or the words that there is none yet.
    #[tool(description = "Show a colony's pull request URL and state, or that there is none yet")]
    async fn colony_pr(&self, Parameters(params): Parameters<ColonyId>) -> Result<CallToolResult, McpError> {
        let detail = self.machine.get(&format!("/api/sessions/{}", params.id)).await;
        Self::rendered(detail, |detail| match detail["pr_url"].as_str() {
            Some(url) => {
                let state = detail["status"].as_str().unwrap_or("unknown");
                let checks = detail["ci_state"]
                    .as_str()
                    .map(|c| format!(" (checks {c})"))
                    .unwrap_or_default();
                format!("{url} — {state}{checks}")
            }
            None => format!("{}: no pull request yet", params.id),
        })
    }

    /// Answer a colony's question: option number, option label, or free text.
    #[tool(description = "Answer a colony's pending question: an option's 1-based number, its label, or free text")]
    async fn answer_colony(&self, Parameters(params): Parameters<AnswerColony>) -> Result<CallToolResult, McpError> {
        let question = match self
            .machine
            .get_optional(&format!("/api/sessions/{}/question", params.id))
            .await
        {
            // Nothing to answer: the colony is not asking anything.
            Ok(None) => return is_error("no question is pending for this colony"),
            Ok(Some(question)) => question,
            Err(fail) => return is_error(fail),
        };
        let pending: crate::cli::PendingQuestion = match serde_json::from_value(question) {
            Ok(pending) => pending,
            Err(e) => return is_error(format!("the question came back in a shape this server cannot read: {e}")),
        };
        let resolved = match resolve_answer(&params.answer, &pending) {
            Ok(resolved) => resolved,
            Err(e) => return is_error(e),
        };
        let body = resolved.answer_body(&pending.question_id);
        let what = match &resolved {
            Resolved::Choice { label, .. } => format!("chose \"{label}\""),
            Resolved::FreeText { .. } => "sent a free-text note".to_string(),
        };
        let answer = self
            .machine
            .post(&format!("/api/sessions/{}/answer", params.id), Some(&body))
            .await;
        Self::rendered(Self::sent(answer), |_| format!("{}: answer sent, {what}", params.id))
    }

    /// Stop a colony; the worktree is kept for a later resume.
    #[tool(description = "Stop a colony: its microVM goes away, its worktree is kept for a later resume")]
    async fn stop_colony(&self, Parameters(params): Parameters<ColonyId>) -> Result<CallToolResult, McpError> {
        let stop = self.machine.post(&format!("/api/sessions/{}/stop", params.id), None).await;
        Self::rendered(Self::sent(stop), |reply| {
            format!("{}: {}", params.id, reply["result"].as_str().unwrap_or("stopped"))
        })
    }

    /// Start a stopped colony again.
    #[tool(description = "Resume a stopped colony, picking up its worktree where it was left")]
    async fn resume_colony(&self, Parameters(params): Parameters<ColonyId>) -> Result<CallToolResult, McpError> {
        let resume = self.machine.post(&format!("/api/sessions/{}/resume", params.id), None).await;
        Self::rendered(Self::sent(resume), |session| {
            format!("{}: resumed ({})", params.id, session["status"].as_str().unwrap_or("queued"))
        })
    }

    /// Start a colony on owner/repo, on one issue or free.
    #[tool(description = "Launch a colony: an agent in a microVM, working the repo, or one issue in it")]
    async fn launch_colony(&self, Parameters(params): Parameters<LaunchColony>) -> Result<CallToolResult, McpError> {
        let body = json!({
            "repo": params.repo,
            "issue": params.issue,
            "instructions": params.task.unwrap_or_default(),
            "autopilot": params.autopilot,
            "model_override": params.model,
        });
        let launch = self.machine.post("/api/sessions", Some(&body)).await;
        Self::rendered(Self::sent(launch), |session| {
            json!({ "id": session["id"], "status": session["status"] }).to_string()
        })
    }

    /// The POST routes answer 204 on success — nothing to read, and that is success.
    fn sent(outcome: Result<Option<Value>, Fail>) -> Result<Value, Fail> {
        outcome.map(|body| body.unwrap_or(Value::Null))
    }

    /// One mothership round trip turned into a tool result: the body rendered by `render`, or the
    /// refusal as an `is_error` result carrying the server's own words — never a protocol failure
    /// the caller cannot read.
    fn rendered(outcome: Result<Value, Fail>, render: impl Fn(&Value) -> String) -> Result<CallToolResult, McpError> {
        match outcome {
            Ok(body) => text_result(render(&body)),
            Err(fail) => is_error(fail),
        }
    }
}

/// A tool that could not do its job, with the reason the caller can read. The MCP `isError` bit
/// marks it; the text is the message.
fn is_error(message: impl std::fmt::Display) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![ContentBlock::text(message.to_string())]))
}

/// A successful tool result whose text is a sentence.
fn text_result(text: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(text.into())]))
}

/// One colony as the tools report it: what an MCP client needs, not the whole persisted record.
fn colony_record(s: Value) -> Value {
    json!({
        "id": s["id"],
        "repo": s["repo"],
        "status": s["status"],
        "issue": s["issue"],
        "title": s["issue_title"],
        "pr_url": s["pr_url"],
        "cost_usd": s["cost_usd"],
    })
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ColonyServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Drive a Colonizer harness: list colonies (agent sessions working GitHub repositories in \
             microVMs), read one's status or pending question, answer it, stop or resume it, or launch \
             a new one. Launching needs a token with launch scope.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::PendingQuestion;
    use axum::{
        Json, Router,
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse as _,
        routing::{get, post},
    };
    use rmcp::model::CallToolRequestParams;

    fn machine(base: &str, token: &str) -> Machine {
        Machine::for_tests(base.to_string(), token.to_string())
    }

    fn call(name: &'static str, arguments: Value) -> CallToolRequestParams {
        CallToolRequestParams::new(name).with_arguments(arguments.as_object().expect("object arguments").clone())
    }

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|block| block.as_text())
            .map(|text| text.text.clone())
            .unwrap_or_default()
    }

    /// The scope pruning keeps exactly the read tools from a read-scoped server, everything from a
    /// launch-scoped one, and operate plus read from an operate-scoped one. `tools/list` reports
    /// names sorted, so the expectations are sorted too.
    #[test]
    fn the_router_lists_exactly_what_the_scope_reaches() {
        let names = |scope| ColonyServer::new(machine("http://127.0.0.1:9", "col"), scope).tool_names();
        let sorted = |mut v: Vec<&str>| {
            v.sort_unstable();
            v.into_iter().map(str::to_string).collect::<Vec<_>>()
        };
        assert_eq!(
            names(Scope::Read),
            sorted(vec!["list_colonies", "colony_status", "colony_question", "colony_pr"])
        );
        assert_eq!(
            names(Scope::Operate),
            sorted(vec![
                "list_colonies",
                "colony_status",
                "colony_question",
                "colony_pr",
                "answer_colony",
                "stop_colony",
                "resume_colony"
            ])
        );
        assert_eq!(names(Scope::Launch), sorted(ROLES.iter().map(|(name, _)| *name).collect()));
    }

    impl ColonyServer {
        fn tool_names(&self) -> Vec<String> {
            self.tool_router.list_all().iter().map(|t| t.name.to_string()).collect()
        }
    }

    /// `--scope` lowers a scoped token, never raises it: the effective scope is the smaller, and an
    /// owner's read default is only a floor the operator's flag raises.
    #[test]
    fn a_requested_scope_can_lower_but_never_raise() {
        let owned = json!({"owner": true, "scope": "owner"});
        assert_eq!(effective_scope(&owned, None), Scope::Read);
        assert_eq!(effective_scope(&owned, Some(Scope::Launch)), Scope::Launch);
        assert_eq!(effective_scope(&owned, Some(Scope::Operate)), Scope::Operate);

        let launch = json!({"owner": false, "scope": "launch"});
        assert_eq!(effective_scope(&launch, None), Scope::Launch);
        assert_eq!(effective_scope(&launch, Some(Scope::Operate)), Scope::Operate, "lowered");
        assert_eq!(effective_scope(&launch, Some(Scope::Launch)), Scope::Launch);

        let read = json!({"owner": false, "scope": "read"});
        assert_eq!(effective_scope(&read, Some(Scope::Launch)), Scope::Read, "never raised");
    }

    // -- The stub mothership and the in-process MCP conformance smoke test. ----------------

    /// What the stub mothership answers per bearer token: the scope line decides the tool set.
    #[derive(Clone)]
    struct StubState {
        token: &'static str,
    }

    impl StubState {
        fn scope(&self) -> &'static str {
            match self.token {
                "col_launch" => "launch",
                "col_operate" => "operate",
                _ => "read",
            }
        }
    }

    fn stub(state: StubState) -> Router {
        Router::new()
            .route(
                "/api/tokens/self",
                get(|State(state): State<StubState>| async move {
                    Json(json!({"owner": false, "name": "stub", "scope": state.scope(), "orgs": [], "repos": []}))
                }),
            )
            .route(
                "/api/sessions",
                get(|| async {
                    Json(json!([
                        {"id": "abc123", "org": "acme", "repo": "acme/app", "status": "running", "issue": 5,
                         "issue_title": "Fix the deploy", "pr_url": null, "cost_usd": 0.25},
                    ]))
                }),
            )
            .route(
                "/api/sessions/{id}",
                get(|| async {
                    Json(json!({
                        "id": "abc123", "org": "acme", "repo": "acme/app", "status": "pr_opened",
                        "branch": "colonizer/issue-5-abc123", "pr_url": "https://github.com/acme/app/pull/99",
                        "ci_state": "success", "cost_usd": 0.5,
                        "diagnosis": {"state": "working", "text": "pushed, waiting for a review"},
                    }))
                }),
            )
            .route(
                "/api/sessions/{id}/question",
                get(|Path(id): Path<String>| async move {
                    // The "quiet" colony is not asking anything: the route's 204, no body.
                    if id == "quiet" {
                        return StatusCode::NO_CONTENT.into_response();
                    }
                    Json(json!({
                        "question_id": "q1", "risk": "workspace_write",
                        "questions": [{"question": "Push the branch now?", "multi_select": false,
                                       "options": [{"label": "Push now"}, {"label": "Wait"}]}],
                    }))
                    .into_response()
                }),
            )
            .route(
                "/api/sessions/{id}/answer",
                post(|State(state): State<StubState>| async move {
                    if state.scope() == "operate" {
                        // The operate token may ask, but the mothership still refuses this answer.
                        let body = Json(json!({"error": "this answer is not yours to give"}));
                        return (StatusCode::FORBIDDEN, body).into_response();
                    }
                    StatusCode::NO_CONTENT.into_response()
                }),
            )
            .with_state(state)
    }

    async fn serve_stub(token: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, stub(StubState { token })).await.unwrap() });
        format!("http://{addr}")
    }

    /// Serves a real `ColonyServer` over one half of a duplex and an rmcp test client over the
    /// other, so the conformance path is the wire, not the impl. The server reads its standing
    /// from the stub the same way `run` does.
    async fn connected(
        base: &str,
        token: &'static str,
        requested: Option<Scope>,
    ) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
        let who = machine(base, token).get("/api/tokens/self").await.unwrap();
        let server = ColonyServer::new(machine(base, token), effective_scope(&who, requested));
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            if let Ok(running) = server.serve(server_io).await
                && let Err(e) = running.waiting().await
            {
                eprintln!("the MCP server under test stopped: {e:?}");
            }
        });
        ().serve(client_io).await.unwrap()
    }

    /// The whole conformance path over the wire: initialize, the read scope's exact tool list, a
    /// `list_colonies` and `colony_status` against the stub through the real HTTP client.
    #[tokio::test]
    async fn the_mcp_server_conforms_over_a_wire_against_a_stub_mothership() {
        let base = serve_stub("col_read").await;
        let client = connected(&base, "col_read", None).await;

        // Read scope: exactly the four read tools, none of the driving or launching ones. The
        // protocol reports tool names sorted.
        let listed = client.peer().list_tools(Default::default()).await.unwrap();
        let mut names: Vec<&str> = listed.tools.iter().map(|t| t.name.as_ref()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["colony_pr", "colony_question", "colony_status", "list_colonies"]);

        // list_colonies answers the stub's session list, reduced to the tool's record shape.
        let result = client.peer().call_tool(call("list_colonies", json!({}))).await.unwrap();
        assert_ne!(result.is_error, Some(true));
        assert!(text_of(&result).contains("acme/app"), "{}", text_of(&result));

        // colony_status answers the detail body, diagnosis included.
        let result = client
            .peer()
            .call_tool(call("colony_status", json!({"id": "abc123"})))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        assert!(text_of(&result).contains("waiting for a review"), "{}", text_of(&result));

        // colony_pr reads the pull request off the session.
        let result = client
            .peer()
            .call_tool(call("colony_pr", json!({"id": "abc123"})))
            .await
            .unwrap();
        assert!(
            text_of(&result).contains("github.com/acme/app/pull/99"),
            "{}",
            text_of(&result)
        );
        let _ = client.cancel().await;
    }

    /// A launch-scoped token lists every tool, including `launch_colony`.
    #[tokio::test]
    async fn a_launch_scoped_token_lists_every_tool() {
        let base = serve_stub("col_launch").await;
        let client = connected(&base, "col_launch", None).await;
        let listed = client.peer().list_tools(Default::default()).await.unwrap();
        let mut names: Vec<&str> = listed.tools.iter().map(|t| t.name.as_ref()).collect();
        names.sort_unstable();
        let mut every: Vec<&str> = ROLES.iter().map(|(name, _)| *name).collect();
        every.sort_unstable();
        assert_eq!(names, every);
        let _ = client.cancel().await;
    }

    /// The mothership's refusal of an answer is a tool error carrying its words, not a broken call.
    #[tokio::test]
    async fn a_refused_answer_comes_back_as_an_is_error_result() {
        let base = serve_stub("col_operate").await;
        let client = connected(&base, "col_operate", None).await;
        let listed = client.peer().list_tools(Default::default()).await.unwrap();
        let names: Vec<&str> = listed.tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"answer_colony"), "{names:?}");
        assert!(!names.contains(&"launch_colony"), "{names:?}");

        let result = client
            .peer()
            .call_tool(call("answer_colony", json!({"id": "abc123", "answer": "Push now"})))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true), "{}", text_of(&result));
        assert!(text_of(&result).contains("not yours to give"), "{}", text_of(&result));
        let _ = client.cancel().await;
    }

    /// The question a stub colony asks reaches the client, options and risk and all — and a
    /// colony that is not asking reads as a plain sentence, not an error.
    #[tokio::test]
    async fn colony_question_reads_the_pending_question() {
        let base = serve_stub("col_read").await;
        let client = connected(&base, "col_read", None).await;
        let result = client
            .peer()
            .call_tool(call("colony_question", json!({"id": "abc123"})))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let pending: PendingQuestion = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!((pending.question_id.as_str(), pending.questions.len()), ("q1", 1));

        let result = client
            .peer()
            .call_tool(call("colony_question", json!({"id": "quiet"})))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        assert_eq!(text_of(&result), "no question is pending for this colony");
        let _ = client.cancel().await;
    }

    /// Guarding the router-pruning table: the withheld sets are what the tool-list tests read.
    #[test]
    fn withheld_tools_track_the_table() {
        assert_eq!(withheld(Scope::Read).len(), 4);
        assert_eq!(withheld(Scope::Operate), vec!["launch_colony"]);
        assert!(withheld(Scope::Launch).is_empty());
        // An unknown response shape reads as the narrowest scope.
        assert_eq!(parse_scope("root"), Scope::Read);
    }
}
