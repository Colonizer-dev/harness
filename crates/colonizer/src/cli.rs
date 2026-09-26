//! The `colonizer` command line, built on clap: the commands that run against this machine
//! (`version`, `update`, `open`, `login-item`, `telemetry`), and the client commands that drive a
//! mothership already running somewhere — here or across a tailnet (`launch`, `list`, `status`,
//! `logs`, `diff`, `ask`, `answer`, `stop`, `resume`, `pr`, `map`, `token`, `mcp`).
//!
//! With no subcommand at all the binary starts the mothership, exactly as it always has.
//!
//! Exit codes are part of the interface, so scripts can tell a typo from a refusal: [`EXIT_ERROR`]
//! and friends are defined once here, shown in `--help`, and returned from [`run`].

use crate::{Settings, auth, util};
use anyhow::{Context as _, Result};
use clap::{ArgAction, CommandFactory as _, Parser, Subcommand, ValueEnum};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

/// Everything worked. Also the informational commands (`--help`, `--version`, `version`).
pub const EXIT_OK: i32 = 0;
/// The mothership could not be reached, it answered with an error no code below names, or the
/// command failed locally. The catch-all: every other code is one specific kind of refusal.
pub const EXIT_ERROR: i32 = 1;
/// The arguments name no command this build knows. Clap's own code for that, kept here so the set
/// is all in one place.
pub const EXIT_USAGE: i32 = 2;
/// The token does not open this door: 401 (missing or unknown credential) or 403 (a scoped token
/// whose scope, or org/repo limits, do not cover the request).
pub const EXIT_FORBIDDEN: i32 = 3;
/// The colony (or token) does not exist: 404 — except `ask`'s nothing-pending case, which is 5.
pub const EXIT_NOT_FOUND: i32 = 4;
/// 409: the colony is in the wrong state for that (`stop` mid-publish), or, for `ask`, it is not
/// asking anything.
pub const EXIT_CONFLICT: i32 = 5;
/// 429: a scoped token's launch cap — its concurrency limit or daily budget — refused the launch.
pub const EXIT_CAP: i32 = 6;

/// The port a `--host` that names no port of its own gets: the mothership's default bind.
const DEFAULT_PORT: u16 = 7878;

/// `colonizer`, as clap sees it. The global flags work before or after the subcommand, so both
/// `colonizer --json list` and `colonizer list --json` read the same.
#[derive(Parser, Debug)]
#[command(
    name = "colonizer",
    about = "turn a task into a pull request: coding agents in private microVMs, watched from a cockpit",
    version = crate::version::build().line(),
    after_help = AFTER_HELP,
)]
pub struct Cli {
    /// Which mothership the client commands talk to: a host name or host:port. The default is the
    /// local one (COLONIZER_BIND, else 127.0.0.1:7878); across a tailnet or tunnel, name it.
    #[arg(long, global = true, value_parser = parse_host, value_name = "HOST[:PORT]")]
    host: Option<String>,

    /// File holding the API token (the value itself, one line). The resolution order is
    /// COLONIZER_TOKEN, then this file, then the local <config_dir>/api-token.
    #[arg(long, global = true, value_name = "PATH")]
    token_file: Option<PathBuf>,

    /// Print machine-readable JSON instead of the human rendering, where a command has one.
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    json: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

const AFTER_HELP: &str = "With no subcommand at all, colonizer starts the mothership: it serves the web UI and the API
on COLONIZER_BIND (default 127.0.0.1:7878) and runs the colonies.

Exit codes:
  0  ok
  1  error: the mothership is unreachable, refused, or something else went wrong
  2  usage: the arguments name no command this build knows
  3  unauthorized or forbidden: the token is missing, unknown (401) or not allowed (403)
  4  not found: no such colony or token (404)
  5  conflict (409), or `ask` on a colony that is not asking anything
  6  a launch cap was refused (429)

Settings come from the environment, not flags: COLONIZER_BIND, COLONIZER_DATA_DIR,
COLONIZER_HOME and the rest are in docs/install.md. The mothership and the local commands
(`update`, `open`, `login-item`) read them; the client commands take --host and --token-file.";

#[derive(Subcommand, Debug)]
enum Command {
    /// Print what this build is, and whether it is a release (also `--version`)
    Version,
    /// Install the newest release against a running mothership and restart into it
    Update {
        /// Install over a development build, or one newer than the latest release
        #[arg(long)]
        force: bool,
    },
    /// Print the cockpit sign-in link and open it in a browser
    Open,
    /// Start the mothership at login (macOS LaunchAgent, Linux systemd user unit)
    LoginItem {
        /// enable, disable or status; disable never stops a running one
        #[arg(value_enum)]
        action: LoginItemAction,
    },
    /// Show or change anonymous usage reporting (no network, no daemon needed)
    Telemetry {
        /// show, on or off
        #[arg(value_enum)]
        action: TelemetryAction,
    },
    /// Print a shell completion script for this command (source it from your shell's rc)
    Completions {
        /// Which shell to emit for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print this command's man page to stdout
    Man,
    /// Start a colony: an agent in a microVM, working the repo, or one issue in it
    Launch {
        /// The repository to work in, as owner/repo
        #[arg(value_name = "OWNER/REPO")]
        repo: String,
        /// Work one issue instead of the repository's own backlog
        #[arg(long)]
        issue: Option<u64>,
        /// Run the orchestrator on this model instead of what routing would pick
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        /// Run the colony's subagents on this model
        #[arg(long, value_name = "MODEL")]
        subagent_model: Option<String>,
        /// Answer routine questions itself, without waiting for a person
        #[arg(long)]
        autopilot: bool,
        /// Keep autopilot off, even where the install default is on
        #[arg(long, conflicts_with = "autopilot")]
        no_autopilot: bool,
        /// The task, when the issue alone does not say it (the issue body is read either way)
        task: Option<String>,
    },
    /// List the colonies this token may see, newest first
    List {
        /// Only colonies of this org (the repository owner)
        #[arg(long)]
        org: Option<String>,
        /// Only colonies in this state: queued, running, waiting_for_answer, pr_opened, ...
        #[arg(long)]
        status: Option<String>,
    },
    /// Show one colony: where it stands, what it costs, and what it is doing right now
    Status { id: String },
    /// Print a colony's recent events, or follow them live
    Logs {
        id: String,
        /// Follow: stream events until the mothership closes the stream; Ctrl-C ends the follow
        #[arg(short = 'f', long)]
        follow: bool,
    },
    /// Print everything a colony has changed against its base branch, as a unified diff
    Diff {
        id: String,
        /// Print per-file +/- counts instead of the diff text
        #[arg(long)]
        stat: bool,
    },
    /// Print the question a colony is waiting on, with its options numbered
    Ask { id: String },
    /// Answer a colony's pending question: an option's number, its label, or free text
    ///
    /// With one question pending, `1` picks its first option, a label matches its options
    /// case-insensitively, and anything else goes to the agent as a free-text note. With several
    /// pending, only a number or label of the first question is accepted — `colonizer ask <id>`
    /// shows the rest.
    Answer { id: String, answer: String },
    /// Stop a colony: the microVM goes away, the worktree is kept for a later `resume`
    Stop { id: String },
    /// Start a stopped colony again, picking up its worktree where it was left
    Resume { id: String },
    /// Print a colony's pull request URL and state
    Pr { id: String },
    /// Print a repository's architecture map as a text outline, or search it with --find
    Map {
        /// The repository the map was drawn from, as owner/repo
        #[arg(value_name = "OWNER/REPO")]
        repo: String,
        /// Print only the components matching this query: a label, id, type or source path
        #[arg(long, value_name = "QUERY")]
        find: Option<String>,
    },
    /// Serve this harness to MCP clients over stdio (tools for listing, watching and driving
    /// colonies); the tool set follows the token's scope
    Mcp {
        /// Raise an owner token above read (operate or launch); never raises a scoped token
        #[arg(long, value_enum)]
        scope: Option<McpScope>,
    },
    /// Manage the mothership's scoped API tokens (the owner token only)
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LoginItemAction {
    Enable,
    Disable,
    Status,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TelemetryAction {
    Show,
    On,
    Off,
}

/// The scope `colonizer mcp --scope` asks for: a floor under an owner token's default `read`, and
/// at most a no-op for a scoped token, whose own scope is never raised.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum McpScope {
    Read,
    Operate,
    Launch,
}

impl McpScope {
    fn scope(self) -> crate::api_tokens::Scope {
        match self {
            Self::Read => crate::api_tokens::Scope::Read,
            Self::Operate => crate::api_tokens::Scope::Operate,
            Self::Launch => crate::api_tokens::Scope::Launch,
        }
    }
}

#[derive(Subcommand, Debug)]
enum TokenCommand {
    /// List every token's metadata. The plaintext is never here; it was shown once, at creation.
    List,
    /// Mint a token. The plaintext is printed once and cannot be shown again.
    Create {
        /// A name that says who uses it ("ci", "rachel's assistant")
        name: String,
        /// How much it may do: read (watch), operate (answer, stop, resume), launch (start colonies)
        #[arg(long, value_enum)]
        scope: TokenScope,
        /// Limit it to these orgs (repeat the flag); none listed means no limit
        #[arg(long)]
        org: Vec<String>,
        /// Limit it to these repositories, owner/repo (repeat the flag); none listed means no limit
        #[arg(long)]
        repo: Vec<String>,
        /// The most colonies it may keep unfinished at once
        #[arg(long)]
        max_concurrent: Option<u32>,
        /// The most model spend its colonies may run up per UTC day, in dollars
        #[arg(long)]
        budget_usd_per_day: Option<f64>,
    },
    /// Revoke a token. Presentations of it stop authenticating at once.
    Revoke { id: String },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TokenScope {
    Read,
    Operate,
    Launch,
}

impl TokenScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Operate => "operate",
            Self::Launch => "launch",
        }
    }
}

// ---------------------------------------------------------------------------
// The client half: a mothership already running somewhere.
// ---------------------------------------------------------------------------

/// A client of one mothership: where it answers and what proves us to it. Every client command
/// (and `colonizer mcp`) builds one of these from the same global flags.
pub(crate) struct Machine {
    base: String,
    pub(crate) token: String,
    http: reqwest::Client,
}

/// Why a client command failed, split so the exit code and the message can be decided apart: a
/// status code maps to its own exit code (the constants above), a transport failure is a plain 1.
#[derive(Debug)]
pub(crate) enum Fail {
    Server { status: u16, message: String },
    Transport(anyhow::Error),
}

impl From<anyhow::Error> for Fail {
    fn from(e: anyhow::Error) -> Self {
        Fail::Transport(e)
    }
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Server { message, .. } => f.write_str(message),
            Fail::Transport(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for Fail {}

impl Fail {
    /// The exit code a script should read this failure as.
    pub(crate) fn exit_code(&self) -> i32 {
        match self {
            Fail::Server { status, .. } => match *status {
                401 | 403 => EXIT_FORBIDDEN,
                404 => EXIT_NOT_FOUND,
                409 => EXIT_CONFLICT,
                429 => EXIT_CAP,
                _ => EXIT_ERROR,
            },
            Fail::Transport(_) => EXIT_ERROR,
        }
    }

    /// Reads the mothership's refusal out of its error body (`{"error": ...}`), falling back to
    /// the raw bytes so a proxy's plain-text answer still says something.
    fn message(status: u16, body: &str) -> Self {
        let message = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| util::truncate(body.trim(), 500));
        Fail::Server { status, message }
    }
}

impl Machine {
    /// Builds the client from the global flags, resolving the host and the token the way every
    /// client command does.
    pub(crate) fn from_cli(cli: &Cli) -> Result<Self> {
        let host = match &cli.host {
            // The flag's parser already normalized it to `host:port`.
            Some(host) => host.clone(),
            None => Settings::from_env()?.bind,
        };
        Ok(Self {
            base: format!("http://{host}"),
            token: resolve_token(cli)?,
            http: reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Builds the client straight at a base and token, for the `mcp` tests that stand a stub
    /// mothership up on a loopback port.
    #[cfg(test)]
    pub(crate) fn for_tests(base: String, token: String) -> Self {
        Self {
            base,
            token,
            http: reqwest::Client::new(),
        }
    }

    /// GET expecting a JSON body.
    pub(crate) async fn get(&self, path: &str) -> Result<Value, Fail> {
        let response = self
            .http
            .get(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| Fail::Transport(anyhow::anyhow!("no mothership answering on {}: {e}", self.base)))?;
        Self::body(response).await
    }

    /// GET whose 204 success answers with no body at all: the question route's way of saying the
    /// colony is not asking anything, which is an answer, not a failure.
    pub(crate) async fn get_optional(&self, path: &str) -> Result<Option<Value>, Fail> {
        let response = self
            .http
            .get(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| Fail::Transport(anyhow::anyhow!("no mothership answering on {}: {e}", self.base)))?;
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        Self::body(response).await.map(Some)
    }

    /// POST, with a body only when there is one to send; `Ok(None)` is the 204 the answer route
    /// answers — nothing to read there, and that is success.
    pub(crate) async fn post(&self, path: &str, body: Option<&Value>) -> Result<Option<Value>, Fail> {
        let mut request = self.http.post(self.url(path)).bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| Fail::Transport(anyhow::anyhow!("no mothership answering on {}: {e}", self.base)))?;
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        Self::body(response).await.map(Some)
    }

    /// DELETE expecting a JSON body.
    pub(crate) async fn delete(&self, path: &str) -> Result<Value, Fail> {
        let response = self
            .http
            .delete(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| Fail::Transport(anyhow::anyhow!("no mothership answering on {}: {e}", self.base)))?;
        Self::body(response).await
    }

    /// One response, either the JSON it answers with or the failure it refuses with.
    async fn body(response: reqwest::Response) -> Result<Value, Fail> {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.is_success() {
            return serde_json::from_str(&body)
                .with_context(|| {
                    format!(
                        "the mothership answered {status} with something but JSON: {}",
                        util::truncate(&body, 200)
                    )
                })
                .map_err(Fail::Transport);
        }
        Err(Fail::message(status.as_u16(), &body))
    }
}

/// `--host` as the flag stores it: always `host:port`, normalized at parse time. A bare host gets
/// the mothership's default port; a bare IPv6 literal is bracketed before the port can follow
/// (`::1` dials as `[::1]:7878`); a bracketed `[::1]:port` is kept as written. A URL is refused:
/// `--host` names where the mothership is, not how to speak to it, and the mothership serves plain
/// HTTP.
fn parse_host(host: &str) -> Result<String, String> {
    if host.contains("://") {
        return Err(format!(
            "--host takes a host or host:port, not a URL ({host}); drop the scheme — the mothership serves plain HTTP"
        ));
    }
    if host.starts_with('[') {
        if !host.contains(']') {
            return Err(format!("--host opens a bracket without closing it: {host}"));
        }
        return Ok(if host.ends_with(']') {
            format!("{host}:{DEFAULT_PORT}")
        } else {
            host.to_string()
        });
    }
    match host.matches(':').count() {
        0 => Ok(format!("{host}:{DEFAULT_PORT}")),
        // host:port
        1 => Ok(host.to_string()),
        // A bare IPv6 literal (`::1`, `fd7a::12`): bracket it, then the port.
        _ => Ok(format!("[{host}]:{DEFAULT_PORT}")),
    }
}

/// The token a client command proves itself with: the environment first, so a shell (or a CI job)
/// can hold it without touching disk; then the file the operator named; then the local install's
/// own token, read without creating it — the CLI is a client here, not the mint (the mothership
/// writes that file on its first start).
fn resolve_token(cli: &Cli) -> Result<String> {
    if let Some(token) = std::env::var("COLONIZER_TOKEN").ok().map(|t| t.trim().to_string())
        && !token.is_empty()
    {
        return Ok(token);
    }
    if let Some(path) = &cli.token_file {
        return util::read_trimmed(path).filter(|t| !t.is_empty()).ok_or_else(|| {
            anyhow::anyhow!(
                "no token in {}: set COLONIZER_TOKEN or write the token into the file",
                path.display()
            )
        });
    }
    let cfg = Settings::from_env()?;
    let path = auth::token_file(&cfg.config_dir);
    util::read_trimmed(&path).filter(|t| !t.is_empty()).ok_or_else(|| {
        anyhow::anyhow!(
            "no API token: set COLONIZER_TOKEN or --token-file, or start `colonizer` once (it writes {})",
            path.display()
        )
    })
}

// ---------------------------------------------------------------------------
// The question an `answer` answers, and how an argument becomes one.
// ---------------------------------------------------------------------------

/// One question of a pending `ask_user`, as the agent asked it. `header` is its short form and may
/// be absent; the free-text "Other" the cockpit's card always offers is not on the wire — an
/// option's label is what the answers map is keyed by.
#[derive(Debug, Deserialize)]
pub(crate) struct QuestionBody {
    pub question: String,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// `GET /api/sessions/{id}/question`'s body: what the colony is asking and how risky an answer is.
#[derive(Debug, Deserialize)]
pub(crate) struct PendingQuestion {
    pub question_id: String,
    #[serde(default)]
    pub risk: String,
    #[serde(default)]
    pub questions: Vec<QuestionBody>,
}

/// What `answer` (CLI) and `answer_colony` (MCP) send once the argument is matched: the picked
/// option — the answers map wants the label, in a list when the question is multi-select — or the
/// free text, which becomes both the answer value and the note the agent reads.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    Choice {
        question: String,
        multi_select: bool,
        label: String,
    },
    FreeText {
        question: String,
        multi_select: bool,
        text: String,
    },
}

impl Resolved {
    /// The body `POST /api/sessions/{id}/answer` takes, mirroring how the cockpit's choice card
    /// builds the same command over the events socket: the answers map is keyed by the question's
    /// own text, a pick is the option's label (a one-element list when multi-select), the "Other"
    /// card's free text is the value itself, and `response` carries the note.
    pub(crate) fn answer_body(&self, question_id: &str) -> Value {
        let (question, multi, value, response) = match self {
            Resolved::Choice {
                question,
                multi_select,
                label,
            } => (question, *multi_select, json!(label), None),
            Resolved::FreeText {
                question,
                multi_select,
                text,
            } => (question, *multi_select, json!(text), Some(text.clone())),
        };
        let answers = json!({ question: if multi { json!([value]) } else { value } });
        json!({ "question_id": question_id, "answers": answers, "response": response })
    }
}

/// How `colonizer answer`'s argument failed to name an answer; every case says the way out.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MatchError {
    /// A bare number that names no option: read as a mis-typed option reference, never silently as
    /// free text — answering "12" to a question with 3 options is a typo, not a note.
    NoSuchOption { number: u64, options: usize },
    /// A label more than one option of the question answers to.
    AmbiguousLabel { label: String },
    /// Free text, but several questions are pending, so no one question is what it answers.
    FreeTextNeedsOneQuestion { questions: usize },
    /// The pending question carries no questions at all, so there is nothing to match against.
    NoQuestions,
}

impl std::fmt::Display for MatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MatchError::NoSuchOption { number, options } => {
                write!(
                    f,
                    "there is no option {number}; this question has {options} — see `colonizer ask <id>`"
                )
            }
            MatchError::AmbiguousLabel { label } => {
                write!(f, "\"{label}\" matches more than one option; use the option's number")
            }
            MatchError::FreeTextNeedsOneQuestion { questions } => write!(
                f,
                "{questions} questions are pending and free text would answer only one of them; answer the others by number or label, or in the cockpit"
            ),
            MatchError::NoQuestions => {
                write!(f, "the pending question carries no questions; answer it in the cockpit")
            }
        }
    }
}

/// Matches `colonizer answer <id> <arg>` against a pending question: a 1-based option number wins,
/// then a case-insensitive whole-label match, and anything else is the free-text note. Labels are
/// matched against the first question only — exactly one question's worth of guessing — and free
/// text is refused outright when more than one question is pending, because there is no telling
/// which one it answers. Pure, so the CLI and the MCP tool cannot drift.
pub(crate) fn resolve_answer(arg: &str, pending: &PendingQuestion) -> Result<Resolved, MatchError> {
    let Some(first) = pending.questions.first() else {
        // The question route answers 204 when nothing is pending, so a pending question with no
        // bodies should not happen; refusing beats sending an answer the runner would call
        // malformed.
        return Err(MatchError::NoQuestions);
    };
    let labels: Vec<&str> = first.options.iter().map(|o| o.label.trim()).collect();
    if let Ok(number) = arg.trim().parse::<u64>() {
        return match number.checked_sub(1).and_then(|i| labels.get(i as usize)) {
            Some(label) => Ok(choice(first, label)),
            None => Err(MatchError::NoSuchOption {
                number,
                options: labels.len(),
            }),
        };
    }
    let wanted = arg.trim();
    let matches: Vec<&&str> = labels.iter().filter(|l| l.eq_ignore_ascii_case(wanted)).collect();
    match matches.len() {
        1 => Ok(choice(first, matches[0])),
        0 if pending.questions.len() == 1 => Ok(Resolved::FreeText {
            question: first.question.clone(),
            multi_select: first.multi_select,
            text: wanted.to_string(),
        }),
        0 => Err(MatchError::FreeTextNeedsOneQuestion {
            questions: pending.questions.len(),
        }),
        _ => Err(MatchError::AmbiguousLabel {
            label: wanted.to_string(),
        }),
    }
}

fn choice(question: &QuestionBody, label: &str) -> Resolved {
    Resolved::Choice {
        question: question.question.clone(),
        multi_select: question.multi_select,
        label: label.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Running the commands.
// ---------------------------------------------------------------------------

/// Parses the command line the way clap does: usage errors exit [`EXIT_USAGE`], `--help` and
/// `--version` print and exit [`EXIT_OK`] — neither ever starts a mothership.
pub fn parse() -> Cli {
    Cli::try_parse().unwrap_or_else(|e| {
        debug_assert_eq!(e.exit_code(), if e.use_stderr() { EXIT_USAGE } else { EXIT_OK });
        let _ = e.print();
        std::process::exit(e.exit_code());
    })
}

/// Runs what the command line asked for and returns the exit code. Starting the mothership — no
/// subcommand — is the default, so `colonizer` alone keeps doing what it always did.
pub async fn run(mut cli: Cli) -> i32 {
    match cli.command.take() {
        None => await_local(crate::serve().await),
        Some(command) => dispatch(&cli, command).await,
    }
}

/// Local commands answer `Result` and map to ok/1; the client commands carry their own codes.
fn await_local(result: Result<()>) -> i32 {
    match result {
        Ok(()) => EXIT_OK,
        Err(e) => {
            eprintln!("colonizer: {e:#}");
            EXIT_ERROR
        }
    }
}

async fn dispatch(cli: &Cli, command: Command) -> i32 {
    match command {
        Command::Version => {
            // The stamped build, not CARGO_PKG_VERSION: the crate version says nothing about
            // which commit an install came from.
            println!("{}", crate::version::build().line());
            EXIT_OK
        }
        Command::Update { force } => await_local(crate::update::command(force).await),
        Command::Open => await_local(open()),
        Command::LoginItem { action } => {
            let cfg = match Settings::from_env() {
                Ok(cfg) => cfg,
                Err(e) => return await_local(Err(e)),
            };
            let action = action
                .to_possible_value()
                .map(|v| v.get_name().to_string())
                .unwrap_or_default();
            await_local(crate::login_item::command(&action, &cfg.data_dir))
        }
        Command::Telemetry { action } => {
            let cfg = match Settings::from_env() {
                Ok(cfg) => cfg,
                Err(e) => return await_local(Err(e)),
            };
            let result = match action {
                TelemetryAction::Show => crate::usage::cli_show(&cfg.config_dir),
                TelemetryAction::On => crate::usage::cli_set(&cfg.config_dir, true),
                TelemetryAction::Off => crate::usage::cli_set(&cfg.config_dir, false),
            };
            await_local(result)
        }
        Command::Completions { shell } => {
            // The generator writes straight through and panics on a failed write of its own, so
            // the script is buffered: a closed pipe (a `| head`) ends the copy, not the process.
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "colonizer", &mut script);
            let _ = std::io::stdout().write_all(&script);
            EXIT_OK
        }
        Command::Man => {
            // EPIPE (a `| head`) is not an error here; the page was read as far as it was read.
            let _ = clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout().lock());
            EXIT_OK
        }
        Command::Mcp { scope } => match Machine::from_cli(cli) {
            Ok(machine) => match crate::mcp::run(machine, scope.map(McpScope::scope)).await {
                Ok(()) => EXIT_OK,
                Err(e) => {
                    eprintln!("colonizer: {e:#}");
                    EXIT_ERROR
                }
            },
            Err(e) => {
                eprintln!("colonizer: {e:#}");
                EXIT_ERROR
            }
        },
        Command::Launch {
            repo,
            issue,
            model,
            subagent_model,
            autopilot,
            no_autopilot,
            task,
        } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                let body = json!({
                    "repo": repo,
                    "issue": issue,
                    "instructions": task.unwrap_or_default(),
                    // No flag at all: the publish module's autopilot setting decides, server-side.
                    "autopilot": if autopilot { Some(true) } else if no_autopilot { Some(false) } else { None },
                    "model_override": model,
                    "subagent_model_override": subagent_model,
                });
                // A person is launching, so `origin` stays unset: the field marks machine
                // launchers (the burn-down scheduler, red-team hunters), not this command.
                let Some(session) = machine.post("/api/sessions", Some(&body)).await? else {
                    println!("colony started");
                    return Ok(EXIT_OK);
                };
                if json {
                    println!("{}", pretty(&session)?);
                } else {
                    let status = session["status"].as_str().unwrap_or("queued");
                    println!("colony {} started ({status})", session["id"].as_str().unwrap_or("?"));
                }
                Ok(EXIT_OK)
            })
            .await
        }
        Command::List { org, status } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                let sessions = machine.get("/api/sessions").await?;
                let filtered = filter_sessions(
                    sessions.as_array().cloned().unwrap_or_default(),
                    org.as_deref(),
                    status.as_deref(),
                );
                if json {
                    println!("{}", pretty(&Value::Array(filtered.clone()))?);
                    return Ok(EXIT_OK);
                }
                // A person sees a word, a script sees an empty stdout and a 0.
                if filtered.is_empty() {
                    eprintln!("no colonies");
                    return Ok(EXIT_OK);
                }
                for s in &filtered {
                    let task = match &s["issue"] {
                        Value::Number(issue) => format!("#{issue} {}", s["issue_title"].as_str().unwrap_or_default()),
                        _ => s["instructions"].as_str().unwrap_or_default().to_string(),
                    };
                    println!(
                        "{:<10}  {:<18}  {:<24}  {}",
                        s["id"].as_str().unwrap_or("?"),
                        s["status"].as_str().unwrap_or("?"),
                        s["repo"].as_str().unwrap_or("?"),
                        util::truncate(&task, 80)
                    );
                }
                Ok(EXIT_OK)
            })
            .await
        }
        Command::Status { id } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                let detail = machine.get(&format!("/api/sessions/{id}")).await?;
                if json {
                    println!("{}", pretty(&detail)?);
                    return Ok(EXIT_OK);
                }
                for (label, value) in [
                    ("colony", format!("{} ({})", id, detail["status"].as_str().unwrap_or("?"))),
                    ("repo", detail["repo"].as_str().unwrap_or("?").to_string()),
                    ("branch", detail["branch"].as_str().unwrap_or("-").to_string()),
                    ("pr", detail["pr_url"].as_str().unwrap_or("-").to_string()),
                    ("cost", format!("${:.2}", detail["cost_usd"].as_f64().unwrap_or(0.0))),
                ] {
                    println!("{label:<9} {value}");
                }
                if let Some(diagnosis) = detail["diagnosis"]["text"].as_str() {
                    println!("{:<9} {}", "now", diagnosis);
                }
                if let Some(error) = detail["error"].as_str() {
                    println!("{:<9} {}", "error", error);
                }
                Ok(EXIT_OK)
            })
            .await
        }
        Command::Logs { id, follow } => {
            let json = cli.json;
            if follow {
                client_command(cli, move |machine| async move { follow_logs(machine, &id, json).await }).await
            } else {
                client_command(cli, move |machine| async move {
                    let detail = machine.get(&format!("/api/sessions/{id}")).await?;
                    let events = detail["recent_events"].as_array().cloned().unwrap_or_default();
                    for event in &events {
                        if json {
                            println!("{event}");
                        } else {
                            println!("{}", recent_line(event));
                        }
                    }
                    Ok(EXIT_OK)
                })
                .await
            }
        }
        Command::Diff { id, stat } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                let body = machine.get(&format!("/api/sessions/{id}/diff")).await?;
                if json {
                    println!("{}", pretty(&body)?);
                    return Ok(EXIT_OK);
                }
                if body["truncated"] == json!(true) {
                    eprintln!("note: the diff was truncated; the colony's worktree has the whole thing");
                }
                if !stat {
                    // The raw diff on stdout, so it pipes into `git apply`, a pager or a file.
                    print!("{}", body["diff"].as_str().unwrap_or_default());
                    return Ok(EXIT_OK);
                }
                let files = values(&body["files"]);
                let count = |v: &Value| v.as_u64().unwrap_or(0);
                let stat = |a: u64, r: u64, what: &str| format!("+{a:<4} -{r:<4} {what}");
                for file in files {
                    println!(
                        "{}",
                        stat(
                            count(&file["added"]),
                            count(&file["removed"]),
                            file["path"].as_str().unwrap_or("?")
                        )
                    );
                }
                let total = format!("across {} files", files.len());
                println!("{}", stat(count(&body["added"]), count(&body["removed"]), &total));
                Ok(EXIT_OK)
            })
            .await
        }
        Command::Ask { id } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                let Some((pending, body)) = pending_question(&machine, &id).await? else {
                    eprintln!("{id} is not asking anything");
                    return Ok(EXIT_CONFLICT);
                };
                if json {
                    println!("{}", pretty(&body)?);
                    return Ok(EXIT_OK);
                }
                println!("{id} is asking ({})", pending.risk);
                for q in &pending.questions {
                    println!();
                    println!("{}", q.question);
                    for (n, option) in q.options.iter().enumerate() {
                        let why = option.description.as_deref().map(|d| format!(" — {d}")).unwrap_or_default();
                        println!("  {}) {}{}", n + 1, option.label, why);
                    }
                }
                Ok(EXIT_OK)
            })
            .await
        }
        Command::Answer { id, answer } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                let Some((pending, _)) = pending_question(&machine, &id).await? else {
                    eprintln!("{id} is not asking anything; there is no question to answer");
                    return Ok(EXIT_CONFLICT);
                };
                let resolved = match resolve_answer(&answer, &pending) {
                    Ok(resolved) => resolved,
                    // Nothing in the question to match against: the same empty-inbox conflict a
                    // person would read in the cockpit, not a generic 1.
                    Err(e @ MatchError::NoQuestions) => {
                        return Err(Fail::Server {
                            status: 409,
                            message: e.to_string(),
                        });
                    }
                    Err(e) => return Err(anyhow::anyhow!("{e}").into()),
                };
                let body = resolved.answer_body(&pending.question_id);
                if machine
                    .post(&format!("/api/sessions/{id}/answer"), Some(&body))
                    .await?
                    .is_none()
                    && json
                {
                    println!("{}", pretty(&body)?);
                } else if !json {
                    println!("answer sent to {id}");
                }
                Ok(EXIT_OK)
            })
            .await
        }
        Command::Stop { id } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                match machine.post(&format!("/api/sessions/{id}/stop"), None).await? {
                    Some(reply) if json => println!("{}", pretty(&reply)?),
                    Some(reply) => println!("colony {id}: {}", reply["result"].as_str().unwrap_or("stopped")),
                    None => println!("colony {id}: stopped"),
                }
                Ok(EXIT_OK)
            })
            .await
        }
        Command::Resume { id } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                match machine.post(&format!("/api/sessions/{id}/resume"), None).await? {
                    Some(session) if json => println!("{}", pretty(&session)?),
                    Some(session) => {
                        println!(
                            "colony {} resumed ({})",
                            session["id"].as_str().unwrap_or(&id),
                            session["status"].as_str().unwrap_or("queued")
                        )
                    }
                    None => println!("colony {id} resumed"),
                }
                Ok(EXIT_OK)
            })
            .await
        }
        Command::Pr { id } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                let detail = machine.get(&format!("/api/sessions/{id}")).await?;
                if json {
                    println!(
                        "{}",
                        pretty(&json!({
                            "id": id,
                            "pr_url": detail["pr_url"],
                            "status": detail["status"],
                            "ci_state": detail["ci_state"],
                            "merged_at": detail["merged_at"],
                        }))?
                    );
                    return Ok(EXIT_OK);
                }
                match detail["pr_url"].as_str() {
                    Some(url) => {
                        let state = detail["status"].as_str().unwrap_or("unknown");
                        let ci = detail["ci_state"]
                            .as_str()
                            .map(|c| format!(", checks {c}"))
                            .unwrap_or_default();
                        println!("{url} ({state}{ci})");
                    }
                    None => println!("no pull request yet"),
                }
                Ok(EXIT_OK)
            })
            .await
        }
        Command::Map { repo, find } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                let Some((owner, name)) = repo.split_once('/') else {
                    return Err(Fail::Transport(anyhow::anyhow!(
                        "the repository is owner/repo (got \"{repo}\")"
                    )));
                };
                let body = machine.get(&format!("/api/maps/{owner}/{name}")).await?;
                // No map drawn yet is a not-found the words explain, not an empty outline.
                if body["map"].is_null() {
                    eprintln!("no map for {repo} yet: draw one from the cockpit's Map view");
                    return Ok(EXIT_NOT_FOUND);
                }
                let doc = &body["map"];
                if let Some(query) = &find {
                    let found = search_map(doc, query)?;
                    if json {
                        println!("{}", pretty(&found)?);
                    } else {
                        for component in values(&found["components"]) {
                            print!("{}", component_outline(component));
                        }
                    }
                    return Ok(EXIT_OK);
                }
                if json {
                    println!("{}", pretty(doc)?);
                } else {
                    print!("{}", map_outline(doc));
                }
                Ok(EXIT_OK)
            })
            .await
        }
        Command::Token { command } => token_command(cli, command).await,
    }
}

/// Every client command resolves the mothership and the token first, so the flag errors surface
/// before anything is sent; then the failure a command answers with decides the exit code.
async fn client_command<F, Fut>(cli: &Cli, f: F) -> i32
where
    F: FnOnce(Machine) -> Fut,
    Fut: std::future::Future<Output = Result<i32, Fail>>,
{
    let machine = match Machine::from_cli(cli) {
        Ok(machine) => machine,
        Err(e) => {
            eprintln!("colonizer: {e:#}");
            return EXIT_ERROR;
        }
    };
    match f(machine).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("colonizer: {e}");
            e.exit_code()
        }
    }
}

async fn token_command(cli: &Cli, command: TokenCommand) -> i32 {
    let json = cli.json;
    client_command(cli, move |machine| async move {
        match command {
            TokenCommand::List => {
                let tokens = machine.get("/api/tokens").await?;
                if json {
                    println!("{}", pretty(&tokens)?);
                    return Ok(EXIT_OK);
                }
                let rows = tokens.as_array().cloned().unwrap_or_default();
                // A person sees a word, a script sees an empty stdout and a 0.
                if rows.is_empty() {
                    eprintln!("no tokens");
                    return Ok(EXIT_OK);
                }
                for t in rows {
                    let created = t["created_at"].as_str().map(|c| c.get(..10).unwrap_or(c)).unwrap_or("?");
                    println!(
                        "{:<14} {:<20} {:<8} {:<40} created {created}",
                        t["id"].as_str().unwrap_or("?"),
                        util::truncate(t["name"].as_str().unwrap_or("?"), 20),
                        t["scope"].as_str().unwrap_or("?"),
                        limit_note(&t),
                    );
                }
                Ok(EXIT_OK)
            }
            TokenCommand::Create {
                name,
                scope,
                org,
                repo,
                max_concurrent,
                budget_usd_per_day,
            } => {
                let body = json!({
                    "name": name,
                    "scope": scope.as_str(),
                    "orgs": org,
                    "repos": repo,
                    "max_concurrent": max_concurrent,
                    "budget_usd_per_day": budget_usd_per_day,
                });
                let created = machine
                    .post("/api/tokens", Some(&body))
                    .await?
                    .ok_or_else(|| Fail::Transport(anyhow::anyhow!("the mothership answered no body")))?;
                if json {
                    println!("{}", pretty(&created)?);
                } else {
                    // The plaintext on stdout (pipe-able), the warning where a person reads.
                    println!("{}", created["token"].as_str().unwrap_or("?"));
                    eprintln!("this is the only time the token is shown; store it now — it cannot be read back");
                }
                Ok(EXIT_OK)
            }
            TokenCommand::Revoke { id } => {
                machine.delete(&format!("/api/tokens/{id}")).await?;
                if !json {
                    println!("token {id} revoked");
                }
                Ok(EXIT_OK)
            }
        }
    })
    .await
}

/// The org/repo limits a `token list` line shows: `*/*` when there are none.
fn limit_note(token: &Value) -> String {
    let list = |values: Option<&Vec<Value>>, fallback: &str| match values {
        Some(values) if !values.is_empty() => values.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","),
        _ => fallback.to_string(),
    };
    format!(
        "{}/{}",
        list(token["orgs"].as_array(), "*"),
        list(token["repos"].as_array(), "*")
    )
}

/// The client-side `--org`/`--status` filters of `list`: everything the mothership already sent
/// this token, narrowed locally. Status names compare as the API spells them, case-insensitively.
fn filter_sessions(sessions: Vec<Value>, org: Option<&str>, status: Option<&str>) -> Vec<Value> {
    sessions
        .into_iter()
        .filter(|s| match org {
            None => true,
            Some(org) => s["org"].as_str() == Some(org) || s["repo"].as_str().unwrap_or_default().split('/').next() == Some(org),
        })
        .filter(|s| match status {
            None => true,
            Some(status) => s["status"].as_str().is_some_and(|s| s.eq_ignore_ascii_case(status)),
        })
        .collect()
}

/// One recent event as one compact line: `42  2026-09-25T10:32:01  status  Working on the parser`.
fn recent_line(event: &Value) -> String {
    let seq = event["seq"].as_u64().unwrap_or(0);
    let ts = event["ts"].as_str().unwrap_or("");
    let kind = event["type"].as_str().unwrap_or("event");
    format!(
        "{seq:<6} {ts:<27} {kind:<14} {}",
        util::truncate(event["summary"].as_str().unwrap_or_default(), 120)
    )
}

// ---------------------------------------------------------------------------
// The repository map: searching it and drawing it as text.
// ---------------------------------------------------------------------------

/// A JSON array as a slice, empty when it is absent or not an array — every map-document field
/// the renderers walk.
fn values(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or_default()
}

/// A component's label by its id, for rendering connections: the label when it has one, else the
/// raw id.
fn component_labels(components: &[Value]) -> impl Fn(&str) -> String + '_ {
    move |id| {
        components
            .iter()
            .find(|c| c["id"].as_str() == Some(id))
            .and_then(|c| c["label"].as_str())
            .unwrap_or(id)
            .to_string()
    }
}

/// One connection's ends as labels — the spelling both renderings share.
fn connection_ends(x: &Value, label_of: &impl Fn(&str) -> String) -> (String, String) {
    (
        label_of(x["from"].as_str().unwrap_or_default()),
        label_of(x["to"].as_str().unwrap_or_default()),
    )
}

/// Searches a stored map document (`GET /api/maps/{owner}/{name}`'s `map`) for the components
/// matching `query`, case-insensitively: a substring of the id, label, sublabel, type or a source
/// path — or a file under a source path (`src/auth/login.rs` finds the component whose source is
/// `src/auth`). The answer is `{repo, revision, query, components}`, each hit carrying the
/// connections that touch it; an empty query is refused. Pure, so the CLI and the MCP tool agree.
pub(crate) fn search_map(doc: &Value, query: &str) -> Result<Value, Fail> {
    let query = query.trim();
    if query.is_empty() {
        return Err(Fail::Transport(anyhow::anyhow!("the map search query is empty")));
    }
    let wanted = query.to_lowercase();
    let map = &doc["map"];
    let label_of = component_labels(values(&map["components"]));
    let hits: Vec<Value> = values(&map["components"])
        .iter()
        .filter(|c| component_matches(c, &wanted))
        .map(|c| {
            let id = c["id"].as_str().unwrap_or_default();
            json!({
                "id": c["id"],
                "label": c["label"],
                "type": c["type"],
                "sublabel": c["sublabel"],
                "sources": c["sources"],
                "connections": values(&map["connections"])
                    .iter()
                    .filter(|x| x["from"].as_str() == Some(id) || x["to"].as_str() == Some(id))
                    .map(|x| {
                        let (from, to) = connection_ends(x, &label_of);
                        json!({"from": from, "to": to, "label": x["label"]})
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({"repo": doc["repo"], "revision": doc["revision"], "query": query, "components": hits}))
}

/// Whether one component matches the lowercased query, the way [`search_map`] says.
fn component_matches(c: &Value, wanted: &str) -> bool {
    let holds = |v: &Value| v.as_str().unwrap_or_default().to_lowercase().contains(wanted);
    holds(&c["id"])
        || holds(&c["label"])
        || holds(&c["sublabel"])
        || holds(&c["type"])
        || values(&c["sources"]).iter().any(|s| {
            let path = s["path"].as_str().unwrap_or_default().to_lowercase();
            let dir = path.trim_end_matches('/');
            path.contains(wanted) || wanted == dir || wanted.starts_with(&format!("{dir}/"))
        })
}

/// A stored map document drawn as a compact text outline: title and subtitle, the revision, the
/// components grouped under their boundary labels (the ones in no boundary last), then the
/// connections by component label.
fn map_outline(doc: &Value) -> String {
    fn id_of(c: &Value) -> &str {
        c["id"].as_str().unwrap_or_default()
    }
    let map = &doc["map"];
    let components = values(&map["components"]);
    let mut out = String::new();
    out.push_str(map["title"].as_str().unwrap_or("Architecture"));
    if let Some(subtitle) = map["subtitle"].as_str() {
        out.push_str(&format!(" — {subtitle}"));
    }
    out.push('\n');
    if let Some(revision) = doc["revision"].as_str() {
        out.push_str(&format!("revision {revision}\n"));
    }
    let mut grouped: Vec<&str> = Vec::new();
    for boundary in values(&map["boundaries"]) {
        // A component two boundaries wrap renders under the first one only.
        let members: Vec<&Value> = components
            .iter()
            .filter(|c| !grouped.contains(&id_of(c)))
            .filter(|c| values(&boundary["wraps"]).iter().any(|w| w.as_str() == Some(id_of(c))))
            .collect();
        if members.is_empty() {
            continue;
        }
        out.push('\n');
        if let Some(label) = boundary["label"].as_str().filter(|l| !l.is_empty()) {
            out.push_str(&format!("{label}:\n"));
        }
        for c in members {
            out.push_str(&component_outline(c));
            grouped.push(id_of(c));
        }
    }
    let rest: Vec<&Value> = components.iter().filter(|c| !grouped.contains(&id_of(c))).collect();
    if !rest.is_empty() && !grouped.is_empty() {
        out.push('\n');
    }
    for c in rest {
        out.push_str(&component_outline(c));
    }
    let label_of = component_labels(components);
    let connections: Vec<String> = values(&map["connections"])
        .iter()
        .map(|x| {
            let (from, to) = connection_ends(x, &label_of);
            format!(
                "{from} → {to}{}",
                x["label"].as_str().map(|l| format!("  {l}")).unwrap_or_default()
            )
        })
        .collect();
    if !connections.is_empty() {
        out.push('\n');
        out.push_str(&connections.join("\n"));
        out.push('\n');
    }
    out
}

/// One component's outline block: `label (type) — sublabel`, its source paths (`path:line`)
/// indented under it.
fn component_outline(c: &Value) -> String {
    let mut out = format!(
        "{} ({}){}\n",
        c["label"].as_str().unwrap_or_else(|| c["id"].as_str().unwrap_or("?")),
        c["type"].as_str().unwrap_or("?"),
        c["sublabel"].as_str().map(|s| format!(" — {s}")).unwrap_or_default()
    );
    for s in values(&c["sources"]) {
        let path = s["path"].as_str().unwrap_or_default();
        match s["line"].as_u64() {
            Some(line) => out.push_str(&format!("    {path}:{line}\n")),
            None => out.push_str(&format!("    {path}\n")),
        }
    }
    out
}

/// The pending question behind `ask`/`answer`, with the raw body `--json` reprints. `None` is the
/// question route's 204 — the colony is not asking anything — the empty-inbox case `ask` and
/// `answer` report with their own exit code (5), not an error; an unknown colony stays a 404.
async fn pending_question(machine: &Machine, id: &str) -> Result<Option<(PendingQuestion, Value)>, Fail> {
    let Some(body) = machine.get_optional(&format!("/api/sessions/{id}/question")).await? else {
        return Ok(None);
    };
    Ok(Some((read_question(&body)?, body)))
}

/// The question body, or the refusal when the mothership answered in a shape this build cannot read.
fn read_question(body: &Value) -> Result<PendingQuestion, Fail> {
    serde_json::from_value(body.clone()).map_err(|e| {
        Fail::Transport(anyhow::anyhow!(
            "the question came back in a shape this build cannot read: {e}"
        ))
    })
}

fn pretty(value: &Value) -> Result<String, Fail> {
    serde_json::to_string_pretty(value)
        .map_err(|e| Fail::Transport(anyhow::anyhow!("could not serialise the mothership's answer: {e}")))
}

/// `colonizer open`, as it has always been: reprint the link startup printed and hand it to a
/// browser. Local on purpose — the mothership on another machine cannot be opened from here.
fn open() -> Result<()> {
    let cfg = Settings::from_env()?;
    let token = auth::load_or_create(&cfg.config_dir)?;
    let url = auth::login_url(&cfg.bind, &token);
    println!("{url}");
    auth::open_browser(&url);
    Ok(())
}

/// `colonizer logs <id> -f`: the events WebSocket, streaming until the mothership closes it or the
/// operator presses Ctrl-C. The wire opens with three housekeeping frames (`run_epoch`, `session`,
/// `replay_done`) that frame the replay; a terminal wants the events, so those are skipped.
async fn follow_logs(machine: Machine, id: &str, json: bool) -> Result<i32, Fail> {
    use futures_util::StreamExt as _;
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest as _, http::header::HeaderValue};

    let host = machine.base.trim_start_matches("http://");
    let mut request = format!("ws://{host}/api/sessions/{id}/events")
        .into_client_request()
        .map_err(|e| Fail::Transport(anyhow::anyhow!("the events URL is not a WebSocket URL: {e}")))?;
    request.headers_mut().insert(
        reqwest::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", machine.token))
            .map_err(|e| Fail::Transport(anyhow::anyhow!("the token is not a valid header value: {e}")))?,
    );
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| Fail::Transport(anyhow::anyhow!("no events stream at {}: {e}", machine.base)))?;
    let (_, mut read) = socket.split();
    let mut stdout = std::io::stdout();
    loop {
        let frame = tokio::select! {
            // Ctrl-C ends the follow cleanly; the colony and the stream are left alone.
            _ = tokio::signal::ctrl_c() => break,
            frame = read.next() => frame,
        };
        let Some(frame) = frame else { break };
        let Message::Text(line) =
            frame.map_err(|e| Fail::Transport(anyhow::anyhow!("the events stream dropped mid-frame: {e}")))?
        else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if matches!(value["type"].as_str(), Some("run_epoch" | "session" | "replay_done")) {
            continue;
        }
        let line = if json { value.to_string() } else { recent_line(&value) };
        // A closed stdout (the reader went away) ends the loop on the next frame.
        if writeln!(stdout, "{line}").and_then(|_| stdout.flush()).is_err() {
            break;
        }
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("colonizer").chain(args.iter().copied()))
    }

    /// The documented commands all still parse, and the global flags reach them from either side.
    #[test]
    fn the_documented_commands_parse() {
        for args in [
            &["version"][..],
            &["update"][..],
            &["update", "--force"][..],
            &["open"][..],
            &["login-item", "enable"][..],
            &["login-item", "disable"][..],
            &["login-item", "status"][..],
            &["telemetry", "show"][..],
            &["telemetry", "on"][..],
            &["telemetry", "off"][..],
            &["completions", "bash"][..],
            &["man"][..],
            &["launch", "acme/app"][..],
            &["list", "--org", "acme", "--status", "running"][..],
            &["status", "abc123"][..],
            &["logs", "abc123"][..],
            &["logs", "abc123", "-f"][..],
            &["diff", "abc123"][..],
            &["diff", "abc123", "--stat"][..],
            &["ask", "abc123"][..],
            &["answer", "abc123", "1"][..],
            &["stop", "abc123"][..],
            &["resume", "abc123"][..],
            &["pr", "abc123"][..],
            &["map", "acme/app"][..],
            &["map", "acme/app", "--find", "login"][..],
            &["mcp"][..],
            &["mcp", "--scope", "launch"][..],
            &["token", "list"][..],
            &[
                "token",
                "create",
                "ci",
                "--scope",
                "operate",
                "--org",
                "acme",
                "--repo",
                "acme/app",
                "--max-concurrent",
                "2",
                "--budget-usd-per-day",
                "5",
            ][..],
            &["token", "revoke", "tok_x"][..],
        ] {
            assert!(parse(args).is_ok(), "{args:?} should parse");
        }
        // The global flags work on both sides of the subcommand.
        for args in [&["--json", "list"][..], &["list", "--json"][..]] {
            assert!(parse(args).unwrap().json, "{args:?} should carry --json");
        }
    }

    /// The launch flags arrive as the API body wants them, task included.
    #[test]
    fn a_launch_with_issue_model_and_autopilot_off_parses() {
        let cli = parse(&[
            "launch",
            "owner/repo",
            "--issue",
            "5",
            "--model",
            "haiku",
            "--subagent-model",
            "sonnet",
            "--no-autopilot",
            "fix the thing",
        ])
        .unwrap();
        let Command::Launch {
            repo,
            issue,
            model,
            subagent_model,
            autopilot,
            no_autopilot,
            task,
        } = cli.command.unwrap()
        else {
            panic!("launch did not parse");
        };
        assert_eq!(repo, "owner/repo");
        assert_eq!(issue, Some(5));
        assert_eq!(
            (model.as_deref(), subagent_model.as_deref(), task.as_deref()),
            (Some("haiku"), Some("sonnet"), Some("fix the thing"))
        );
        assert!(!autopilot);
        assert!(no_autopilot);
        // The two autopilot flags refuse to combine: the answer would depend on their order.
        assert!(parse(&["launch", "owner/repo", "--autopilot", "--no-autopilot"]).is_err());
    }

    /// An argument nobody planned for is a usage error (exit 2), not a silently started server —
    /// the same promise the hand-written parser made.
    #[test]
    fn an_argument_nobody_planned_for_is_a_usage_error() {
        for args in [
            &["login-item", "start"][..],
            &["telemetry", "maybe"][..],
            &["update", "extra"][..],
            &["update", "--bogus"][..],
            &["frobnicate"][..],
        ] {
            let err = parse(args).unwrap_err();
            assert_eq!(err.exit_code(), EXIT_USAGE, "{args:?} should be a usage error");
            assert!(matches!(
                err.kind(),
                ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand | ErrorKind::InvalidValue
            ));
        }
    }

    /// `--help` and `--version` are answers, not commands: clap turns them into display errors so
    /// `run` never reaches the mothership, and both exit 0.
    #[test]
    fn help_and_version_print_and_exit_without_starting_anything() {
        for args in [&["--help"][..], &["-h"][..], &["--version"][..], &["-V"][..]] {
            let err = parse(args).unwrap_err();
            assert!(
                matches!(err.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion),
                "{args:?} should display and stop, got {:?}",
                err.kind()
            );
            assert_eq!(err.exit_code(), EXIT_OK);
        }
        // The command structure itself is internally consistent (every subcommand reachable).
        Cli::command().debug_assert();
    }

    /// The exit codes a script reads are the ones `--help` documents.
    #[test]
    fn http_statuses_map_to_the_documented_exit_codes() {
        let fail = |status| {
            Fail::Server {
                status,
                message: "x".into(),
            }
            .exit_code()
        };
        assert_eq!(fail(401), EXIT_FORBIDDEN);
        assert_eq!(fail(403), EXIT_FORBIDDEN);
        assert_eq!(fail(404), EXIT_NOT_FOUND);
        assert_eq!(fail(409), EXIT_CONFLICT);
        assert_eq!(fail(429), EXIT_CAP);
        assert_eq!(fail(400), EXIT_ERROR);
        assert_eq!(fail(500), EXIT_ERROR);
        assert_eq!(Fail::Transport(anyhow::anyhow!("no route")).exit_code(), EXIT_ERROR);
    }

    /// A pending question with three options, as an agent might emit it.
    fn pending(questions: usize) -> PendingQuestion {
        serde_json::from_value(json!({
            "question_id": "q1",
            "risk": "workspace_write",
            "questions": (0..questions).map(|_| json!({
                "question": "Push the branch now?",
                "header": "Push",
                "multi_select": false,
                "options": [
                    {"label": "Push now"},
                    {"label": "Wait for CI"},
                    {"label": "Discard the work"},
                ],
            })).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    /// Numbers, labels and free text each find their answer — and a bare number that names no
    /// option is refused rather than quietly read as text.
    #[test]
    fn an_answer_argument_matches_by_number_label_or_free_text() {
        let one = pending(1);
        let pick = |resolved: &Resolved| match resolved {
            Resolved::Choice { label, .. } => label.clone(),
            Resolved::FreeText { text, .. } => text.clone(),
        };
        assert_eq!(pick(&resolve_answer("1", &one).unwrap()), "Push now");
        assert_eq!(pick(&resolve_answer("3", &one).unwrap()), "Discard the work");
        assert_eq!(pick(&resolve_answer("wait for ci", &one).unwrap()), "Wait for CI");
        assert_eq!(pick(&resolve_answer("  PUSH NOW  ", &one).unwrap()), "Push now");
        assert_eq!(
            resolve_answer("hold it until the morning", &one).unwrap(),
            Resolved::FreeText {
                question: "Push the branch now?".into(),
                multi_select: false,
                text: "hold it until the morning".into()
            }
        );
        assert_eq!(
            resolve_answer("9", &one).unwrap_err(),
            MatchError::NoSuchOption { number: 9, options: 3 }
        );
    }

    /// More than one question pending: an option reference still works (it names the first
    /// question's options), but free text has no one question to answer and is refused.
    #[test]
    fn free_text_is_refused_when_several_questions_are_pending() {
        let two = pending(2);
        assert_eq!(
            resolve_answer("2", &two).unwrap(),
            Resolved::Choice {
                question: "Push the branch now?".into(),
                multi_select: false,
                label: "Wait for CI".into()
            }
        );
        assert_eq!(
            resolve_answer("not sure yet", &two).unwrap_err(),
            MatchError::FreeTextNeedsOneQuestion { questions: 2 }
        );
    }

    /// The body an answer sends mirrors the cockpit's choice card: keyed by the question's text, a
    /// bare label when single-select, a one-element list when multi-select, free text in the value
    /// and in the response.
    #[test]
    fn the_answer_body_mirrors_the_cockpit_choice_card() {
        let single = resolve_answer("1", &pending(1)).unwrap().answer_body("q1");
        assert_eq!(single["question_id"], json!("q1"));
        assert_eq!(single["answers"]["Push the branch now?"], json!("Push now"));
        assert_eq!(single["response"], Value::Null);

        let multi: PendingQuestion = serde_json::from_value(json!({
            "question_id": "q2",
            "risk": "read_only",
            "questions": [{
                "question": "Which files may I touch?",
                "multi_select": true,
                "options": [{"label": "src/**"}, {"label": "docs/**"}],
            }],
        }))
        .unwrap();
        let picked = resolve_answer("docs/**", &multi).unwrap();
        assert_eq!(
            picked,
            Resolved::Choice {
                question: "Which files may I touch?".into(),
                multi_select: true,
                label: "docs/**".into()
            }
        );
        let body = picked.answer_body("q2");
        assert_eq!(body["answers"]["Which files may I touch?"], json!(["docs/**"]));

        let free = resolve_answer("hold the docs", &multi).unwrap().answer_body("q2");
        assert_eq!(free["answers"]["Which files may I touch?"], json!(["hold the docs"]));
        assert_eq!(free["response"], json!("hold the docs"));
    }

    /// The org and status filters narrow a session list the way `--org`/`--status` say.
    #[test]
    fn list_filters_narrow_by_org_and_status() {
        let sessions = vec![
            json!({"id": "a", "org": "acme", "repo": "acme/app", "status": "running"}),
            json!({"id": "b", "org": "acme", "repo": "acme/web", "status": "pr_opened"}),
            json!({"id": "c", "org": "other", "repo": "other/tool", "status": "running"}),
        ];
        assert_eq!(filter_sessions(sessions.clone(), Some("acme"), None).len(), 2);
        assert_eq!(filter_sessions(sessions.clone(), None, Some("RUNNING")).len(), 2);
        let both = filter_sessions(sessions, Some("acme"), Some("pr_opened"));
        assert_eq!(both.iter().map(|s| s["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["b"]);
    }

    /// A stored map document, as `GET /api/maps/{owner}/{name}`'s `map` carries it.
    fn map_doc() -> Value {
        json!({
            "repo": "acme/app",
            "revision": "abc123",
            "generated_at": "2026-09-26T00:00:00Z",
            "session": "m1",
            "map": {
                "title": "Demo",
                "subtitle": "the app",
                "components": [
                    {"id": "web", "type": "frontend", "label": "Web", "pos": [10, 20], "size": [160, 60],
                     "sources": [{"path": "web/src", "line": 3}]},
                    {"id": "auth", "type": "backend", "label": "Auth", "sublabel": "logins and tokens",
                     "pos": [300, 20], "sources": [{"path": "src/auth"}]},
                    {"id": "gh", "type": "external", "label": "GitHub", "pos": [600, 20]}
                ],
                "connections": [{"from": "web", "to": "auth", "label": "REST"}, {"from": "auth", "to": "gh"}],
                "boundaries": [{"label": "app", "wraps": ["web", "auth"]}, {"label": "core", "wraps": ["auth"]}]
            }
        })
    }

    /// A map search finds components by label, by source path and by a file under a source
    /// directory — case-insensitively — and a query nothing matches finds nothing.
    #[test]
    fn a_map_search_finds_labels_types_and_source_paths() {
        fn ids(found: &Value) -> Vec<&str> {
            found["components"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["id"].as_str().unwrap())
                .collect()
        }
        let found = search_map(&map_doc(), " AUTH ").unwrap();
        assert_eq!(ids(&found), ["auth"], "a label or id hit, case-insensitively, trimmed");
        assert_eq!(found["repo"], "acme/app");
        assert_eq!(found["revision"], "abc123");
        assert_eq!(found["query"], "AUTH");
        assert_eq!(
            found["components"][0]["connections"][0],
            json!({"from": "Web", "to": "Auth", "label": "REST"}),
            "the hit carries its connections, from/to as labels"
        );
        assert_eq!(ids(&search_map(&map_doc(), "frontend").unwrap()), ["web"], "a type hit");
        assert_eq!(
            ids(&search_map(&map_doc(), "src/auth/login.rs").unwrap()),
            ["auth"],
            "a file under a component's source directory finds it"
        );
        assert_eq!(ids(&search_map(&map_doc(), "src/auth/").unwrap()), ["auth"]);
        assert!(ids(&search_map(&map_doc(), "nowhere").unwrap()).is_empty());
        assert!(search_map(&map_doc(), "   ").is_err(), "an empty query is refused");
    }

    /// The human outline groups the components under their boundaries, leaves the ones in no
    /// boundary for last, and ends with the connections.
    #[test]
    fn the_map_outline_draws_boundaries_components_and_connections() {
        let out = map_outline(&map_doc());
        assert!(out.contains("Demo — the app\nrevision abc123\n"), "{out}");
        assert!(
            out.contains("\napp:\nWeb (frontend)\n    web/src:3\nAuth (backend) — logins and tokens\n    src/auth\n"),
            "{out}"
        );
        assert!(out.contains("GitHub (external)"), "a component in no boundary is drawn last");
        assert_eq!(
            out.matches("Auth (backend)").count(),
            1,
            "a component two boundaries wrap renders under the first only"
        );
        assert!(out.ends_with("\nWeb → Auth  REST\nAuth → GitHub\n"), "{out}");
    }

    /// `--host` accepts the shapes a mothership answers on: a bare host gets the default port, a
    /// spelled port is kept, a bare IPv6 literal is bracketed before the port follows, a bracketed
    /// literal is kept, and a URL (or an unclosed bracket) is refused.
    #[test]
    fn the_host_flag_takes_hosts_and_ports_not_urls() {
        assert_eq!(parse_host("mothership.tailnet").unwrap(), "mothership.tailnet:7878");
        assert_eq!(parse_host("mothership.tailnet:8123").unwrap(), "mothership.tailnet:8123");
        assert_eq!(parse_host("127.0.0.1:7878").unwrap(), "127.0.0.1:7878");
        assert_eq!(parse_host("::1").unwrap(), "[::1]:7878");
        assert_eq!(parse_host("fd7a::12").unwrap(), "[fd7a::12]:7878");
        assert_eq!(parse_host("[::1]:7878").unwrap(), "[::1]:7878");
        assert_eq!(parse_host("[::1]").unwrap(), "[::1]:7878");
        assert!(parse_host("http://mothership:7878").is_err());
        assert!(parse_host("https://mothership").is_err());
        assert!(parse_host("[::1:7878").is_err(), "a bracket nobody closed");
    }

    /// A URL `--host` is a usage error at parse time, from either side of the subcommand.
    #[test]
    fn a_host_flag_with_a_scheme_is_a_usage_error() {
        assert!(parse(&["--host", "http://mothership:7878", "list"]).is_err());
        assert!(parse(&["list", "--host", "https://mothership"]).is_err());
    }

    /// A pending question that carries no questions at all is refused with the cockpit hint, not a
    /// nonsense "0 questions are pending" — and it reads as the empty-inbox conflict (5).
    #[test]
    fn an_answer_to_a_question_with_no_questions_is_a_conflict() {
        let err = resolve_answer("1", &pending(0)).unwrap_err();
        assert!(matches!(err, MatchError::NoQuestions));
        assert!(err.to_string().contains("answer it in the cockpit"), "{err}");
        let refused = Fail::Server {
            status: 409,
            message: err.to_string(),
        };
        assert_eq!(refused.exit_code(), EXIT_CONFLICT);
    }

    /// `ask`/`answer` read the question route's shapes, no message sniffing: a 204 is the empty
    /// inbox (`None`, the colony is not asking anything) and an unknown colony keeps the 404.
    #[tokio::test]
    async fn ask_separates_nothing_pending_from_an_unknown_colony() {
        use axum::{Json, Router, http::StatusCode, routing::get};

        let app = Router::new()
            .route("/api/sessions/abc/question", get(|| async { StatusCode::NO_CONTENT }))
            .route(
                "/api/sessions/zzz/question",
                get(|| async { (StatusCode::NOT_FOUND, Json(json!({"error": "no such session"}))) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let machine = Machine::for_tests(format!("http://{addr}"), "col".into());
        assert!(pending_question(&machine, "abc").await.unwrap().is_none());
        assert_eq!(
            pending_question(&machine, "zzz").await.unwrap_err().exit_code(),
            EXIT_NOT_FOUND
        );
    }
}
