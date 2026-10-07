//! Chat tools (issue #1217): the cockpit chat's model can look at, and act on, this mothership.
//!
//! Every tool is classified as a **read** or a **write**. A read runs the moment the model asks for
//! it. A write never does: it becomes a pending **approval** that carries the tool, its arguments, a
//! plain-language summary, a dry-run diff where the API supports `dry_run`, and the blast radius.
//! Nothing runs until `POST /api/chat/approvals/{id}` says `approve`, `edit` or `reject`, and that
//! call settles the approval exactly once: the first decision claims it under a lock, a second one
//! is a 409. Each decision is a line in the activity log (`chat.approve`, `chat.reject`) naming the
//! chat message that proposed it.
//!
//! Guardrails, applied when a call is proposed and again when it is approved (state moves): secrets
//! are never read or written (the tools take no secret arguments and any argument that looks like
//! one is refused; the model is pointed at Settings → Secrets); a security hold is never released
//! through chat; an org switched off (hidden) is never touched; and approvals are rate-limited.
//!
//! The tools call the mothership's own API in-process, through the same router and layers a request
//! from outside goes through, so a tool does exactly what the cockpit button does and the activity
//! log records it the same way. Chat is reachable only by the owner's cockpit (and a paired phone),
//! so the calls run as the owner who is signed in.

use crate::{ApiResult, Shared, client_error, sessions::SessionStatus, util::short_id};
use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode, header},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use tower::ServiceExt as _;

/// The most tool rounds one reply may take before it must answer in words.
pub const MAX_ROUNDS: usize = 5;
/// Approved writes allowed per minute across the install: a runaway model cannot flood the API.
const WRITES_PER_MINUTE: usize = 10;
/// Pending approvals one install keeps; a flood of proposals is refused instead of piling up.
const PENDING_LIMIT: usize = 40;
/// The stored approvals: the newest are kept.
const KEEP: usize = 500;
/// A tool's result, as far as the model and the card show it.
const RESULT_LIMIT: usize = 12_000;
/// Free text a write carries (a task, an answer) is shown to the user whole, so it is capped: a
/// longer one is refused rather than summarised, because what is approved must be what runs.
const TEXT_LIMIT: usize = 1_500;
/// A held write nobody decided goes stale: the world it described has moved on.
const APPROVAL_TTL_SECS: i64 = 2 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Read,
    Write,
}

/// One tool: its name, class, what the model is told, and its arguments as `(name, type, help)`.
struct Spec {
    name: &'static str,
    kind: Kind,
    description: &'static str,
    args: &'static [(&'static str, &'static str, &'static str)],
    required: &'static [&'static str],
}

const SPECS: &[Spec] = &[
    Spec {
        name: "list_colonies",
        kind: Kind::Read,
        description: "List colonies (agent sessions), newest first. Defaults to the workspace the chat is in.",
        args: &[
            ("org", "string", "Only this org"),
            (
                "status",
                "string",
                "Only this status: queued, running, waiting_for_answer, pr_opened, failed, ...",
            ),
            ("limit", "integer", "At most this many (default 20)"),
        ],
        required: &[],
    },
    Spec {
        name: "colony_status",
        kind: Kind::Read,
        description: "One colony in full: status, branch, pull request, cost, what it is doing.",
        args: &[("id", "string", "The colony id")],
        required: &["id"],
    },
    Spec {
        name: "colony_question",
        kind: Kind::Read,
        description: "The question a colony is waiting on, with its options.",
        args: &[("id", "string", "The colony id")],
        required: &["id"],
    },
    Spec {
        name: "list_issues",
        kind: Kind::Read,
        description: "Open issues of a repository.",
        args: &[("repo", "string", "owner/name")],
        required: &["repo"],
    },
    Spec {
        name: "list_loops",
        kind: Kind::Read,
        description: "The loops (scheduled recurring colonies) and their cadence.",
        args: &[],
        required: &[],
    },
    Spec {
        name: "list_providers",
        kind: Kind::Read,
        description: "The model providers and the models they serve. Never key values.",
        args: &[],
        required: &[],
    },
    Spec {
        name: "model_assignments",
        kind: Kind::Read,
        description: "Which model each role runs on, for the install and for each org.",
        args: &[],
        required: &[],
    },
    Spec {
        name: "list_orgs",
        kind: Kind::Read,
        description: "The orgs and their settings.",
        args: &[],
        required: &[],
    },
    Spec {
        name: "recent_activity",
        kind: Kind::Read,
        description: "The latest lines of the activity log.",
        args: &[("limit", "integer", "How many (default 20)")],
        required: &[],
    },
    Spec {
        name: "stop_colony",
        kind: Kind::Write,
        description: "Stop a colony: its microVM goes away, its worktree is kept for a resume.",
        args: &[("id", "string", "The colony id")],
        required: &["id"],
    },
    Spec {
        name: "resume_colony",
        kind: Kind::Write,
        description: "Resume a stopped colony.",
        args: &[("id", "string", "The colony id")],
        required: &["id"],
    },
    Spec {
        name: "publish_colony",
        kind: Kind::Write,
        description: "Open the pull request for a colony's work.",
        args: &[("id", "string", "The colony id")],
        required: &["id"],
    },
    Spec {
        name: "answer_colony",
        kind: Kind::Write,
        description: "Answer the question a colony is waiting on: an option's number or label, or free text.",
        args: &[
            ("id", "string", "The colony id"),
            ("answer", "string", "Option number, label or free text"),
        ],
        required: &["id", "answer"],
    },
    Spec {
        name: "launch_colony",
        kind: Kind::Write,
        description: "Start a colony on a repository, on an issue or on a task.",
        args: &[
            ("repo", "string", "owner/name"),
            ("issue", "integer", "The issue to work"),
            ("task", "string", "What to do, when no issue says it"),
        ],
        required: &["repo"],
    },
    Spec {
        name: "move_to_front",
        kind: Kind::Write,
        description: "Move a queued colony to the front of the start queue.",
        args: &[("id", "string", "The colony id")],
        required: &["id"],
    },
    Spec {
        name: "move_to_back",
        kind: Kind::Write,
        description: "Move a queued colony to the back of the start queue.",
        args: &[("id", "string", "The colony id")],
        required: &["id"],
    },
    Spec {
        name: "set_priority",
        kind: Kind::Write,
        description: "Set a queued colony's own priority (higher starts first; 10 is High).",
        args: &[("id", "string", "The colony id"), ("priority", "integer", "The priority")],
        required: &["id", "priority"],
    },
    Spec {
        name: "switch_models",
        kind: Kind::Write,
        description: "Switch which model the roles run on, for the install or one org. `roles` maps a role (model, subagent_model, background_model, ...) to provider/model. `apply` is `new` (colonies started from now on) or `running` (also restart the running ones).",
        args: &[
            ("scope", "string", "install or org"),
            ("org", "string", "The org, with scope org"),
            ("roles", "object", "Role to provider/model"),
            ("apply", "string", "new or running"),
        ],
        required: &["scope", "roles"],
    },
    Spec {
        name: "run_loop_now",
        kind: Kind::Write,
        description: "Run a loop once, now.",
        args: &[("id", "string", "The loop id")],
        required: &["id"],
    },
    Spec {
        name: "apply_update",
        kind: Kind::Write,
        description: "Install the newer Colonizer release, when one is out.",
        args: &[],
        required: &[],
    },
];

fn spec(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.name == name)
}

/// A tool's class, or `None` for a name that is not a tool.
pub fn kind_of(name: &str) -> Option<Kind> {
    spec(name).map(|s| s.kind)
}

/// The tools as the Anthropic Messages API takes them.
pub fn anthropic_tools() -> Value {
    Value::Array(
        SPECS
            .iter()
            .map(|s| {
                let props: serde_json::Map<String, Value> = s
                    .args
                    .iter()
                    .map(|(name, ty, help)| ((*name).to_string(), json!({"type": ty, "description": help})))
                    .collect();
                let kind = if s.kind == Kind::Write {
                    " Needs the user's approval before it runs."
                } else {
                    ""
                };
                json!({
                    "name": s.name,
                    "description": format!("{}{kind}", s.description),
                    "input_schema": {"type": "object", "properties": props, "required": s.required},
                })
            })
            .collect(),
    )
}

/// What the model is told about its tools, appended to the conversation's system prompt.
pub const SYSTEM_NOTE: &str = "\n\nYou are connected to this Colonizer mothership through tools. Reads run at once. \
Every change is held for the user's approval: call the write tool with exact arguments and say in a sentence what it will do; \
never claim a change happened until the tool result says it ran. Secret values are never read or written through chat: send the \
user to Settings → Secrets. A security hold can only be released by a person in the cockpit.";

// ---------------------------------------------------------------------------
// Calls, plans and guardrails
// ---------------------------------------------------------------------------

/// An API call a tool makes.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub method: &'static str,
    pub path: String,
    pub body: Option<Value>,
}

/// An id that is safe in a path.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !id.contains("..")
}

fn arg_str<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args[name]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("`{name}` is required"))
}

fn arg_id<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    let id = arg_str(args, name)?;
    if plain_id(id) {
        Ok(id)
    } else {
        Err(format!("`{name}` is not a valid id"))
    }
}

/// The API call a tool's arguments make. Pure and strict: an unknown argument is refused rather than
/// ignored, so what is approved is exactly what runs.
pub fn plan(name: &str, args: &Value) -> Result<Plan, String> {
    let s = spec(name).ok_or_else(|| format!("no such tool: {name}"))?;
    let object = args.as_object().ok_or("the arguments must be an object")?;
    for key in object.keys() {
        if !s.args.iter().any(|(a, _, _)| a == key) {
            return Err(format!("`{key}` is not an argument of {name}"));
        }
    }
    let get = |path: String| Plan {
        method: "GET",
        path,
        body: None,
    };
    let post = |path: String, body: Option<Value>| Plan {
        method: "POST",
        path,
        body,
    };
    Ok(match name {
        "colony_status" => get(format!("/api/sessions/{}", arg_id(args, "id")?)),
        "colony_question" => get(format!("/api/sessions/{}/question", arg_id(args, "id")?)),
        "list_issues" => {
            let repo = arg_str(args, "repo")?;
            let (owner, name) = repo
                .split_once('/')
                .filter(|_| crate::util::valid_repo(repo))
                .ok_or("`repo` is owner/name")?;
            get(format!("/api/repos/{owner}/{name}/issues"))
        }
        "list_loops" => get("/api/loops".into()),
        "list_providers" => get("/api/providers".into()),
        "model_assignments" => get("/api/models/assignments".into()),
        "list_orgs" => get("/api/orgs".into()),
        "recent_activity" => get(format!(
            "/api/activity?limit={}",
            args["limit"].as_u64().unwrap_or(20).clamp(1, 50)
        )),
        // Served from memory, not over HTTP: see `run_read`.
        "list_colonies" => get("/api/sessions".into()),
        "stop_colony" => post(format!("/api/sessions/{}/stop", arg_id(args, "id")?), None),
        "resume_colony" => post(format!("/api/sessions/{}/resume", arg_id(args, "id")?), None),
        "publish_colony" => post(format!("/api/sessions/{}/publish", arg_id(args, "id")?), None),
        "move_to_front" => post(format!("/api/sessions/{}/move-to-front", arg_id(args, "id")?), None),
        "move_to_back" => post(format!("/api/sessions/{}/move-to-back", arg_id(args, "id")?), None),
        "set_priority" => {
            let priority = args["priority"]
                .as_i64()
                .filter(|n| crate::queue_priority::valid_priority(*n))
                .ok_or("`priority` is a whole number")?;
            post(
                format!("/api/sessions/{}/priority", arg_id(args, "id")?),
                Some(json!({"priority": priority})),
            )
        }
        "answer_colony" => {
            // The body needs the pending question's id, so the call is finished at run time
            // (`finish_answer`); the plan names the route and carries the answer.
            arg_str(args, "answer")?;
            post(
                format!("/api/sessions/{}/answer", arg_id(args, "id")?),
                Some(json!({"answer": args["answer"]})),
            )
        }
        "launch_colony" => {
            let repo = arg_str(args, "repo")?;
            if !crate::util::valid_repo(repo) {
                return Err("`repo` is owner/name".into());
            }
            let mut body = json!({"repo": repo, "origin": "chat"});
            if let Some(issue) = args.get("issue").filter(|v| !v.is_null()) {
                body["issue"] = json!(issue.as_u64().ok_or("`issue` is a number")?);
            }
            if let Some(task) = args["task"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                body["instructions"] = json!(task);
            }
            if body.get("issue").is_none() && body.get("instructions").is_none() {
                return Err("name an `issue` or a `task`".into());
            }
            post("/api/sessions".into(), Some(body))
        }
        "switch_models" => {
            let scope = arg_str(args, "scope")?;
            if scope != "install" && scope != "org" {
                return Err("`scope` is install or org".into());
            }
            let roles = args["roles"]
                .as_object()
                .filter(|r| !r.is_empty())
                .ok_or("`roles` maps a role to a model")?;
            if roles.values().any(|v| !v.is_string() && !v.is_null()) {
                return Err("each role maps to a model name".into());
            }
            let mut body = json!({"scope": scope, "roles": roles, "apply": args["apply"].as_str().unwrap_or("new")});
            if scope == "org" {
                body["org"] = json!(arg_str(args, "org")?);
            }
            post("/api/models/switch".into(), Some(body))
        }
        "run_loop_now" => post(format!("/api/loops/{}/run-now", arg_id(args, "id")?), None),
        "apply_update" => post("/api/update/apply".into(), None),
        _ => return Err(format!("no such tool: {name}")),
    })
}

/// The preview's twin: the same call, asked only to say what it would do. `None` when the API has no
/// `dry_run` for it. The body is the plan's own, plus the flag, so the preview cannot drift from
/// what runs.
pub fn dry_run_of(plan: &Plan) -> Option<Plan> {
    if plan.path != "/api/models/switch" {
        return None;
    }
    let mut body = plan.body.clone()?;
    body["dry_run"] = json!(true);
    Some(Plan {
        body: Some(body),
        ..plan.clone()
    })
}

/// Argument names that carry secrets: a call naming one is refused, read or write.
const SECRET_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "key",
    "token",
    "secret",
    "password",
    "credential",
    "private_key",
    "passphrase",
];

fn names_a_secret(value: &Value) -> bool {
    match value {
        Value::Object(map) => map
            .iter()
            .any(|(k, v)| SECRET_KEYS.contains(&k.to_ascii_lowercase().as_str()) || names_a_secret(v)),
        Value::Array(list) => list.iter().any(names_a_secret),
        _ => false,
    }
}

/// Why a call is refused outright, or `None`. Checked at proposal and again at approval.
pub async fn refusal(app: &Shared, name: &str, args: &Value) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if spec(name).is_none() {
        if ["secret", "key", "token", "vault", "password", "credential"]
            .iter()
            .any(|w| lower.contains(w))
        {
            return Some("Secrets are never read or written through chat. Open Settings → Secrets.".into());
        }
        if lower.contains("hold") || lower.contains("release") || lower.contains("security") {
            return Some("A security hold can only be released by a person in the cockpit, not through chat.".into());
        }
        return Some(format!("There is no tool called {name}."));
    }
    if names_a_secret(args) {
        return Some("Secret values are never passed through chat. Open Settings → Secrets.".into());
    }
    // The colony a call targets: a security hold is a person's to release, and a hidden org is off limits.
    let mut orgs: Vec<String> = Vec::new();
    if let Some(id) = args["id"].as_str().filter(|_| !matches!(name, "run_loop_now"))
        && let Some(s) = app.sessions.read().await.iter().find(|s| s.id == id)
    {
        if matches!(name, "resume_colony" | "answer_colony" | "publish_colony")
            && crate::playbook::is_security_hold(s.attention.as_ref())
        {
            return Some(format!(
                "Colony {id} is on a security hold. Only a person can release it, in the cockpit."
            ));
        }
        orgs.push(if s.org.is_empty() {
            s.repo.split('/').next().unwrap_or_default().to_string()
        } else {
            s.org.clone()
        });
    }
    if let Some(repo) = args["repo"].as_str() {
        orgs.push(repo.split('/').next().unwrap_or_default().to_string());
    }
    if let Some(org) = args["org"].as_str() {
        orgs.push(org.to_string());
    }
    // A loop's run starts a colony in the loop's own org and repository.
    if name == "run_loop_now"
        && let Some(id) = args["id"].as_str()
        && let Some(l) = app.loops.get(id).await
    {
        orgs.push(if l.org.is_empty() {
            l.repo.split('/').next().unwrap_or_default().to_string()
        } else {
            l.org.clone()
        });
    }
    let saved: std::collections::BTreeMap<String, crate::orgs::OrgSettings> = if kind_of(name) == Some(Kind::Write) {
        crate::util::read_json_or_default(&app.orgs_file()).unwrap_or_default()
    } else {
        Default::default()
    };
    let hidden = |org: &str| {
        saved
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case(org) && v.enabled == Some(false))
    };
    if kind_of(name) == Some(Kind::Write) {
        for org in &orgs {
            if hidden(org) {
                return Some(format!("{org} is switched off in this cockpit. Chat does not touch it."));
            }
        }
    }
    // `apply: running` stops and resumes every colony in scope, which is a resume by another route:
    // it must not reach a colony on a security hold or one in a switched-off org.
    if name == "switch_models" && args["apply"].as_str() == Some("running") {
        let scope_org = (args["scope"].as_str() == Some("org")).then(|| args["org"].as_str().unwrap_or_default().to_string());
        for s in app.sessions.read().await.iter() {
            let org = if s.org.is_empty() {
                s.repo.split('/').next().unwrap_or_default().to_string()
            } else {
                s.org.clone()
            };
            let in_play =
                !s.cleaned_up && (s.status.is_live() || matches!(s.status, SessionStatus::Parked | SessionStatus::Queued));
            if !in_play || scope_org.as_deref().is_some_and(|o| !o.eq_ignore_ascii_case(&org)) {
                continue;
            }
            if crate::playbook::is_security_hold(s.attention.as_ref()) {
                return Some(format!(
                    "Colony {} is on a security hold and this switch would restart it. Only a person can release a hold, in the cockpit; switch for new colonies only, or release it first.",
                    s.id
                ));
            }
            if hidden(&org) {
                return Some(format!(
                    "{org} is switched off in this cockpit and this switch would restart its colonies. Chat does not touch it."
                ));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Running a call
// ---------------------------------------------------------------------------

/// Calls the mothership's own API through its router, as the owner.
async fn call(app: &Shared, plan: &Plan) -> Result<Value, String> {
    let mut builder = Request::builder()
        .method(plan.method)
        .uri(&plan.path)
        .header(header::HOST, "127.0.0.1")
        .header(header::AUTHORIZATION, format!("Bearer {}", app.api_token));
    let body = match &plan.body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let request = builder.body(body).map_err(|e| e.to_string())?;
    let response = crate::server::router(app)
        .oneshot(request)
        .await
        .map_err(|e| format!("the mothership did not answer: {e}"))?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .map_err(|e| e.to_string())?;
    let parsed =
        serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    if status.is_success() {
        Ok(if status == StatusCode::NO_CONTENT {
            json!("nothing pending")
        } else {
            parsed
        })
    } else {
        let why = parsed["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| parsed.to_string());
        Err(format!("{status}: {}", crate::util::truncate(&why, 300)))
    }
}

/// A result the model and the card may show: secret-looking fields dropped, credentials redacted,
/// clipped.
pub fn scrub(value: &Value) -> String {
    fn strip(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter(|(k, _)| {
                        let k = k.to_ascii_lowercase();
                        !(SECRET_KEYS.contains(&k.as_str())
                            || k.ends_with("_key")
                            || k.ends_with("_secret")
                            || k.ends_with("_token")
                            || k == "authorization")
                    })
                    .map(|(k, v)| (k.clone(), strip(v)))
                    .collect(),
            ),
            Value::Array(list) => Value::Array(list.iter().map(strip).collect()),
            other => other.clone(),
        }
    }
    let text = match value {
        Value::String(s) => s.clone(),
        other => strip(other).to_string(),
    };
    crate::util::truncate(&crate::redact::redact_line(&text), RESULT_LIMIT)
}

/// A colony as the chat shows it: the few fields that answer "what is going on".
fn colony_record(s: &crate::sessions::Session) -> Value {
    json!({
        "id": s.id,
        "repo": s.repo,
        "status": s.status,
        "issue": s.issue,
        "title": if s.issue_title.is_empty() { s.summary.clone().unwrap_or_default() } else { s.issue_title.clone() },
        "pr_url": s.pr_url,
    })
}

async fn run_read(app: &Shared, name: &str, args: &Value, workspace: Option<&str>) -> Result<Value, String> {
    if name == "list_colonies" {
        let org = args["org"].as_str().or(workspace);
        let status = args["status"].as_str();
        let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 100) as usize;
        let sessions = app.sessions.read().await;
        let mut list: Vec<&crate::sessions::Session> = sessions
            .iter()
            .filter(|s| org.is_none_or(|o| s.repo.split('/').next().is_some_and(|x| x.eq_ignore_ascii_case(o))))
            .filter(|s| {
                status.is_none_or(|st| {
                    serde_json::to_value(s.status)
                        .ok()
                        .and_then(|v| v.as_str().map(|x| x.eq_ignore_ascii_case(st)))
                        .unwrap_or(false)
                })
            })
            .collect();
        list.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        return Ok(Value::Array(list.into_iter().take(limit).map(colony_record).collect()));
    }
    call(app, &plan(name, args)?).await
}

/// Finishes a call whose request needs the world's state: an answer needs the open question.
/// The question a colony is waiting on, or why there is none.
async fn pending_question(app: &Shared, id: &str) -> Result<crate::cli::PendingQuestion, String> {
    if !plain_id(id) {
        return Err("`id` is not a valid id".into());
    }
    let question = call(
        app,
        &Plan {
            method: "GET",
            path: format!("/api/sessions/{id}/question"),
            body: None,
        },
    )
    .await?;
    if question.as_str().is_some() {
        return Err("no question is pending for this colony".into());
    }
    serde_json::from_value(question).map_err(|e| format!("the question is not in a shape chat can answer: {e}"))
}

async fn finish_answer(app: &Shared, id: &str, answer: &str, bound: Option<&str>) -> Result<Plan, String> {
    let pending = pending_question(app, id).await?;
    if bound.is_none_or(|b| b != pending.question_id) {
        return Err("the colony is asking a different question than the one this answer was approved for; ask again".into());
    }
    let resolved = crate::cli::resolve_answer(answer, &pending).map_err(|e| e.to_string())?;
    Ok(Plan {
        method: "POST",
        path: format!("/api/sessions/{id}/answer"),
        body: Some(resolved.answer_body(&pending.question_id)),
    })
}

async fn run_write(app: &Shared, name: &str, args: &Value, bound: Option<&str>) -> Result<Value, String> {
    let mut p = plan(name, args)?;
    if name == "answer_colony" {
        p = finish_answer(app, arg_id(args, "id")?, arg_str(args, "answer")?, bound).await?;
    }
    call(app, &p).await
}

// ---------------------------------------------------------------------------
// Previews
// ---------------------------------------------------------------------------

/// How far a call reaches.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Blast {
    pub colonies: usize,
    pub orgs: usize,
    pub repos: usize,
    pub note: String,
}

/// What an approval card shows before anything runs.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Preview {
    pub summary: String,
    /// Before → after, one row per change, when the API can plan it (`dry_run`).
    pub diff: Vec<Value>,
    pub dry_run: bool,
    pub blast: Blast,
    /// For an answer: the question the user was shown. The answer only runs against that question.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_id: Option<String>,
}

struct Target {
    repo: String,
    title: String,
    status: SessionStatus,
}

async fn target(app: &Shared, id: &str) -> Option<Target> {
    app.sessions.read().await.iter().find(|s| s.id == id).map(|s| Target {
        repo: s.repo.clone(),
        title: if s.issue_title.is_empty() {
            s.summary.clone().unwrap_or_default()
        } else {
            s.issue_title.clone()
        },
        status: s.status,
    })
}

fn quoted(title: &str) -> String {
    if title.is_empty() {
        String::new()
    } else {
        format!(" (“{}”)", crate::util::truncate(title, 60))
    }
}

/// The preview of a write: a sentence in plain words, the dry-run diff, the blast radius.
pub async fn preview(app: &Shared, name: &str, args: &Value) -> Result<Preview, String> {
    let p = plan(name, args)?;
    let id = args["id"].as_str().unwrap_or_default();
    let t = if id.is_empty() { None } else { target(app, id).await };
    let one = |t: &Option<Target>, note: &str| Blast {
        colonies: usize::from(t.is_some()),
        orgs: usize::from(t.is_some()),
        repos: usize::from(t.is_some()),
        note: note.into(),
    };
    let on = |t: &Option<Target>| t.as_ref().map(|t| format!(" on {}", t.repo)).unwrap_or_default();
    let label = |t: &Option<Target>| t.as_ref().map(|t| quoted(&t.title)).unwrap_or_default();
    let mut out = Preview::default();
    match name {
        "stop_colony" => {
            out.summary = format!(
                "Stop colony {id}{}{}. Its microVM goes away; the worktree is kept so it can be resumed.",
                label(&t),
                on(&t)
            );
            out.blast = one(&t, "One colony, stopped now.");
        }
        "resume_colony" => {
            out.summary = format!(
                "Resume colony {id}{}{}. It boots again on its kept worktree.",
                label(&t),
                on(&t)
            );
            out.blast = one(&t, "One colony, started again.");
        }
        "publish_colony" => {
            out.summary = format!("Open the pull request for colony {id}{}{}.", label(&t), on(&t));
            out.blast = one(&t, "Creates a pull request on GitHub.");
        }
        "answer_colony" => {
            let answer = arg_str(args, "answer")?;
            if answer.chars().count() > TEXT_LIMIT {
                return Err(format!("the answer is longer than {TEXT_LIMIT} characters; shorten it"));
            }
            // The answer is bound to the question on screen: if the colony asks something else by
            // the time it is approved, it does not run.
            let pending = pending_question(app, id).await?;
            let asked = pending
                .questions
                .first()
                .map(|q| crate::util::truncate(&q.question, 200).to_string())
                .unwrap_or_default();
            out.question_id = Some(pending.question_id);
            out.summary = format!(
                "Answer colony {id}{}{} with “{answer}”. It is asking: “{asked}”",
                label(&t),
                on(&t),
            );
            out.blast = one(&t, "The colony carries on with this answer.");
        }
        "move_to_front" | "move_to_back" => {
            let place = if name == "move_to_front" { "front" } else { "back" };
            out.summary = format!("Move colony {id}{}{} to the {place} of the start queue.", label(&t), on(&t));
            if t.as_ref().is_some_and(|t| t.status != SessionStatus::Queued) {
                out.summary.push_str(" It is not queued, so the server will refuse.");
            }
            out.blast = one(&t, "Changes who starts next.");
        }
        "set_priority" => {
            out.summary = format!("Set colony {id}{}{} to priority {}.", label(&t), on(&t), args["priority"]);
            out.blast = one(&t, "Changes who starts next.");
        }
        "launch_colony" => {
            let repo = arg_str(args, "repo")?;
            let task = args["task"].as_str().map(str::trim).filter(|t| !t.is_empty());
            if task.is_some_and(|t| t.chars().count() > TEXT_LIMIT) {
                return Err(format!("the task is longer than {TEXT_LIMIT} characters; shorten it"));
            }
            // Both an issue and a task can ride on one launch: the card says every part of it.
            let what = match (args["issue"].as_u64(), task) {
                (Some(n), Some(t)) => format!("issue #{n} with the extra instructions “{t}”"),
                (Some(n), None) => format!("issue #{n}"),
                (None, Some(t)) => format!("the task “{t}”"),
                (None, None) => return Err("name an `issue` or a `task`".into()),
            };
            out.summary = format!("Start a colony on {repo} for {what}.");
            out.blast = Blast {
                colonies: 1,
                orgs: 1,
                repos: 1,
                note: "Uses a slot and model spend.".into(),
            };
        }
        "run_loop_now" => {
            out.summary = format!("Run loop {id} once, now.");
            out.blast = Blast {
                colonies: 1,
                orgs: 0,
                repos: 1,
                note: "Starts one colony for the loop.".into(),
            };
        }
        "apply_update" => {
            out.summary = "Install the newer Colonizer release.".into();
            out.blast = Blast {
                colonies: 0,
                orgs: 0,
                repos: 0,
                note: "The mothership drains, installs and restarts.".into(),
            };
        }
        "switch_models" => switch_preview(app, name, args, &p, &mut out).await?,
        _ => return Err(format!("{name} needs no approval")),
    }
    Ok(out)
}

async fn switch_preview(app: &Shared, _name: &str, args: &Value, p: &Plan, out: &mut Preview) -> Result<(), String> {
    let roles = args["roles"].as_object().cloned().unwrap_or_default();
    let said: Vec<String> = roles
        .iter()
        .map(|(role, model)| {
            format!(
                "{} to {}",
                role.replace('_', " "),
                model.as_str().filter(|m| !m.is_empty()).unwrap_or("the default")
            )
        })
        .collect();
    let scope = if args["scope"] == "org" {
        format!("the {} org", args["org"].as_str().unwrap_or("?"))
    } else {
        "all orgs".to_string()
    };
    let running = args["apply"] == "running";
    out.summary = format!("Switch {} for {scope}.", said.join(" and "));
    let Some(dry) = dry_run_of(p) else { return Ok(()) };
    let planned = call(app, &dry).await?;
    out.dry_run = true;
    out.diff = planned["changes"].as_array().cloned().unwrap_or_default();
    let affected: Vec<String> = planned["affected"]
        .as_array()
        .map(|l| l.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let sessions = app.sessions.read().await;
    let mut repos = std::collections::BTreeSet::new();
    let mut orgs = std::collections::BTreeSet::new();
    for s in sessions.iter().filter(|s| affected.contains(&s.id)) {
        repos.insert(s.repo.clone());
        orgs.insert(s.repo.split('/').next().unwrap_or_default().to_string());
    }
    for c in &out.diff {
        if c["scope"] == "org"
            && let Some(t) = c["target"].as_str()
        {
            orgs.insert(t.to_string());
        }
    }
    out.blast = Blast {
        colonies: affected.len(),
        orgs: orgs.len(),
        repos: repos.len(),
        note: if running {
            format!("{} running colonies restart on the new models.", affected.len())
        } else {
            "Colonies started from now on; running ones are untouched.".into()
        },
    };
    if running && !affected.is_empty() {
        out.summary
            .push_str(&format!(" {} running colonies restart on it.", affected.len()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Approvals
// ---------------------------------------------------------------------------

/// A held write.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Approval {
    pub id: String,
    pub chat: String,
    /// The assistant message that proposed it.
    pub message: String,
    pub tool: String,
    pub args: Value,
    pub preview: Preview,
    /// `pending`, `running`, `approved`, `rejected` or `failed`.
    pub status: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    /// `approve`, `edit` or `reject`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// The arguments that actually ran, when an edit changed them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ran_with: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

/// What a message records of one tool call, for the card and for the model's next turn.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ToolNote {
    pub tool: String,
    pub kind: Option<Kind>,
    /// `ran`, `failed`, `refused`, `pending`, `approved`, `rejected`.
    pub status: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

/// The text a past turn's tool calls add to the history the model sees.
pub fn trailer(notes: &[ToolNote]) -> String {
    notes
        .iter()
        .map(|n| {
            let result = n
                .result
                .as_deref()
                .map(|r| format!(" Result: {}", crate::util::truncate(r, 600)))
                .unwrap_or_default();
            format!("[tool {} — {}: {}.{result}]", n.tool, n.status, n.summary)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a held write has waited past its time.
fn stale(a: &Approval, now: chrono::DateTime<Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(&a.created_at)
        .map(|t| (now - t.with_timezone(&Utc)).num_seconds() > APPROVAL_TTL_SECS)
        .unwrap_or(true)
}

fn store_path(app: &crate::App) -> std::path::PathBuf {
    app.cfg.data_dir.join("chats").join("_approvals.json")
}

/// One writer at a time over the approvals file.
static STORE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn load(app: &crate::App) -> Vec<Approval> {
    std::fs::read(store_path(app))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

async fn save(app: &crate::App, list: &[Approval]) -> Result<(), String> {
    if let Some(dir) = store_path(app).parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("could not make the chats directory: {e}"))?;
    }
    let start = list.len().saturating_sub(KEEP);
    let text = serde_json::to_string_pretty(&list[start..]).map_err(|e| e.to_string())?;
    crate::util::write_atomic(&store_path(app), text.as_bytes())
        .await
        .map_err(|e| format!("could not save the approval: {e:#}"))
}

/// One tool call's outcome inside a round.
pub struct Done {
    pub id: String,
    /// What goes back to the model as the tool's result.
    pub content: String,
    pub is_error: bool,
    pub note: ToolNote,
    pub approval: Option<Approval>,
}

/// One tool call the model made.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// Runs one model-requested call: a read runs, a write is held, a refused call says why.
pub async fn run_call(app: &Shared, chat: &str, message: &str, workspace: Option<&str>, call: &ToolCall) -> Done {
    let finish = |content: String, is_error: bool, note: ToolNote, approval: Option<Approval>| Done {
        id: call.id.clone(),
        content,
        is_error,
        note,
        approval,
    };
    let summary_of = |text: &str| crate::util::truncate(text, 200);
    if let Some(why) = refusal(app, &call.name, &call.input).await {
        let note = ToolNote {
            tool: call.name.clone(),
            kind: kind_of(&call.name),
            status: "refused".into(),
            summary: summary_of(&why),
            ..ToolNote::default()
        };
        return finish(why, true, note, None);
    }
    match kind_of(&call.name) {
        Some(Kind::Read) => match run_read(app, &call.name, &call.input, workspace).await {
            Ok(value) => {
                let text = scrub(&value);
                let note = ToolNote {
                    tool: call.name.clone(),
                    kind: Some(Kind::Read),
                    status: "ran".into(),
                    summary: describe_read(&call.name, &call.input),
                    result: Some(crate::util::truncate(&text, 600)),
                    ..ToolNote::default()
                };
                finish(text, false, note, None)
            }
            Err(e) => {
                let note = ToolNote {
                    tool: call.name.clone(),
                    kind: Some(Kind::Read),
                    status: "failed".into(),
                    summary: summary_of(&e),
                    ..ToolNote::default()
                };
                finish(e, true, note, None)
            }
        },
        Some(Kind::Write) => match propose(app, chat, message, call).await {
            Ok(approval) => {
                let note = ToolNote {
                    tool: call.name.clone(),
                    kind: Some(Kind::Write),
                    status: "pending".into(),
                    summary: approval.preview.summary.clone(),
                    approval: Some(approval.id.clone()),
                    ..ToolNote::default()
                };
                let content = format!(
                    "Held for the user's approval (approval {}). Nothing has run. Tell the user what it will do and wait.",
                    approval.id
                );
                finish(content, false, note, Some(approval))
            }
            Err(e) => {
                let note = ToolNote {
                    tool: call.name.clone(),
                    kind: Some(Kind::Write),
                    status: "refused".into(),
                    summary: summary_of(&e),
                    ..ToolNote::default()
                };
                finish(e, true, note, None)
            }
        },
        None => unreachable!("refusal() catches names that are not tools"),
    }
}

fn describe_read(name: &str, args: &Value) -> String {
    let id = args["id"].as_str().unwrap_or_default();
    match name {
        "list_colonies" => "Listed colonies".into(),
        "colony_status" => format!("Looked at colony {id}"),
        "colony_question" => format!("Read colony {id}'s question"),
        "list_issues" => format!("Listed issues of {}", args["repo"].as_str().unwrap_or("a repository")),
        "list_loops" => "Listed loops".into(),
        "list_providers" => "Listed model providers".into(),
        "model_assignments" => "Read the model assignments".into(),
        "list_orgs" => "Listed orgs".into(),
        "recent_activity" => "Read the activity log".into(),
        _ => name.replace('_', " "),
    }
}

async fn propose(app: &Shared, chat: &str, message: &str, call: &ToolCall) -> Result<Approval, String> {
    // The strict shape check first, so the card never shows arguments that could not run.
    plan(&call.name, &call.input)?;
    let preview = preview(app, &call.name, &call.input).await?;
    let _guard = STORE.lock().await;
    let mut list = load(app);
    let now = Utc::now();
    if list.iter().filter(|a| a.status == "pending" && !stale(a, now)).count() >= PENDING_LIMIT {
        return Err("Too many approvals are waiting. Decide some before asking for more.".into());
    }
    let approval = Approval {
        id: short_id(),
        chat: chat.to_string(),
        message: message.to_string(),
        tool: call.name.clone(),
        args: call.input.clone(),
        preview,
        status: "pending".into(),
        created_at: Utc::now().to_rfc3339(),
        ..Approval::default()
    };
    list.push(approval.clone());
    save(app, &list).await?;
    Ok(approval)
}

// ---------------------------------------------------------------------------
// The stream's tool_use blocks
// ---------------------------------------------------------------------------

/// Collects the `tool_use` blocks of one Anthropic streamed reply.
#[derive(Default)]
pub struct ToolUses {
    open: HashMap<u64, (String, String, String)>,
    pub done: Vec<ToolCall>,
    pub stop_reason: Option<String>,
}

impl ToolUses {
    /// Reads one streaming event's `data`.
    pub fn feed(&mut self, data: &str) {
        let Ok(v) = serde_json::from_str::<Value>(data) else { return };
        let index = v["index"].as_u64().unwrap_or(0);
        match v["type"].as_str() {
            Some("content_block_start") if v["content_block"]["type"] == "tool_use" => {
                let block = &v["content_block"];
                self.open.insert(
                    index,
                    (
                        block["id"].as_str().unwrap_or_default().to_string(),
                        block["name"].as_str().unwrap_or_default().to_string(),
                        String::new(),
                    ),
                );
            }
            Some("content_block_delta") if v["delta"]["type"] == "input_json_delta" => {
                if let Some(open) = self.open.get_mut(&index) {
                    open.2.push_str(v["delta"]["partial_json"].as_str().unwrap_or_default());
                }
            }
            Some("content_block_stop") => {
                if let Some((id, name, json_text)) = self.open.remove(&index) {
                    let input = if json_text.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&json_text).unwrap_or(Value::Null)
                    };
                    self.done.push(ToolCall { id, name, input });
                }
            }
            Some("message_delta") => {
                if let Some(reason) = v["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.to_string());
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// The approvals API
// ---------------------------------------------------------------------------

/// `GET /api/chat/approvals?chat=<id>`: a conversation's approvals, newest last.
pub async fn list(
    State(app): State<Shared>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> Json<Value> {
    let chat = q.get("chat");
    let list: Vec<Approval> = load(&app).into_iter().filter(|a| chat.is_none_or(|c| &a.chat == c)).collect();
    Json(json!({ "approvals": list }))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Proposal {
    pub tool: String,
    pub args: Value,
    /// The conversation proposing it, or none for Spotlight's own actions.
    pub chat: Option<String>,
}

/// `POST /api/chat/approvals`: holds a write the cockpit itself proposes (a Spotlight action), with
/// the same preview, guardrails and approval a model's call gets. A read is refused here: it needs
/// no approval and has its own routes.
pub async fn propose_http(State(app): State<Shared>, Json(req): Json<Proposal>) -> ApiResult<Approval> {
    match kind_of(&req.tool) {
        Some(Kind::Write) => {}
        Some(Kind::Read) => return Err(client_error(StatusCode::BAD_REQUEST, "a read needs no approval")),
        None => {
            return Err(client_error(
                StatusCode::BAD_REQUEST,
                &format!("there is no tool called {}", req.tool),
            ));
        }
    }
    let args = if req.args.is_null() { json!({}) } else { req.args.clone() };
    if let Some(why) = refusal(&app, &req.tool, &args).await {
        return Err(client_error(StatusCode::FORBIDDEN, &why));
    }
    let chat = req.chat.clone().unwrap_or_default();
    let call = ToolCall {
        id: String::new(),
        name: req.tool.clone(),
        input: args,
    };
    propose(&app, &chat, if chat.is_empty() { "spotlight" } else { "cockpit" }, &call)
        .await
        .map(Json)
        .map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))
}

/// `GET /api/chat/approvals/{id}`.
pub async fn get(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Approval> {
    load(&app)
        .into_iter()
        .find(|a| a.id == id)
        .map(Json)
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such approval"))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Decision {
    /// `approve`, `edit` or `reject`.
    pub decision: String,
    /// With `edit`: the arguments to run instead.
    pub args: Option<Value>,
    pub reason: Option<String>,
}

async fn log_decision(app: &Shared, a: &Approval, kind: &str, detail: String) {
    let mut entry = crate::activity::Entry::new(kind, "you");
    entry.via = Some("cockpit".into());
    entry.target = Some(a.tool.clone());
    entry.section = Some("chat".into());
    // What ran is what is logged: an edit's arguments, not the ones first proposed.
    let ran = a.ran_with.as_ref().unwrap_or(&a.args);
    entry.colony = ran["id"]
        .as_str()
        .filter(|_| a.tool.ends_with("_colony") || a.tool.starts_with("move_") || a.tool == "set_priority")
        .map(str::to_string);
    entry.repo = ran["repo"].as_str().map(str::to_string);
    let edited = a
        .ran_with
        .as_ref()
        .map(|r| format!(" [ran with {}]", crate::util::truncate(&r.to_string(), 300)))
        .unwrap_or_default();
    entry.detail = Some(if a.chat.is_empty() {
        format!("{detail}{edited} — from Spotlight")
    } else {
        format!("{detail}{edited} — chat {}, message {}", a.chat, a.message)
    });
    crate::activity::record(app, entry).await;
}

/// `POST /api/chat/approvals/{id}`: approve, edit or reject a held write. The first decision claims
/// the approval; a second is a 409, so a call runs at most once.
pub async fn decide(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<Decision>) -> ApiResult<Value> {
    if !matches!(req.decision.as_str(), "approve" | "edit" | "reject") {
        return Err(client_error(StatusCode::BAD_REQUEST, "decision is approve, edit or reject"));
    }
    // Claim under the lock: pending → rejected, or pending → running.
    let claimed = {
        let _guard = STORE.lock().await;
        let mut list = load(&app);
        let Some(at) = list.iter().position(|a| a.id == id) else {
            return Err(client_error(StatusCode::NOT_FOUND, "no such approval"));
        };
        if list[at].status != "pending" {
            return Err(client_error(
                StatusCode::CONFLICT,
                &format!("this approval is already {}", list[at].status),
            ));
        }
        let now = Utc::now();
        if req.decision != "reject" && stale(&list[at], now) {
            // Settled, not left pending: it can never run, and it stops counting against the limit.
            list[at].status = "rejected".into();
            list[at].decision = Some("reject".into());
            list[at].decided_at = Some(now.to_rfc3339());
            list[at].result = Some("Expired: nobody decided it in time. Ask again.".into());
            let _ = save(&app, &list).await;
            return Err(client_error(
                StatusCode::GONE,
                "this approval waited too long and has expired; ask for the change again",
            ));
        }
        if req.decision != "reject" {
            let recent = list
                .iter()
                .filter(|a| {
                    a.decision.as_deref() != Some("reject")
                        && a.decided_at.as_deref().is_some_and(|t| {
                            chrono::DateTime::parse_from_rfc3339(t)
                                .is_ok_and(|t| (now - t.with_timezone(&Utc)).num_seconds() < 60)
                        })
                })
                .count();
            if recent >= WRITES_PER_MINUTE {
                return Err(client_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "too many changes in the last minute; wait a moment",
                ));
            }
        }
        let mut args = list[at].args.clone();
        if req.decision == "edit" {
            let Some(edited) = req.args.clone() else {
                return Err(client_error(StatusCode::BAD_REQUEST, "an edit carries the new arguments"));
            };
            plan(&list[at].tool, &edited).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?;
            args = edited;
        }
        if req.decision != "reject" {
            // State moves between the proposal and the click: the guardrails run again.
            if let Some(why) = refusal(&app, &list[at].tool, &args).await {
                return Err(client_error(StatusCode::FORBIDDEN, &why));
            }
        }
        list[at].decision = Some(req.decision.clone());
        list[at].decided_at = Some(now.to_rfc3339());
        list[at].status = if req.decision == "reject" {
            "rejected".into()
        } else {
            "running".into()
        };
        if req.decision == "reject" {
            list[at].result = req.reason.clone().map(|r| format!("Rejected: {r}"));
        } else if args != list[at].args {
            list[at].ran_with = Some(args.clone());
        }
        save(&app, &list)
            .await
            .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
        (list[at].clone(), args)
    };
    let (mut approval, args) = claimed;

    if req.decision == "reject" {
        log_decision(
            &app,
            &approval,
            "chat.reject",
            format!("rejected: {}", approval.preview.summary),
        )
        .await;
        settle_note(&app, &approval, "rejected", approval.result.clone()).await;
        return Ok(Json(json!({"approval": approval})));
    }

    let outcome = run_write(&app, &approval.tool, &args, approval.preview.question_id.as_deref()).await;
    let (status, result) = match &outcome {
        Ok(v) => ("approved", scrub(v)),
        Err(e) => ("failed", e.clone()),
    };
    {
        let _guard = STORE.lock().await;
        let mut list = load(&app);
        if let Some(a) = list.iter_mut().find(|a| a.id == id) {
            a.status = status.into();
            a.result = Some(result.clone());
            approval = a.clone();
        }
        let _ = save(&app, &list).await;
    }
    let edited = approval.ran_with.is_some();
    let verb = format!(
        "approved{}{}",
        if edited { " with edits" } else { "" },
        if status == "failed" { " but failed" } else { "" }
    );
    log_decision(
        &app,
        &approval,
        "chat.approve",
        format!("{verb}: {}", approval.preview.summary),
    )
    .await;
    settle_note(&app, &approval, status, Some(result.clone())).await;
    Ok(Json(json!({"approval": approval, "result": result, "ok": outcome.is_ok()})))
}

/// Records the decision on the message that proposed the call, so the thread shows what ran and the
/// model's next turn reads it.
async fn settle_note(app: &Shared, a: &Approval, status: &str, result: Option<String>) {
    if a.chat.is_empty() || !crate::chat::valid_id(&a.chat) {
        return;
    }
    let mut messages = crate::chat::read_messages(app, &a.chat).await;
    let mut changed = false;
    for m in messages.iter_mut().filter(|m| m.id == a.message) {
        for n in m.tools.iter_mut().filter(|n| n.approval.as_deref() == Some(a.id.as_str())) {
            n.status = status.to_string();
            n.result = result.clone().map(|r| crate::util::truncate(&r, 600));
            changed = true;
        }
    }
    if changed {
        let _ = crate::chat::rewrite_messages(app, &a.chat, &messages).await;
    }
}

pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/chat/approvals", routing::get(list).post(propose_http))
        .route("/api/chat/approvals/{id}", routing::get(get).post(decide))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tools() -> Vec<&'static str> {
        SPECS.iter().filter(|s| s.kind == Kind::Write).map(|s| s.name).collect()
    }

    #[test]
    fn every_tool_is_classified_and_has_a_plan() {
        for s in SPECS {
            let mut args = serde_json::Map::new();
            for (name, ty, _) in s.args {
                if s.required.contains(name) {
                    args.insert(
                        (*name).into(),
                        match *ty {
                            "integer" => json!(3),
                            "object" => json!({"model": "zai/glm-5.3"}),
                            _ if *name == "repo" => json!("o/r"),
                            _ if *name == "scope" => json!("install"),
                            _ => json!("abc"),
                        },
                    );
                }
            }
            if s.name == "launch_colony" {
                args.insert("issue".into(), json!(4));
            }
            let p = plan(s.name, &Value::Object(args)).unwrap_or_else(|e| panic!("{}: {e}", s.name));
            // A read never POSTs and a write never GETs: the class is the method.
            assert_eq!(p.method == "GET", s.kind == Kind::Read, "{}", s.name);
        }
        assert!(write_tools().contains(&"switch_models"));
        assert_eq!(kind_of("stop_colony"), Some(Kind::Write));
        assert_eq!(kind_of("list_colonies"), Some(Kind::Read));
        assert_eq!(kind_of("release_security_hold"), None);
    }

    #[test]
    fn plans_are_strict_and_paths_cannot_escape() {
        assert!(plan("stop_colony", &json!({"id": "../x"})).is_err());
        assert!(plan("stop_colony", &json!({"id": "a/b"})).is_err());
        assert!(
            plan("stop_colony", &json!({"id": "abc", "force": true})).is_err(),
            "unknown arguments are refused"
        );
        assert!(plan("launch_colony", &json!({"repo": "o/r"})).is_err(), "an issue or a task");
        assert!(
            plan("switch_models", &json!({"scope": "org", "roles": {"model": "a/b"}})).is_err(),
            "org scope names the org"
        );
        assert_eq!(
            plan("stop_colony", &json!({"id": "abc"})).unwrap().path,
            "/api/sessions/abc/stop"
        );
    }

    #[test]
    fn the_dry_run_is_the_same_call_with_the_flag() {
        let args = json!({"scope": "install", "roles": {"subagent_model": "byteplus/glm-5.1"}, "apply": "running"});
        let real = plan("switch_models", &args).unwrap();
        let dry = dry_run_of(&real).expect("the switch has a dry run");
        let mut stripped = dry.body.clone().unwrap();
        assert_eq!(stripped["dry_run"], json!(true));
        stripped.as_object_mut().unwrap().remove("dry_run");
        assert_eq!(Some(stripped), real.body, "what is previewed is what runs");
        assert_eq!((dry.method, dry.path.as_str()), (real.method, real.path.as_str()));
        assert!(dry_run_of(&plan("stop_colony", &json!({"id": "abc"})).unwrap()).is_none());
    }

    #[test]
    fn secrets_are_named_and_stripped() {
        assert!(names_a_secret(&json!({"roles": {"model": "a/b"}, "api_key": "x"})));
        assert!(names_a_secret(&json!({"a": [{"Token": "x"}]})));
        assert!(!names_a_secret(&json!({"id": "abc", "answer": "yes"})));
        let shown = scrub(
            &json!({"providers": [{"id": "z", "api_key": "sk-ant-api03-abcdefghijklmnopqrstuvwxyz", "base_url": "https://x"}]}),
        );
        assert!(!shown.contains("sk-ant") && !shown.contains("api_key"), "{shown}");
        assert!(shown.contains("base_url"));
    }

    #[test]
    fn the_stream_yields_tool_calls() {
        let mut uses = ToolUses::default();
        uses.feed(r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"stop_colony","input":{}}}"#);
        uses.feed(r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"id\":"}}"#);
        uses.feed(r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"abc\"}"}}"#);
        uses.feed(r#"{"type":"content_block_stop","index":1}"#);
        uses.feed(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#);
        assert_eq!(uses.done.len(), 1);
        assert_eq!(uses.done[0].name, "stop_colony");
        assert_eq!(uses.done[0].input, json!({"id": "abc"}));
        assert_eq!(uses.stop_reason.as_deref(), Some("tool_use"));
    }

    fn call_of(name: &str, input: Value) -> ToolCall {
        ToolCall {
            id: "toolu_x".into(),
            name: name.into(),
            input,
        }
    }

    fn temp(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("colonizer-chat-tools-{label}-{}", short_id()))
    }

    #[tokio::test]
    async fn a_write_is_held_and_never_runs_without_approval() {
        let root = temp("held");
        let app = crate::tests::test_app(&root);
        let done = run_call(
            &app,
            "chat0001",
            "msg00001",
            None,
            &call_of("stop_colony", json!({"id": "nope"})),
        )
        .await;
        let approval = done.approval.expect("a write becomes an approval");
        assert_eq!(approval.status, "pending");
        assert_eq!(done.note.status, "pending");
        assert!(approval.preview.summary.contains("Stop colony nope"));
        assert_eq!(load(&app)[0].status, "pending", "stored, and nothing has run");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn secret_and_security_hold_tools_are_refused() {
        let root = temp("refused");
        let app = crate::tests::test_app(&root);
        for name in ["read_secret", "set_api_key", "vault_unseal"] {
            let done = run_call(&app, "c", "m", None, &call_of(name, json!({}))).await;
            assert!(done.approval.is_none() && done.is_error, "{name}");
            assert!(done.content.contains("Secrets"), "{name}: {}", done.content);
        }
        let done = run_call(&app, "c", "m", None, &call_of("release_security_hold", json!({"id": "abc"}))).await;
        assert!(done.content.contains("security hold"), "{}", done.content);
        // A real tool carrying a secret argument is refused too.
        let done = run_call(
            &app,
            "c",
            "m",
            None,
            &call_of(
                "switch_models",
                json!({"scope": "install", "roles": {"model": "a/b"}, "api_key": "x"}),
            ),
        )
        .await;
        assert_eq!(done.note.status, "refused");
        assert!(load(&app).is_empty(), "a refused call leaves no approval");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_decision_settles_an_approval_exactly_once() {
        let root = temp("once");
        let app = crate::tests::test_app(&root);
        let done = run_call(
            &app,
            "chat0001",
            "msg00001",
            None,
            &call_of("stop_colony", json!({"id": "nope"})),
        )
        .await;
        let id = done.approval.unwrap().id;
        let first = decide(
            State(app.clone()),
            Path(id.clone()),
            Json(Decision {
                decision: "approve".into(),
                ..Decision::default()
            }),
        )
        .await
        .expect("the first decision settles it");
        // The colony does not exist, so the call ran and failed: it still counts as run, once.
        assert_eq!(first.0["approval"]["status"], "failed");
        let second = decide(
            State(app.clone()),
            Path(id.clone()),
            Json(Decision {
                decision: "approve".into(),
                ..Decision::default()
            }),
        )
        .await;
        assert_eq!(second.err().map(|e| e.0), Some(StatusCode::CONFLICT));
        let reject = decide(
            State(app.clone()),
            Path(id),
            Json(Decision {
                decision: "reject".into(),
                ..Decision::default()
            }),
        )
        .await;
        assert_eq!(
            reject.err().map(|e| e.0),
            Some(StatusCode::CONFLICT),
            "a settled approval cannot be reversed"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn reject_records_the_refusal_and_runs_nothing() {
        let root = temp("reject");
        let app = crate::tests::test_app(&root);
        let done = run_call(
            &app,
            "chat0001",
            "msg00001",
            None,
            &call_of("stop_colony", json!({"id": "nope"})),
        )
        .await;
        let id = done.approval.unwrap().id;
        let out = decide(
            State(app.clone()),
            Path(id.clone()),
            Json(Decision {
                decision: "reject".into(),
                reason: Some("not now".into()),
                ..Decision::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(out.0["approval"]["status"], "rejected");
        let stored = load(&app).into_iter().find(|a| a.id == id).unwrap();
        assert_eq!(stored.decision.as_deref(), Some("reject"));
        assert!(stored.ran_with.is_none());
        let (entries, _) = crate::activity::read_all(&app.cfg.data_dir);
        let line = entries
            .iter()
            .find(|e| e.kind == "chat.reject")
            .expect("the refusal is logged");
        assert_eq!(line.target.as_deref(), Some("stop_colony"));
        assert!(line.detail.as_deref().unwrap_or_default().contains("msg00001"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_edit_changes_the_arguments_before_they_run() {
        let root = temp("edit");
        let app = crate::tests::test_app(&root);
        let done = run_call(
            &app,
            "chat0001",
            "msg00001",
            None,
            &call_of("stop_colony", json!({"id": "first"})),
        )
        .await;
        let id = done.approval.unwrap().id;
        let bad = decide(
            State(app.clone()),
            Path(id.clone()),
            Json(Decision {
                decision: "edit".into(),
                args: Some(json!({"id": "../x"})),
                ..Decision::default()
            }),
        )
        .await;
        assert_eq!(
            bad.err().map(|e| e.0),
            Some(StatusCode::BAD_REQUEST),
            "an edit is validated like a proposal"
        );
        assert_eq!(load(&app)[0].status, "pending", "a refused edit leaves it pending");
        let out = decide(
            State(app.clone()),
            Path(id.clone()),
            Json(Decision {
                decision: "edit".into(),
                args: Some(json!({"id": "second"})),
                ..Decision::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(out.0["approval"]["ran_with"], json!({"id": "second"}));
        assert!(
            out.0["result"].as_str().unwrap().contains("404") || out.0["result"].as_str().unwrap().contains("no such"),
            "{}",
            out.0["result"]
        );
        let (entries, _) = crate::activity::read_all(&app.cfg.data_dir);
        assert!(
            entries
                .iter()
                .any(|e| e.kind == "chat.approve" && e.detail.as_deref().unwrap_or_default().contains("edits"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_read_runs_at_once_and_the_listing_leaves_no_approval() {
        let root = temp("read");
        let app = crate::tests::test_app(&root);
        let done = run_call(&app, "c", "m", None, &call_of("list_colonies", json!({}))).await;
        assert!(!done.is_error && done.approval.is_none());
        assert_eq!(done.note.status, "ran");
        assert_eq!(done.content, "[]");
        assert!(load(&app).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_security_hold_and_a_hidden_org_are_never_touched() {
        let root = temp("hold");
        let app = crate::tests::test_app(&root);
        {
            let mut sessions = app.sessions.write().await;
            sessions.push(crate::sessions::Session {
                id: "held1".into(),
                repo: "acme/web".into(),
                org: "acme".into(),
                status: SessionStatus::Stopped,
                attention: Some(json!({"reason": "control_defeat"})),
                ..crate::sessions::Session::default()
            });
            sessions.push(crate::sessions::Session {
                id: "off1".into(),
                repo: "secret-org/web".into(),
                org: "secret-org".into(),
                status: SessionStatus::Running,
                ..crate::sessions::Session::default()
            });
        }
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config/orgs.json"), r#"{"secret-org":{"enabled":false}}"#).unwrap();
        for tool in ["resume_colony", "publish_colony"] {
            let done = run_call(&app, "c", "m", None, &call_of(tool, json!({"id": "held1"}))).await;
            assert!(
                done.approval.is_none() && done.content.contains("security hold"),
                "{tool}: {}",
                done.content
            );
        }
        // Stopping a held colony is the cautious direction and stays possible.
        let stop = run_call(&app, "c", "m", None, &call_of("stop_colony", json!({"id": "held1"}))).await;
        assert!(stop.approval.is_some());
        let done = run_call(&app, "c", "m", None, &call_of("stop_colony", json!({"id": "off1"}))).await;
        assert!(
            done.approval.is_none() && done.content.contains("switched off"),
            "{}",
            done.content
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn spotlight_proposes_a_write_and_the_decision_is_logged() {
        let root = temp("spot");
        let app = crate::tests::test_app(&root);
        let held = propose_http(
            State(app.clone()),
            Json(Proposal {
                tool: "move_to_front".into(),
                args: json!({"id": "q1"}),
                chat: None,
            }),
        )
        .await
        .expect("held, not run");
        assert_eq!(held.0.status, "pending");
        assert!(held.0.preview.summary.contains("front of the start queue"));
        let read = propose_http(
            State(app.clone()),
            Json(Proposal {
                tool: "list_colonies".into(),
                ..Proposal::default()
            }),
        )
        .await;
        assert_eq!(read.err().map(|e| e.0), Some(StatusCode::BAD_REQUEST));
        let secret = propose_http(
            State(app.clone()),
            Json(Proposal {
                tool: "stop_colony".into(),
                args: json!({"id": "a", "token": "x"}),
                chat: None,
            }),
        )
        .await;
        assert_eq!(secret.err().map(|e| e.0), Some(StatusCode::FORBIDDEN));
        let _ = decide(
            State(app.clone()),
            Path(held.0.id.clone()),
            Json(Decision {
                decision: "reject".into(),
                ..Decision::default()
            }),
        )
        .await
        .unwrap();
        let (entries, _) = crate::activity::read_all(&app.cfg.data_dir);
        let line = entries.iter().find(|e| e.kind == "chat.reject").unwrap();
        assert!(line.detail.as_deref().unwrap_or_default().contains("Spotlight"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_running_switch_never_restarts_a_held_colony() {
        let root = temp("switchhold");
        let app = crate::tests::test_app(&root);
        app.sessions.write().await.push(crate::sessions::Session {
            id: "held1".into(),
            repo: "acme/web".into(),
            org: "acme".into(),
            status: SessionStatus::Running,
            attention: Some(json!({"reason": "control_defeat"})),
            ..crate::sessions::Session::default()
        });
        let running = json!({"scope": "install", "roles": {"model": "a/b"}, "apply": "running"});
        let done = run_call(&app, "c", "m", None, &call_of("switch_models", running.clone())).await;
        assert!(
            done.approval.is_none() && done.content.contains("security hold"),
            "{}",
            done.content
        );
        // Scoped to another org it does not reach the held colony; and "new" restarts nothing.
        let other = json!({"scope": "org", "org": "other", "roles": {"model": "a/b"}, "apply": "running"});
        assert!(refusal(&app, "switch_models", &other).await.is_none());
        let new_only = json!({"scope": "install", "roles": {"model": "a/b"}, "apply": "new"});
        assert!(refusal(&app, "switch_models", &new_only).await.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_card_says_everything_that_will_run() {
        let root = temp("card");
        let app = crate::tests::test_app(&root);
        let both = preview(
            &app,
            "launch_colony",
            &json!({"repo": "o/r", "issue": 4, "task": "also delete the tests"}),
        )
        .await
        .unwrap();
        assert!(
            both.summary.contains("#4") && both.summary.contains("also delete the tests"),
            "{}",
            both.summary
        );
        let long = "x".repeat(TEXT_LIMIT + 1);
        assert!(
            preview(&app, "launch_colony", &json!({"repo": "o/r", "task": long}))
                .await
                .is_err()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_stale_approval_expires_instead_of_running() {
        let root = temp("stale");
        let app = crate::tests::test_app(&root);
        let done = run_call(&app, "c", "m", None, &call_of("stop_colony", json!({"id": "nope"}))).await;
        let id = done.approval.unwrap().id;
        let mut list = load(&app);
        list[0].created_at = (Utc::now() - chrono::Duration::seconds(APPROVAL_TTL_SECS + 5)).to_rfc3339();
        save(&app, &list).await.unwrap();
        let out = decide(
            State(app.clone()),
            Path(id.clone()),
            Json(Decision {
                decision: "approve".into(),
                ..Decision::default()
            }),
        )
        .await;
        assert_eq!(out.err().map(|e| e.0), Some(StatusCode::GONE));
        let stored = load(&app).into_iter().find(|a| a.id == id).unwrap();
        assert_eq!(stored.status, "rejected", "it never ran and no longer holds a slot");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_trailer_tells_the_model_what_happened() {
        let notes = vec![ToolNote {
            tool: "stop_colony".into(),
            status: "approved".into(),
            summary: "Stop colony abc".into(),
            result: Some("stopped".into()),
            ..ToolNote::default()
        }];
        let text = trailer(&notes);
        assert!(text.contains("approved") && text.contains("Result: stopped"), "{text}");
    }
}
