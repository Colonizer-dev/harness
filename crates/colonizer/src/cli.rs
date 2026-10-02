//! The `colonizer` command line, built on clap: the commands that run against this machine
//! (`version`, `update`, `open`, `login-item`, `telemetry`), and the client commands that drive a
//! mothership already running somewhere — here or across a tailnet (`launch`, `list`, `status`,
//! `logs`, `diff`, `ask`, `answer`, `stop`, `resume`, `pr`, `map`, `loop`, `token`, `mcp`).
//!
//! With no subcommand at all the binary starts the mothership, exactly as it always has.
//!
//! Exit codes are part of the interface, so scripts can tell a typo from a refusal: [`EXIT_ERROR`]
//! and friends are defined once here, shown in `--help`, and returned from [`run`].

use crate::{Settings, auth, util};
use anyhow::{Context as _, Result};
use chrono::{DateTime, FixedOffset};
use clap::{ArgAction, CommandFactory as _, Parser, Subcommand, ValueEnum};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{Read as _, Write as _};
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
/// `pr --wait` followed the pull request's checks and they failed.
pub const EXIT_CHECKS_FAILED: i32 = 7;
/// `pr --wait --timeout` ran out of time before the checks settled.
pub const EXIT_TIMEOUT: i32 = 8;

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
  4  not found: no such colony, loop or token (404)
  5  conflict (409), or `ask`/`answer` on a colony that is not asking anything
  6  a launch cap was refused (429)
  7  `pr --wait` followed the checks and they failed
  8  `pr --wait --timeout` ran out of time before the checks settled

Settings come from the environment, not flags: COLONIZER_BIND, COLONIZER_DATA_DIR,
COLONIZER_HOME and the rest are in docs/install.md. The mothership and the local commands
(`update`, `open`, `login-item`, `telemetry`) read them; the client commands take --host and --token-file.";

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
        /// Work this issue: its title and body go into the prompt. Without it the colony works on TASK
        #[arg(long)]
        issue: Option<u64>,
        /// Run the orchestrator on this model instead of what routing would pick
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        /// Run the colony's subagents on this model
        #[arg(long, value_name = "MODEL")]
        subagent_model: Option<String>,
        /// Open the pull request automatically once the agent finishes cleanly and has written its PR description
        #[arg(long)]
        autopilot: bool,
        /// Keep autopilot off: the finished work waits for you to open the pull request, even where the install default is on
        #[arg(long, conflicts_with = "autopilot")]
        no_autopilot: bool,
        /// Start a colony on an issue another colony already holds, which would otherwise be refused (409)
        #[arg(long)]
        allow_duplicate: bool,
        /// Wait for an issue another colony holds instead of being refused: the colony queues behind the holder and starts when the issue is its own
        #[arg(long)]
        queue_behind_holder: bool,
        /// Start a colony on an epic — an issue with sub-issues, an `epic` label, or a title marking one — which would otherwise be refused (409)
        #[arg(long)]
        allow_epic: bool,
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
    /// Print a colony's pull request URL and state, or --wait for its checks to settle
    Pr {
        id: String,
        /// Follow the pull request's checks until they settle, re-reading the colony, instead of
        /// printing the state once; exits 7 when they fail, 8 when a --timeout runs out
        #[arg(long)]
        wait: bool,
        /// Give up after this long (`--wait` only): `90`, `90s`, `30m`, `2h`. No timeout by default
        #[arg(long, requires = "wait", value_name = "DURATION", value_parser = parse_duration)]
        timeout: Option<Duration>,
    },
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
    /// Manage loops: saved prompts that launch a colony on a schedule — the cockpit's Loops page
    /// from a terminal. `colonizer loop create --help` shows the cadence grammar
    Loop {
        #[command(subcommand)]
        command: LoopCommand,
    },
    /// Start and list red-team runs: hunters that find and report bugs, or security defects with
    /// `--preset security`
    Redteam {
        #[command(subcommand)]
        command: RedteamCommand,
    },
    /// Manage the mothership's scoped API tokens (the owner token only)
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// Export this machine's stats, logs and colony history, or import another machine's (issue #687)
    Fleet {
        #[command(subcommand)]
        command: FleetCommand,
    },
}

/// The `fleet` subcommands. Export and import run locally off the settings — no mothership, no
/// token. #686's join slots a sibling in beside them.
#[derive(Subcommand, Debug)]
enum FleetCommand {
    /// Write a bundle of this machine's stats, logs and colony history for a fleet import
    Export {
        /// Where to write the bundle (default: colonizer-export-<host>-<date>.tar.zst here)
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Leave the colony history out
        #[arg(long)]
        no_history: bool,
        /// Leave the session logs and transcripts out
        #[arg(long)]
        no_logs: bool,
        /// Leave the spend, routing and usage stats out
        #[arg(long)]
        no_stats: bool,
        /// Show what would be exported, and write nothing
        #[arg(long)]
        preview: bool,
    },
    /// Push this member's colony history to its fleet's owner: preview it, consent, drain now, or show where it stands
    Sync {
        /// Show the push's status, and send nothing
        #[arg(long, conflicts_with_all = ["preview", "enable", "disable"])]
        status: bool,
        /// Show what the push would send (colonies, logs, bytes), and send nothing
        #[arg(long, conflicts_with_all = ["enable", "disable"])]
        preview: bool,
        /// Consent to pushing this machine's history to the owner (prints the preview first)
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        /// Withdraw that consent: nothing more is sent
        #[arg(long)]
        disable: bool,
    },
    /// Preview a fleet bundle, then import it into this machine's data dir
    Import {
        /// The .tar.zst bundle to import
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// Show the bundle's manifest, and import nothing
        #[arg(long)]
        preview: bool,
    },
}

/// The `redteam` subcommands.
#[derive(Subcommand, Debug)]
enum RedteamCommand {
    /// Start a run against one repository. It arms by default and launches as soon as no colony
    /// is live; --now starts it immediately or fails with exit 5 while colonies are live
    Start {
        /// The repository to raid, as owner/repo
        #[arg(value_name = "OWNER/REPO")]
        repo: String,
        /// general (bug hunt) or security (security focus areas, a deterministic pre-scan and an
        /// operator checklist)
        #[arg(long, value_enum, default_value_t = RedteamPreset::General)]
        preset: RedteamPreset,
        /// Hunters in the swarm, 1 to 8 (default 3)
        #[arg(long, value_name = "N")]
        hunters: Option<usize>,
        /// Run the hunters' orchestrator on this model
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        /// Run the hunters' subagents on this model
        #[arg(long, value_name = "MODEL")]
        subagent_model: Option<String>,
        /// Let hunters fix what they find (off: they only report, and never open or merge anything)
        #[arg(long)]
        autofix: bool,
        /// Start now instead of arming; refused while any colony is live
        #[arg(long)]
        now: bool,
    },
    /// List red-team runs, newest first
    List,
}

/// A red-team preset, as `--preset` spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RedteamPreset {
    General,
    Security,
}

impl RedteamPreset {
    fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Security => "security",
        }
    }
}

/// The `POST /api/sessions` body `colonizer launch` sends. `autopilot` is `Some` only when a flag
/// chose one, so the publish module's setting decides otherwise; the claim and epic overrides
/// travel as booleans, each off by default, the same fields the cockpit and the API take.
#[allow(clippy::too_many_arguments)]
fn launch_body(
    repo: &str,
    issue: Option<u64>,
    task: Option<String>,
    autopilot: Option<bool>,
    model: Option<String>,
    subagent_model: Option<String>,
    allow_duplicate: bool,
    queue_behind_holder: bool,
    allow_epic: bool,
) -> Value {
    json!({
        "repo": repo,
        "issue": issue,
        "instructions": task.unwrap_or_default(),
        "autopilot": autopilot,
        "model_override": model,
        "subagent_model_override": subagent_model,
        "allow_duplicate": allow_duplicate,
        "queue_behind_holder": queue_behind_holder,
        "allow_epic": allow_epic,
    })
}

/// The `POST /api/redteam/runs` body `redteam start` sends.
fn redteam_start_body(
    repo: &str,
    preset: RedteamPreset,
    hunters: Option<usize>,
    model: Option<String>,
    subagent_model: Option<String>,
    autofix: bool,
    now: bool,
) -> Value {
    json!({
        "repo": repo,
        "preset": preset.as_str(),
        "swarm_size": hunters,
        "model": model,
        "subagent_model": subagent_model,
        "autofix": autofix,
        "arm": !now,
    })
}

async fn redteam_command(cli: &Cli, command: RedteamCommand) -> i32 {
    let json = cli.json;
    client_command(cli, move |machine| async move {
        match command {
            RedteamCommand::Start {
                repo,
                preset,
                hunters,
                model,
                subagent_model,
                autofix,
                now,
            } => {
                let body = redteam_start_body(&repo, preset, hunters, model, subagent_model, autofix, now);
                let run = machine
                    .post("/api/redteam/runs", Some(&body))
                    .await?
                    .ok_or_else(|| Fail::Transport(anyhow::anyhow!("the mothership answered no body")))?;
                if json {
                    println!("{}", pretty(&run)?);
                } else {
                    println!(
                        "red-team run {} on {} ({} preset, {})",
                        run["id"].as_str().unwrap_or("?"),
                        run["repo"].as_str().unwrap_or("?"),
                        run["preset"].as_str().unwrap_or("general"),
                        run["state"].as_str().unwrap_or("?")
                    );
                }
                Ok(EXIT_OK)
            }
            RedteamCommand::List => {
                let runs = machine.get("/api/redteam/runs").await?;
                if json {
                    println!("{}", pretty(&runs)?);
                    return Ok(EXIT_OK);
                }
                let rows = runs.as_array().cloned().unwrap_or_default();
                if rows.is_empty() {
                    eprintln!("no red-team runs");
                    return Ok(EXIT_OK);
                }
                for r in &rows {
                    let c = &r["counts"];
                    let leads = r["prescan"]["leads"].as_array().map(Vec::len);
                    println!(
                        "{:<12}  {:<28}  {:<8}  {:<8}  {} found, {} validated{}",
                        r["id"].as_str().unwrap_or("?"),
                        util::truncate(r["repo"].as_str().unwrap_or("?"), 28),
                        r["preset"].as_str().unwrap_or("general"),
                        r["state"].as_str().unwrap_or("?"),
                        c["found"].as_u64().unwrap_or(0),
                        c["validated"].as_u64().unwrap_or(0),
                        leads.map(|n| format!(", {n} pre-scan leads")).unwrap_or_default()
                    );
                }
                Ok(EXIT_OK)
            }
        }
    })
    .await
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

#[derive(Subcommand, Debug)]
enum LoopCommand {
    /// List every loop this token may see: id, name, repository, cadence, state and next run, in your local time.
    List,
    /// Create a loop: a saved prompt on a repository that launches a colony on a schedule
    ///
    /// The cadence takes `30m`/`2h`, `7d`, `14d@03:00`, `daily@09:00`, `weekly@mon@09:00`,
    /// `monthly@15@09:00` or `self`; clock times are your local time, stored UTC. `--kind map`
    /// refreshes the architecture map instead of running the prompt.
    Create {
        /// The repository to run on, as owner/repo; owner/* for a map loop covers every repository of the org
        #[arg(value_name = "OWNER/REPO")]
        repo: String,
        /// When it runs — see above for the grammar
        #[arg(value_name = "CADENCE")]
        cadence: String,
        /// A name that says what it is for ("Triage new issues"); the lists show it
        #[arg(long)]
        name: String,
        /// What each run should do
        #[arg(long)]
        prompt: Option<String>,
        /// Read the prompt from a file (`-` reads stdin) instead of --prompt
        #[arg(long, value_name = "PATH")]
        prompt_file: Option<PathBuf>,
        /// What a run does: colony (the prompt) or map (refresh the architecture map; the prompt is not used)
        #[arg(long, value_enum, default_value_t = LoopKindArg::Colony)]
        kind: LoopKindArg,
        /// Run each colony's orchestrator on this model instead of what routing would pick
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        /// Run the colonies' subagents on this model
        #[arg(long, value_name = "MODEL")]
        subagent_model: Option<String>,
        /// Open the pull request automatically once a run finishes cleanly and has written its PR description
        #[arg(long)]
        autopilot: bool,
        /// Keep autopilot off: a finished run waits for you to open the pull request
        #[arg(long, conflicts_with = "autopilot")]
        no_autopilot: bool,
        /// End the loop after this many runs
        #[arg(long)]
        max_runs: Option<u32>,
        /// Create it paused: nothing runs until `loop start`
        #[arg(long)]
        disabled: bool,
    },
    /// Start the loop's next run now, whatever its schedule; while a run is live this is a conflict (exit 5)
    Run {
        id: String,
        /// The built-in disk-cleanup loop only: list what a run would remove, with sizes, and remove nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Pause a loop: its settings are kept, and nothing runs until `loop start` (alias: disable)
    #[command(alias = "disable")]
    Stop { id: String },
    /// Enable a paused or ended loop again; the next run is booked from its cadence (alias: enable).
    /// `colonizer loop enable disk-cleanup` switches on the built-in disk cleanup
    #[command(alias = "enable")]
    Start { id: String },
    /// Delete a loop. Its past colonies stay.
    Delete { id: String },
    /// The built-in merge-train loop (issue #754): off by default, hourly, and only in the
    /// repositories you opt in. `show` prints its settings and last report
    #[command(name = "merge-train")]
    MergeTrain {
        #[command(subcommand)]
        command: MergeTrainCommand,
    },
}

/// `colonizer loop merge-train …`: each edit reads the settings, changes one thing and saves them
/// back — the same full replace the cockpit's form sends.
#[derive(Subcommand, Debug)]
enum MergeTrainCommand {
    /// Settings, the next run, paused repositories and the last run's report
    Show,
    /// Switch the loop on (it runs at its cadence; nothing merges until a repository is opted in)
    On,
    /// Switch the loop off
    Off,
    /// Opt a repository (owner/repo) or a whole org (owner) in
    Allow {
        #[arg(value_name = "OWNER[/REPO]")]
        target: String,
    },
    /// Take a repository or org off the allowlist and the never list
    Disallow {
        #[arg(value_name = "OWNER[/REPO]")]
        target: String,
    },
    /// Never merge in this repository or org (an upstream-review-only fork, say), whatever the allowlist says
    Never {
        #[arg(value_name = "OWNER[/REPO]")]
        target: String,
    },
    /// Hold a colony's pull request out of the loop
    Hold { session: String },
    /// Release a held colony
    Unhold { session: String },
    /// Change the loop's limits and switches; only the flags given change
    Set {
        /// Run every N minutes (15 to 10080)
        #[arg(long, value_name = "MINUTES")]
        every: Option<u32>,
        /// Merges per repository per run
        #[arg(long)]
        max_merges: Option<u32>,
        /// A per-repository cap, as owner/repo=N (repeatable)
        #[arg(long, value_name = "OWNER/REPO=N")]
        repo_cap: Vec<String>,
        /// The least seconds between two merges in one repository
        #[arg(long)]
        cooldown_secs: Option<u64>,
        /// Minutes to wait for an updated pull request's CI
        #[arg(long)]
        ci_wait_minutes: Option<u64>,
        /// Known-flaky check names, comma-separated (a trailing * matches a prefix)
        #[arg(long, value_name = "NAMES")]
        flaky: Option<String>,
        /// Re-run main's failed jobs once, then send a fix colony, when main goes red after the train's merge
        #[arg(long, value_enum)]
        self_heal: Option<Toggle>,
        /// With self-heal: revert the train's own last merge instead of sending a fix colony
        #[arg(long, value_enum)]
        revert_on_red: Option<Toggle>,
        /// Dispatch one redo colony for a pull request whose mechanical rebase conflicted
        #[arg(long, value_enum)]
        redo: Option<Toggle>,
    },
    /// Run it now in the background, or with --dry-run list what it would merge, update, rebase and skip
    Run {
        /// Read everything, write nothing, and print the report
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Toggle {
    On,
    Off,
}

/// What a loop's runs do, as `--kind` spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum LoopKindArg {
    /// A colony working from the prompt (the default)
    Colony,
    /// An architecture-map refresh; the prompt is not used
    Map,
}

impl LoopKindArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Colony => "colony",
            Self::Map => "map",
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

    /// PUT with a JSON body, expecting one back: the loops edit route replaces the whole loop.
    async fn put(&self, path: &str, body: &Value) -> Result<Value, Fail> {
        let response = self
            .http
            .put(self.url(path))
            .bearer_auth(&self.token)
            .json(body)
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

/// `--timeout` as a duration: a bare number is seconds, `s`/`sec`/`seconds` says so, and the
/// minutes, hours and days spellings are `loop create`'s ([`split_duration`]: `30m`, `2h`, `1d`,
/// any case). Zero, a count past `u64`, a time of day (`@`) and any other spelling are refused: a
/// wait that ends when it starts is a typo, not a plan.
fn parse_duration(text: &str) -> Result<Duration, String> {
    let refuse = || format!("--timeout takes a number of seconds, or s/m/h/d after it (got \"{text}\")");
    let text = text.trim();
    if text.contains('@') {
        return Err(refuse());
    }
    let secs = match split_duration(text) {
        Some((count, unit)) => {
            let unit_secs = match unit {
                'm' => 60,
                'h' => 3600,
                _ => 86_400,
            };
            count.checked_mul(unit_secs)
        }
        None => {
            let digits = text.chars().take_while(char::is_ascii_digit).count();
            match text[digits..].trim().to_lowercase().as_str() {
                "" | "s" | "sec" | "secs" | "second" | "seconds" => text[..digits].parse::<u64>().ok(),
                _ => None,
            }
        }
    };
    secs.filter(|secs| *secs > 0).map(Duration::from_secs).ok_or_else(refuse)
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
            allow_duplicate,
            queue_behind_holder,
            allow_epic,
            task,
        } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                // No flag at all: the publish module's autopilot setting decides, server-side.
                let autopilot = if autopilot {
                    Some(true)
                } else if no_autopilot {
                    Some(false)
                } else {
                    None
                };
                let body = launch_body(
                    &repo,
                    issue,
                    task,
                    autopilot,
                    model,
                    subagent_model,
                    allow_duplicate,
                    queue_behind_holder,
                    allow_epic,
                );
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
        Command::Pr { id, wait, timeout } => {
            let json = cli.json;
            client_command(cli, move |machine| async move {
                if wait {
                    return wait_pr(&machine, &id, json, timeout, PR_WAIT_POLL).await;
                }
                let detail = machine.get(&format!("/api/sessions/{id}")).await?;
                print_pr(&id, &detail, json)?;
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
        Command::Loop { command } => loop_command(cli, command).await,
        Command::Redteam { command } => redteam_command(cli, command).await,
        Command::Token { command } => token_command(cli, command).await,
        Command::Fleet {
            command:
                FleetCommand::Sync {
                    status,
                    preview,
                    enable,
                    disable,
                },
        } => {
            let action = match (status, preview, enable, disable) {
                (true, ..) => SyncAction::Status,
                (_, true, ..) => SyncAction::Preview,
                (_, _, true, _) => SyncAction::Consent(true),
                (_, _, _, true) => SyncAction::Consent(false),
                _ => SyncAction::Drain,
            };
            fleet_sync_command(cli, action).await
        }
        Command::Fleet { command } => fleet_command(cli, command),
    }
}

/// What `fleet sync` was asked to do.
#[derive(Clone, Copy)]
enum SyncAction {
    Drain,
    Status,
    Preview,
    Consent(bool),
}

/// Prints `fleet sync --preview`'s answer for a person.
fn print_sync_preview(preview: &Value) {
    let mib = |key: &str| preview[key].as_u64().unwrap_or(0) as f64 / (1024.0 * 1024.0);
    println!(
        "{} finished colonies, {} log files, {:.1} MiB in all would go to {}",
        preview["colonies"].as_u64().unwrap_or(0),
        preview["payloads"].as_u64().unwrap_or(0),
        mib("total_bytes"),
        preview["owner_url"].as_str().unwrap_or("the owner"),
    );
    println!(
        "not yet sent: {} colonies, {:.1} MiB",
        preview["pending_colonies"].as_u64().unwrap_or(0),
        mib("pending_bytes")
    );
    if let Some(what) = preview["includes"].as_str() {
        println!("includes: {what}");
    }
    if let Some(what) = preview["excludes"].as_str() {
        println!("never sent: {what}");
    }
}

/// `fleet sync`: the running mothership's history push to its fleet's owner (issue #762) —
/// preview what it would send, consent or withdraw, drain now, or show where it stands.
async fn fleet_sync_command(cli: &Cli, action: SyncAction) -> i32 {
    let json = cli.json;
    client_command(cli, move |machine| async move {
        let answer = match action {
            SyncAction::Status => machine.get("/api/fleet/sync").await?,
            SyncAction::Preview => machine.get("/api/fleet/sync/preview").await?,
            SyncAction::Consent(enabled) => {
                if enabled && !json {
                    print_sync_preview(&machine.get("/api/fleet/sync/preview").await?);
                }
                let body = json!({ "enabled": enabled });
                machine
                    .post("/api/fleet/sync/consent", Some(&body))
                    .await?
                    .unwrap_or(Value::Null)
            }
            SyncAction::Drain => machine.post("/api/fleet/sync", None).await?.unwrap_or(Value::Null),
        };
        if json {
            println!("{}", pretty(&answer)?);
            return Ok(EXIT_OK);
        }
        let status = answer["status"].as_str().unwrap_or("?");
        match action {
            SyncAction::Preview => print_sync_preview(&answer),
            SyncAction::Status | SyncAction::Consent(_) => println!(
                "{status}: history sync {}; {} rows acknowledged, {} retired",
                if answer["consent"].as_bool() == Some(true) {
                    "on"
                } else {
                    "off"
                },
                answer["acknowledged"].as_u64().unwrap_or(0),
                answer["retired"].as_array().map_or(0, Vec::len)
            ),
            SyncAction::Drain => println!(
                "{status}: sent {} rows and {} payloads; {} pending, {} retired",
                answer["sent"].as_u64().unwrap_or(0),
                answer["payloads"].as_u64().unwrap_or(0),
                answer["pending"].as_u64().unwrap_or(0),
                answer["retired"].as_array().map_or(0, Vec::len)
            ),
        }
        if !matches!(action, SyncAction::Preview)
            && let Some(detail) = answer["detail"].as_str()
        {
            eprintln!("{detail}");
        }
        Ok(EXIT_OK)
    })
    .await
}

/// `fleet export` and `fleet import` run on this machine's own data dir (`Settings::from_env`),
/// locally: no mothership to reach and no token to present (issue #687).
fn fleet_command(cli: &Cli, command: FleetCommand) -> i32 {
    let cfg = match Settings::from_env() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("colonizer: {e:#}");
            return EXIT_ERROR;
        }
    };
    let result = match command {
        FleetCommand::Export {
            out,
            no_history,
            no_logs,
            no_stats,
            preview,
        } => {
            let cats = crate::fleet_export::Categories {
                history: !no_history,
                logs: !no_logs,
                stats: !no_stats,
            };
            crate::fleet_export::cli_export(&cfg, cli.json, out, cats, preview)
        }
        FleetCommand::Import { file, preview } => crate::fleet_export::cli_import(&cfg, &file, preview, cli.json),
        FleetCommand::Sync { .. } => unreachable!("`fleet sync` talks to the mothership: fleet_sync_command"),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("colonizer: {e:#}");
            EXIT_ERROR
        }
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

/// How often `pr --wait` re-reads the colony. The mothership re-checks GitHub itself about once a
/// minute while checks run, so reading it faster buys nothing.
const PR_WAIT_POLL: Duration = Duration::from_secs(15);

/// The `pr` answer, human or `--json`: the URL with the colony's state and checks, or the words
/// that there is no pull request yet.
fn print_pr(id: &str, detail: &Value, json: bool) -> Result<()> {
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
        return Ok(());
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
    Ok(())
}

/// The body of `pr --wait`: re-read the colony until its checks settle — 0 on success or when
/// there is nothing to wait for (`no_checks`), 7 when they fail — or until the colony ends without
/// putting a verdict in front of us (1: no pull request, or one merged or closed untested).
/// `timeout` ends the wait with 8 instead, and `interval` is a parameter only so the tests can
/// wait in milliseconds.
async fn wait_pr(machine: &Machine, id: &str, json: bool, timeout: Option<Duration>, interval: Duration) -> Result<i32, Fail> {
    // When to stop, and the value to name when saying so.
    // A timeout too long to land on the clock (`--timeout 5000000000000000h`) is no deadline at all.
    let give_up = timeout.and_then(|t| tokio::time::Instant::now().checked_add(t).map(|at| (at, t)));
    loop {
        let detail = machine.get(&format!("/api/sessions/{id}")).await?;
        let checks = detail["ci_state"].as_str().unwrap_or("unknown");
        let status = detail["status"].as_str().unwrap_or("unknown");
        match detail["pr_url"].as_str() {
            // A settled verdict — or none to wait for — is the ending the wait was for.
            Some(_) if matches!(checks, "success" | "failure" | "no_checks") => {
                print_pr(id, &detail, json)?;
                return Ok(if checks == "failure" { EXIT_CHECKS_FAILED } else { EXIT_OK });
            }
            // The work is out and the verdict never came: the pull request moved on under us.
            Some(_) if matches!(status, "merged" | "closed") => {
                eprintln!("the pull request was {status} before its checks settled");
                return Ok(EXIT_ERROR);
            }
            // Still no pull request: a parked colony is paused, not over, but nothing will
            // publish until someone resumes it, so waiting would hang regardless.
            None if status == "parked" => {
                eprintln!("colony {id} is parked; nothing will publish until it is resumed");
                return Ok(EXIT_ERROR);
            }
            None if !pr_still_coming(status) => {
                eprintln!("colony {id} ended without opening a pull request (status {status})");
                return Ok(EXIT_ERROR);
            }
            _ => {}
        }
        let now = tokio::time::Instant::now();
        if let Some((deadline, after)) = give_up
            && now >= deadline
        {
            eprintln!("colony {id}: checks still {checks} after {after:?}; gave up waiting");
            return Ok(EXIT_TIMEOUT);
        }
        // Sleep to the next poll, but never past the deadline: a short --timeout must not sit out a
        // whole poll interval just to notice it is over.
        let wake = match give_up {
            Some((deadline, _)) => (now + interval).min(deadline),
            None => now + interval,
        };
        tokio::time::sleep(wake - now).await;
    }
}

/// Whether a colony with no pull request yet may still open one — the statuses a `--wait` keeps
/// waiting through. The refused set is `SessionStatus::is_terminal`'s spellings plus `parked`.
fn pr_still_coming(status: &str) -> bool {
    !matches!(
        status,
        "pr_opened" | "merged" | "closed" | "no_changes" | "stopped" | "failed" | "parked"
    )
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

// ---------------------------------------------------------------------------
// Loops: the cockpit's Loops page, driven from a terminal.
// ---------------------------------------------------------------------------

async fn loop_command(cli: &Cli, command: LoopCommand) -> i32 {
    let json = cli.json;
    client_command(cli, move |machine| async move {
        match command {
            LoopCommand::List => {
                let loops = machine.get("/api/loops").await?;
                if json {
                    println!("{}", pretty(&loops)?);
                    return Ok(EXIT_OK);
                }
                let rows = loops.as_array().cloned().unwrap_or_default();
                // A person sees a word, a script sees an empty stdout and a 0.
                if rows.is_empty() {
                    eprintln!("no loops");
                    return Ok(EXIT_OK);
                }
                let offset = local_offset_minutes();
                for l in &rows {
                    let state = if l["enabled"] == json!(true) {
                        "enabled"
                    } else if l["ended_reason"].is_string() {
                        "ended"
                    } else {
                        "paused"
                    };
                    let next = l["next_run_at"]
                        .as_str()
                        .map(|iso| local_stamp(iso, offset))
                        .unwrap_or_else(|| "—".into());
                    // The built-in disk cleanup runs on this host, not on a repository.
                    let scope = match l["repo"].as_str() {
                        Some("") if l["kind"] == json!("disk_cleanup") => "(this host)",
                        Some(repo) => repo,
                        None => "?",
                    };
                    println!(
                        "{:<12}  {:<26}  {:<22}  {:<32}  {:<8}  {}",
                        l["id"].as_str().unwrap_or("?"),
                        util::truncate(l["name"].as_str().unwrap_or("?"), 26),
                        scope,
                        describe_cadence(&l["cadence"], offset),
                        state,
                        next
                    );
                }
                Ok(EXIT_OK)
            }
            LoopCommand::Create {
                repo,
                cadence,
                name,
                prompt,
                prompt_file,
                kind,
                model,
                subagent_model,
                autopilot,
                no_autopilot,
                max_runs,
                disabled,
            } => {
                let offset = local_offset_minutes();
                let cadence = parse_cadence(&cadence, offset).map_err(anyhow::Error::msg)?;
                let prompt = match (prompt, prompt_file) {
                    (Some(text), None) => text,
                    (None, Some(path)) => read_prompt(&path)?,
                    (Some(_), Some(_)) => {
                        return Err(Fail::Transport(anyhow::anyhow!("use --prompt or --prompt-file, not both")));
                    }
                    (None, None) => String::new(),
                };
                if kind != LoopKindArg::Map && prompt.trim().is_empty() {
                    return Err(Fail::Transport(anyhow::anyhow!(
                        "a colony loop needs a prompt (--prompt TEXT or --prompt-file PATH); a map loop needs neither"
                    )));
                }
                if kind == LoopKindArg::Map && !prompt.trim().is_empty() {
                    eprintln!("note: a map loop ignores the prompt; it refreshes the repository's map");
                }
                let body = json!({
                    "name": name,
                    "repo": repo,
                    "prompt": prompt,
                    "cadence": cadence,
                    "kind": kind.as_str(),
                    // This machine's offset now, the field the cockpit sends with its own saves, so
                    // the loop's clock times can be shown in the operator's local time.
                    "tz_offset_minutes": offset,
                    "model": model,
                    "subagent_model": subagent_model,
                    // No flag at all: the server's default (on) decides.
                    "autopilot": if autopilot { Some(true) } else if no_autopilot { Some(false) } else { None },
                    "max_runs": max_runs,
                    "enabled": if disabled { Some(false) } else { None },
                });
                let created = machine
                    .post("/api/loops", Some(&body))
                    .await?
                    .ok_or_else(|| Fail::Transport(anyhow::anyhow!("the mothership answered no body")))?;
                if json {
                    println!("{}", pretty(&created)?);
                } else {
                    let when = created["next_run_at"]
                        .as_str()
                        .map(|iso| format!("next run {}", local_stamp(iso, offset)))
                        .unwrap_or_else(|| "paused".into());
                    println!(
                        "loop {} created (\"{}\" on {}, {when})",
                        created["id"].as_str().unwrap_or("?"),
                        created["name"].as_str().unwrap_or("?"),
                        created["repo"].as_str().unwrap_or("?")
                    );
                }
                Ok(EXIT_OK)
            }
            LoopCommand::Run { id, dry_run } => {
                let path = if dry_run {
                    format!("/api/loops/{id}/run-now?dry_run=1")
                } else {
                    format!("/api/loops/{id}/run-now")
                };
                let session = machine
                    .post(&path, None)
                    .await?
                    .ok_or_else(|| Fail::Transport(anyhow::anyhow!("the mothership answered no body")))?;
                if json {
                    println!("{}", pretty(&session)?);
                } else if session["categories"].is_array() {
                    print_cleanup_report(&session);
                } else {
                    println!(
                        "colony {} started for loop {id} ({})",
                        session["id"].as_str().unwrap_or("?"),
                        session["status"].as_str().unwrap_or("queued")
                    );
                }
                Ok(EXIT_OK)
            }
            LoopCommand::MergeTrain { command } => merge_train_command(&machine, command, json).await,
            LoopCommand::Stop { id } => set_loop_enabled(&machine, &id, false, json).await,
            LoopCommand::Start { id } => set_loop_enabled(&machine, &id, true, json).await,
            LoopCommand::Delete { id } => {
                let deleted = machine.delete(&format!("/api/loops/{id}")).await?;
                if json {
                    println!("{}", pretty(&deleted)?);
                } else {
                    println!("loop {id} deleted; its past colonies stay");
                }
                Ok(EXIT_OK)
            }
        }
    })
    .await
}

/// A disk-cleanup run (or dry run) for a person: one line per category with what went or would
/// go, then the paths, then anything held back and why.
fn print_cleanup_report(report: &Value) {
    let dry = report["dry_run"] == json!(true);
    let size = |v: &Value| v.as_u64().map(util::format_disk_size).unwrap_or_else(|| "?".into());
    println!("{} {}", if dry { "would free" } else { "freed" }, size(&report["bytes"]));
    for c in report["categories"].as_array().cloned().unwrap_or_default() {
        let name = c["category"].as_str().unwrap_or("?").replace('_', " ");
        if c["enabled"] != json!(true) {
            println!("  {name}: off");
            continue;
        }
        println!(
            "  {name}: {} item(s), {}",
            c["count"].as_u64().unwrap_or(0),
            size(&c["bytes"])
        );
        for item in c["items"].as_array().cloned().unwrap_or_default() {
            println!("    {}  {}", size(&item["bytes"]), item["path"].as_str().unwrap_or("?"));
        }
        for held in c["held"].as_array().cloned().unwrap_or_default() {
            println!(
                "    kept  {} ({})",
                held["path"].as_str().unwrap_or("?"),
                held["reason"].as_str().unwrap_or("?")
            );
        }
        if let Some(note) = c["note"].as_str() {
            println!("    note: {note}");
        }
    }
    if let Some(attention) = report["attention"].as_str() {
        println!("{attention}");
    }
}

const MERGE_LOOP: &str = "/api/merge-train/loop";

/// Applies one `merge-train` edit to the settings the server holds. Pure, for the tests.
fn edit_merge_loop(settings: &mut Value, command: &MergeTrainCommand) -> Result<(), String> {
    let list = |settings: &mut Value, key: &str| -> Vec<String> {
        settings[key]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let norm = |t: &str| t.trim().to_ascii_lowercase();
    match command {
        MergeTrainCommand::On => settings["enabled"] = json!(true),
        MergeTrainCommand::Off => settings["enabled"] = json!(false),
        MergeTrainCommand::Allow { target } => {
            let mut allow = list(settings, "allow");
            if !allow.contains(&norm(target)) {
                allow.push(norm(target));
            }
            let never: Vec<String> = list(settings, "never").into_iter().filter(|t| *t != norm(target)).collect();
            settings["allow"] = json!(allow);
            settings["never"] = json!(never);
        }
        MergeTrainCommand::Disallow { target } => {
            for key in ["allow", "never"] {
                let kept: Vec<String> = list(settings, key).into_iter().filter(|t| *t != norm(target)).collect();
                settings[key] = json!(kept);
            }
        }
        MergeTrainCommand::Never { target } => {
            let mut never = list(settings, "never");
            if !never.contains(&norm(target)) {
                never.push(norm(target));
            }
            settings["never"] = json!(never);
        }
        MergeTrainCommand::Hold { session } => {
            let mut held = list(settings, "held");
            if !held.contains(session) {
                held.push(session.clone());
            }
            settings["held"] = json!(held);
        }
        MergeTrainCommand::Unhold { session } => {
            let held: Vec<String> = list(settings, "held").into_iter().filter(|h| h != session).collect();
            settings["held"] = json!(held);
        }
        MergeTrainCommand::Set {
            every,
            max_merges,
            repo_cap,
            cooldown_secs,
            ci_wait_minutes,
            flaky,
            self_heal,
            revert_on_red,
            redo,
        } => {
            if let Some(minutes) = every {
                settings["cadence"] = json!({"every": "interval", "minutes": minutes});
            }
            if let Some(n) = max_merges {
                settings["max_merges"] = json!(n);
            }
            for entry in repo_cap {
                let (repo, n) = entry
                    .split_once('=')
                    .ok_or_else(|| format!("--repo-cap {entry:?} is not owner/repo=N"))?;
                let n: u32 = n
                    .trim()
                    .parse()
                    .map_err(|_| format!("--repo-cap {entry:?}: N is not a number"))?;
                if !settings["repo_max_merges"].is_object() {
                    settings["repo_max_merges"] = json!({});
                }
                settings["repo_max_merges"][norm(repo)] = json!(n);
            }
            if let Some(secs) = cooldown_secs {
                settings["cooldown_secs"] = json!(secs);
            }
            if let Some(minutes) = ci_wait_minutes {
                settings["ci_wait_minutes"] = json!(minutes);
            }
            if let Some(names) = flaky {
                let names: Vec<&str> = names.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
                settings["flaky_checks"] = json!(names);
            }
            for (key, toggle) in [
                ("self_heal", self_heal),
                ("revert_on_red", revert_on_red),
                ("redo_on_conflict", redo),
            ] {
                if let Some(t) = toggle {
                    settings[key] = json!(*t == Toggle::On);
                }
            }
        }
        MergeTrainCommand::Show | MergeTrainCommand::Run { .. } => {}
    }
    Ok(())
}

/// The loop's state in lines: its switch, where it merges, its limits, and the last report.
fn describe_merge_loop(view: &Value) -> Vec<String> {
    let s = &view["settings"];
    let names = |key: &str| {
        let list: Vec<&str> = s[key]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if list.is_empty() {
            "none".to_string()
        } else {
            list.join(", ")
        }
    };
    let on = |key: &str| if s[key] == json!(true) { "on" } else { "off" };
    let mut out = vec![
        format!(
            "merge-train loop: {}{}",
            if s["enabled"] == json!(true) { "on" } else { "off" },
            view["next_run_at"]
                .as_str()
                .map(|at| format!(", next run {}", local_stamp(at, local_offset_minutes())))
                .unwrap_or_default()
        ),
        format!("  cadence: {}", describe_cadence(&s["cadence"], local_offset_minutes())),
        format!("  opted in: {}", names("allow")),
        format!("  never: {}", names("never")),
        format!(
            "  per run: at most {} merges per repository, {}s between merges, {} min wait for CI",
            s["max_merges"], s["cooldown_secs"], s["ci_wait_minutes"]
        ),
        format!("  known-flaky checks: {}", names("flaky_checks")),
        format!(
            "  self-heal: {}, revert on red: {}, redo colonies: {}",
            on("self_heal"),
            on("revert_on_red"),
            on("redo_on_conflict")
        ),
    ];
    if view["writes_blocked"] == json!(true) {
        out.push("  external writes are blocked: every run is a dry run".to_string());
    }
    if let Some(repos) = view["repos"].as_object() {
        for (repo, mem) in repos {
            if let Some(why) = mem["paused"].as_str() {
                out.push(format!("  paused in {repo}: {why}"));
            }
        }
    }
    if let Some(lines) = view["last_report"]["lines"].as_array() {
        out.push(format!(
            "last run{}:",
            view["last_report"]["finished_at"]
                .as_str()
                .map(|at| format!(" ({})", local_stamp(at, local_offset_minutes())))
                .unwrap_or_default()
        ));
        out.extend(lines.iter().filter_map(Value::as_str).map(|l| format!("  {l}")));
    }
    out
}

async fn merge_train_command(machine: &Machine, command: MergeTrainCommand, json: bool) -> Result<i32, Fail> {
    match &command {
        MergeTrainCommand::Show => {
            let view = machine.get(MERGE_LOOP).await?;
            if json {
                println!("{}", pretty(&view)?);
            } else {
                for line in describe_merge_loop(&view) {
                    println!("{line}");
                }
            }
            Ok(EXIT_OK)
        }
        MergeTrainCommand::Run { dry_run } => {
            let path = if *dry_run {
                format!("{MERGE_LOOP}/run?dry_run=true")
            } else {
                format!("{MERGE_LOOP}/run")
            };
            let answer = machine
                .post(&path, None)
                .await?
                .ok_or_else(|| Fail::Transport(anyhow::anyhow!("the mothership answered no body")))?;
            if json {
                println!("{}", pretty(&answer)?);
            } else if answer["started"] == json!(true) {
                println!("merge-train loop run started; `colonizer loop merge-train show` prints its report when it is done");
            } else {
                for line in answer["report"]["lines"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    println!("{line}");
                }
            }
            Ok(EXIT_OK)
        }
        edit => {
            let view = machine.get(MERGE_LOOP).await?;
            let mut settings = view["settings"].clone();
            edit_merge_loop(&mut settings, edit).map_err(|e| Fail::Transport(anyhow::anyhow!(e)))?;
            let saved = machine.put(MERGE_LOOP, &settings).await?;
            if json {
                println!("{}", pretty(&saved)?);
            } else {
                for line in describe_merge_loop(&saved).into_iter().take(7) {
                    println!("{line}");
                }
            }
            Ok(EXIT_OK)
        }
    }
}

/// `loop stop`/`loop start`: the loop's own fields PUT back with `enabled` flipped — the exact edit
/// the cockpit's switch makes, since the route replaces the whole loop. The id is found in the list
/// first, so an unknown one reads as the 404 (exit 4) it is.
async fn set_loop_enabled(machine: &Machine, id: &str, enabled: bool, json: bool) -> Result<i32, Fail> {
    let loops = machine.get("/api/loops").await?;
    let l = loops
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|l| l["id"].as_str() == Some(id))
        .ok_or_else(|| Fail::Server {
            status: 404,
            message: format!("no such loop: {id}"),
        })?;
    let body = json!({
        "name": l["name"],
        "repo": l["repo"],
        "prompt": l["prompt"],
        "cadence": l["cadence"],
        "kind": l["kind"],
        "tz_offset_minutes": l["tz_offset_minutes"],
        "model": l["model"],
        "subagent_model": l["subagent_model"],
        "autopilot": l["autopilot"],
        "max_runs": l["max_runs"],
        "end_at": l["end_at"],
        "enabled": enabled,
    });
    let updated = machine.put(&format!("/api/loops/{id}"), &body).await?;
    if json {
        println!("{}", pretty(&updated)?);
        return Ok(EXIT_OK);
    }
    // The update can end the loop again (its run limit, say), so say what the server now has.
    let state = if updated["enabled"] == json!(true) {
        match updated["next_run_at"].as_str() {
            Some(at) => format!("enabled; next run {}", local_stamp(at, local_offset_minutes())),
            None => "enabled".to_string(),
        }
    } else {
        match updated["ended_reason"].as_str() {
            Some(reason) => format!("ended: {reason}"),
            None => "paused".to_string(),
        }
    };
    println!("loop {id} {state}");
    Ok(EXIT_OK)
}

/// The prompt from `--prompt-file`: a path, or `-` for stdin.
fn read_prompt(path: &std::path::Path) -> Result<String, Fail> {
    if path.as_os_str() == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| Fail::Transport(anyhow::anyhow!("could not read the prompt from stdin: {e}")))?;
        return Ok(text);
    }
    std::fs::read_to_string(path).map_err(|e| Fail::Transport(anyhow::anyhow!("could not read {}: {e}", path.display())))
}

/// Weekday names, Monday first — the stored `weekday` counts from Monday too.
const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// This machine's current UTC offset in minutes, the field the cockpit sends with a loop so its
/// clock times can be shown in the operator's local time.
fn local_offset_minutes() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

/// `hour:minute` shifted by a zone offset: the shifted `(hour, minute)` and how many whole days the
/// shift turned the clock (a UTC 23:00 read at UTC+2 is a `+1`). Pure, so tests fix the zone.
fn shifted_time(hour: u32, minute: u32, offset_minutes: i32) -> (u32, u32, i32) {
    let total = hour as i32 * 60 + minute as i32 + offset_minutes;
    let minutes = total.rem_euclid(24 * 60);
    ((minutes / 60) as u32, (minutes % 60) as u32, total.div_euclid(24 * 60))
}

/// The cadence `loop create` takes, as the stored JSON it becomes: `30m`/`2h` (an interval in
/// minutes), `1d`–`7d` (whole days, still an interval, like the composer's `/loop`), `14d@03:00`
/// (every N days at a local time of day), `daily@HH:MM`, `weekly@mon…sun|0…6@HH:MM`,
/// `monthly@1…31@HH:MM` or `self`/`self-paced`. Clock times arrive local and are stored UTC
/// (`offset_minutes` negated here). Only the shape is checked — ranges stay the server's
/// `Cadence::check`, whose error surfaces verbatim. Pure, so tests fix the zone.
fn parse_cadence(spec: &str, offset_minutes: i32) -> Result<Value, String> {
    let grammar = "try 30m, 2h, 7d, 14d@03:00, daily@09:00, weekly@mon@09:00, monthly@15@09:00 or self";
    let time = |text: &str| -> Result<Value, String> {
        let Some((h, m)) = text.split_once(':') else {
            return Err(format!("\"{text}\" is not a time of day (HH:MM)"));
        };
        match (h.trim().parse::<u32>(), m.trim().parse::<u32>()) {
            (Ok(h), Ok(m)) if h <= 23 && m <= 59 => {
                let (h, m, _) = shifted_time(h, m, -offset_minutes);
                Ok(json!({ "hour": h, "minute": m }))
            }
            _ => Err(format!("\"{text}\" is not a time of day (HH:MM, 00:00 to 23:59)")),
        }
    };
    let weekday = |text: &str| -> Result<u32, String> {
        let names = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
        let t = text.trim().to_lowercase();
        if let Some(i) = names.iter().position(|n| t.starts_with(n)) {
            return Ok(i as u32);
        }
        t.parse::<u32>()
            .map_err(|_| format!("\"{text}\" is not a weekday (mon to sun, or 0 to 6 with Monday 0)"))
    };
    let spec = spec.trim();
    // A duration (`30m`, `2h`, `14d@03:00`) or a keyword (`daily@09:00`, `self`).
    if let Some((count, unit)) = split_duration(spec) {
        return match unit {
            'm' | 'h' => {
                if spec.contains('@') {
                    return Err(format!("\"{spec}\" is not a cadence: {grammar}"));
                }
                let minutes = count.saturating_mul(if unit == 'h' { 60 } else { 1 });
                Ok(json!({ "every": "interval", "minutes": minutes }))
            }
            // An anchored every-N-days cadence, whatever the count; up to a week a bare `Nd` stays
            // an interval, and past one the time of day is what makes it anchored instead.
            'd' => match spec.split_once('@') {
                Some((_, t)) => {
                    let at = time(t.trim())?;
                    Ok(json!({ "every": "every_days", "days": count, "hour": at["hour"], "minute": at["minute"] }))
                }
                None if count <= 7 => Ok(json!({ "every": "interval", "minutes": count.saturating_mul(24 * 60) })),
                None => Err(format!("{count}d is past a week; name the time of day: {count}d@09:00")),
            },
            _ => Err(format!("\"{spec}\" is not a cadence: {grammar}")),
        };
    }
    let (word, rest) = match spec.split_once('@') {
        Some((w, r)) => (w.trim().to_lowercase(), Some(r.trim())),
        None => (spec.to_lowercase(), None),
    };
    match (word.as_str(), rest) {
        ("self" | "self-paced" | "self_paced", None) => Ok(json!({ "every": "self_paced" })),
        ("daily", Some(t)) => {
            let at = time(t)?;
            Ok(json!({ "every": "daily", "hour": at["hour"], "minute": at["minute"] }))
        }
        ("weekly", Some(t)) => {
            let Some((day, t)) = t.split_once('@') else {
                return Err("weekly needs a day and a time: weekly@mon@09:00".to_string());
            };
            let at = time(t)?;
            Ok(json!({ "every": "weekly", "weekday": weekday(day)?, "hour": at["hour"], "minute": at["minute"] }))
        }
        ("monthly", Some(t)) => {
            let Some((day, t)) = t.split_once('@') else {
                return Err("monthly needs a day and a time: monthly@15@09:00".to_string());
            };
            let at = time(t)?;
            let day = day
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("\"{}\" is not a day of the month", day.trim()))?;
            Ok(json!({ "every": "monthly", "day": day, "hour": at["hour"], "minute": at["minute"] }))
        }
        _ => Err(format!("\"{spec}\" is not a cadence: {grammar}")),
    }
}

/// `30m` → `(30, 'm')`, `14d` (also `14d@03:00`) → `(14, 'd')`: a count and one of the composer's
/// unit spellings (m/min/minute(s), h/hr/hour(s), d/day(s)), case-insensitive. `None` for anything
/// else, so `daily@09:00` falls through to the keyword forms. The count fits `u64`; range checks
/// are the server's.
fn split_duration(spec: &str) -> Option<(u64, char)> {
    let base = spec.split('@').next()?;
    let digits: String = base.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let count = digits.parse().ok()?;
    match base[digits.len()..].trim().to_lowercase().as_str() {
        "m" | "min" | "mins" | "minute" | "minutes" => Some((count, 'm')),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some((count, 'h')),
        "d" | "day" | "days" => Some((count, 'd')),
        _ => None,
    }
}

/// A stored cadence in words for `loop list`, its clock times moved into the viewer's local time,
/// the way the cockpit's Loops page shows them.
fn describe_cadence(cadence: &Value, offset_minutes: i32) -> String {
    let field = |name: &str| cadence[name].as_u64().unwrap_or(0) as u32;
    let at = |h: u32, m: u32| {
        let (h, m, _) = shifted_time(h, m, offset_minutes);
        format!("{h:02}:{m:02}")
    };
    match cadence["every"].as_str() {
        Some("interval") => {
            let minutes = cadence["minutes"].as_u64().unwrap_or(0);
            let (n, unit) = if minutes > 0 && minutes.is_multiple_of(24 * 60) {
                (minutes / (24 * 60), "day")
            } else if minutes > 0 && minutes.is_multiple_of(60) {
                (minutes / 60, "hour")
            } else {
                (minutes, "minute")
            };
            format!("every {n} {unit}{}", if n == 1 { "" } else { "s" })
        }
        Some("daily") => format!("every day at {}", at(field("hour"), field("minute"))),
        Some("weekly") => {
            let (h, m, days) = shifted_time(field("hour"), field("minute"), offset_minutes);
            let weekday = (field("weekday") as i32 + days).rem_euclid(7) as usize;
            format!("every {} at {h:02}:{m:02}", WEEKDAYS[weekday])
        }
        Some("monthly") => format!(
            "every month on day {} at {}",
            field("day"),
            at(field("hour"), field("minute"))
        ),
        Some("every_days") => format!("every {} days at {}", field("days"), at(field("hour"), field("minute"))),
        Some("self_paced") => "self-paced".to_string(),
        _ => format!("{cadence}"),
    }
}

/// A stored UTC timestamp as a short local `%Y-%m-%d %H:%M`, for the list's and a run's next-run
/// note. A timestamp this build cannot read prints as itself.
fn local_stamp(iso: &str, offset_minutes: i32) -> String {
    let zone = FixedOffset::east_opt(offset_minutes * 60);
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .zip(zone)
        .map(|(at, zone)| at.with_timezone(&zone).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| iso.to_string())
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
            &["pr", "abc123", "--wait"][..],
            &["pr", "abc123", "--wait", "--timeout", "30m"][..],
            &["map", "acme/app"][..],
            &["map", "acme/app", "--find", "login"][..],
            &["loop", "list"][..],
            &[
                "loop",
                "create",
                "acme/app",
                "--name",
                "Triage",
                "--prompt",
                "Triage new issues",
                "daily@09:00",
            ][..],
            &[
                "loop",
                "create",
                "acme/app",
                "--name",
                "Maps",
                "--kind",
                "map",
                "--max-runs",
                "6",
                "14d@03:00",
            ][..],
            &["loop", "run", "loop_x1"][..],
            &["loop", "stop", "loop_x1"][..],
            &["loop", "start", "loop_x1"][..],
            &["loop", "delete", "loop_x1"][..],
            &["loop", "merge-train", "show"][..],
            &["loop", "merge-train", "on"][..],
            &["loop", "merge-train", "allow", "acme/app"][..],
            &["loop", "merge-train", "never", "acme/upstream-fork"][..],
            &["loop", "merge-train", "hold", "abc123"][..],
            &[
                "loop",
                "merge-train",
                "set",
                "--every",
                "120",
                "--max-merges",
                "2",
                "--repo-cap",
                "acme/app=1",
                "--flaky",
                "e2e*,lint",
                "--self-heal",
                "on",
            ][..],
            &["loop", "merge-train", "run", "--dry-run"][..],
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
            &["redteam", "list"][..],
            &["redteam", "start", "acme/app"][..],
            &[
                "redteam",
                "start",
                "acme/app",
                "--preset",
                "security",
                "--hunters",
                "8",
                "--now",
            ][..],
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
            ..
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

    /// The claim and epic overrides arrive as the API body wants them, off unless a flag asked.
    #[test]
    fn a_launch_with_the_claim_and_epic_overrides_sends_them_in_the_body() {
        let cli = parse(&[
            "launch",
            "owner/repo",
            "--allow-duplicate",
            "--queue-behind-holder",
            "--allow-epic",
        ])
        .unwrap();
        let Command::Launch {
            repo,
            issue,
            task,
            model,
            subagent_model,
            allow_duplicate,
            queue_behind_holder,
            allow_epic,
            ..
        } = cli.command.unwrap()
        else {
            panic!("launch did not parse");
        };
        assert!(allow_duplicate && queue_behind_holder && allow_epic);
        let body = launch_body(
            &repo,
            issue,
            task,
            None,
            model,
            subagent_model,
            allow_duplicate,
            queue_behind_holder,
            allow_epic,
        );
        assert_eq!(body["allow_duplicate"], json!(true));
        assert_eq!(body["queue_behind_holder"], json!(true));
        assert_eq!(body["allow_epic"], json!(true));
        // Set: the body deserializes into the server's request type with the overrides on.
        let parsed: crate::sessions::NewSession =
            serde_json::from_value(body.clone()).expect("the launch body deserializes into NewSession");
        assert!(parsed.allow_duplicate && parsed.queue_behind_holder && parsed.allow_epic);

        // No flags: the body still carries the fields, each off, and the server's defaults decide.
        let plain = parse(&["launch", "owner/repo"]).unwrap();
        let Command::Launch {
            repo,
            issue,
            task,
            model,
            subagent_model,
            allow_duplicate,
            queue_behind_holder,
            allow_epic,
            ..
        } = plain.command.unwrap()
        else {
            panic!("launch did not parse");
        };
        let body = launch_body(
            &repo,
            issue,
            task,
            None,
            model,
            subagent_model,
            allow_duplicate,
            queue_behind_holder,
            allow_epic,
        );
        assert_eq!(body["allow_duplicate"], json!(false));
        assert_eq!(body["queue_behind_holder"], json!(false));
        assert_eq!(body["allow_epic"], json!(false));
        // Omitted: the body still deserializes into the server's request type, each override off.
        let parsed: crate::sessions::NewSession =
            serde_json::from_value(body).expect("a plain launch body deserializes into NewSession");
        assert!(!parsed.allow_duplicate && !parsed.queue_behind_holder && !parsed.allow_epic);
    }

    /// `redteam start` sends the preset and the rest as the API wants them: armed unless --now,
    /// general unless named, and an unknown preset is a usage error.
    #[test]
    fn redteam_start_sends_the_preset_field() {
        let cli = parse(&[
            "redteam",
            "start",
            "acme/app",
            "--preset",
            "security",
            "--hunters",
            "4",
            "--autofix",
        ])
        .unwrap();
        let Some(Command::Redteam {
            command:
                RedteamCommand::Start {
                    repo,
                    preset,
                    hunters,
                    model,
                    subagent_model,
                    autofix,
                    now,
                },
        }) = cli.command
        else {
            panic!("redteam start did not parse");
        };
        let body = redteam_start_body(&repo, preset, hunters, model, subagent_model, autofix, now);
        assert_eq!(body["repo"], json!("acme/app"));
        assert_eq!(body["preset"], json!("security"));
        assert_eq!(body["swarm_size"], json!(4));
        assert_eq!(body["autofix"], json!(true));
        assert_eq!(body["arm"], json!(true), "a CLI start arms unless --now");
        let plain = parse(&["redteam", "start", "acme/app", "--now"]).unwrap();
        let Some(Command::Redteam {
            command: RedteamCommand::Start { preset, now, .. },
        }) = plain.command
        else {
            panic!("redteam start did not parse");
        };
        assert_eq!(preset, RedteamPreset::General, "general is the default preset");
        let body = redteam_start_body("acme/app", preset, None, None, None, false, now);
        assert_eq!(body["preset"], json!("general"));
        assert_eq!(body["arm"], json!(false));
        assert!(parse(&["redteam", "start", "acme/app", "--preset", "offensive"]).is_err());
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

    /// `--timeout` belongs to `--wait` (a one-shot `pr` has nothing to time out), takes a duration,
    /// and refuses a zero or unspellable one: every such argument is a usage error (2).
    #[test]
    fn a_pr_timeout_without_a_wait_or_a_bad_duration_is_a_usage_error() {
        let err = parse(&["pr", "abc123", "--timeout", "30m"]).unwrap_err();
        assert_eq!(err.exit_code(), EXIT_USAGE, "--timeout needs --wait");
        for (good, secs) in [
            ("90", 90),
            ("90s", 90),
            ("30m", 1800),
            ("2h", 7200),
            // The unit spellings `loop create` reads for a cadence read the same here.
            ("2H", 7200),
            ("45min", 2700),
            ("1d", 86_400),
            ("10 seconds", 10),
        ] {
            let cli = parse(&["pr", "abc123", "--wait", "--timeout", good]).unwrap();
            let Command::Pr { timeout, .. } = cli.command.unwrap() else {
                panic!("pr did not parse");
            };
            assert_eq!(timeout, Some(Duration::from_secs(secs)), "{good} should parse as a --timeout");
        }
        for bad in [
            "",
            "0",
            "0m",
            "0s",
            "soon",
            "m",
            "1h30m",
            "99999999999999999999h",
            "999999999999999999d",
            "14d@03:00",
            "daily@09:00",
            "-5",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should be refused");
            assert!(
                parse(&["pr", "abc123", "--wait", "--timeout", bad]).is_err(),
                "{bad:?} should not parse"
            );
        }
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

    // -- Loops ------------------------------------------------------------------

    /// The cadence grammar parses into the shapes the stored `Cadence` takes, with local clock
    /// times moved to UTC (here UTC+2); ranges stay the server's business.
    #[test]
    fn the_cadence_grammar_parses_into_the_stored_shapes() {
        let at = |spec: &str| parse_cadence(spec, 120).unwrap();
        assert_eq!(at("30m"), json!({ "every": "interval", "minutes": 30 }));
        assert_eq!(
            at("2H"),
            json!({ "every": "interval", "minutes": 120 }),
            "units read in any case"
        );
        assert_eq!(at("7d"), json!({ "every": "interval", "minutes": 7 * 24 * 60 }));
        assert_eq!(
            at("daily@09:00"),
            json!({ "every": "daily", "hour": 7, "minute": 0 }),
            "local 09:00 is 07:00 UTC"
        );
        // The shift wraps over midnight: 01:00 at UTC+2 is 23:00 UTC the day before.
        assert_eq!(at("daily@01:00"), json!({ "every": "daily", "hour": 23, "minute": 0 }));
        assert_eq!(
            at("weekly@mon@09:00"),
            json!({ "every": "weekly", "weekday": 0, "hour": 7, "minute": 0 })
        );
        assert_eq!(
            at("weekly@3@09:00"),
            json!({ "every": "weekly", "weekday": 3, "hour": 7, "minute": 0 }),
            "a numeric weekday counts from Monday, like the stored cadence"
        );
        assert_eq!(
            at("monthly@15@09:00"),
            json!({ "every": "monthly", "day": 15, "hour": 7, "minute": 0 })
        );
        assert_eq!(
            at("14d@03:00"),
            json!({ "every": "every_days", "days": 14, "hour": 1, "minute": 0 })
        );
        assert_eq!(at("Self-paced"), json!({ "every": "self_paced" }));
        // The interval's 15-minute floor is the server's check, not the parser's.
        assert_eq!(parse_cadence("5m", 0).unwrap(), json!({ "every": "interval", "minutes": 5 }));
        for bad in [
            "",
            "hourly",
            "daily",
            "daily@25:00",
            "weekly@mon",
            "weekly@nod@09:00",
            "14d",
            "2h@09:00",
        ] {
            assert!(parse_cadence(bad, 0).is_err(), "{bad:?} should not parse");
        }
    }

    /// `loop list` describes a cadence in words with its clock times in the viewer's zone.
    #[test]
    fn a_cadence_describes_itself_in_local_words() {
        assert_eq!(
            describe_cadence(&json!({ "every": "interval", "minutes": 90 }), 0),
            "every 90 minutes"
        );
        assert_eq!(
            describe_cadence(&json!({ "every": "interval", "minutes": 1440 }), 0),
            "every 1 day"
        );
        assert_eq!(
            describe_cadence(&json!({ "every": "daily", "hour": 7, "minute": 0 }), 120),
            "every day at 09:00"
        );
        assert_eq!(
            describe_cadence(&json!({ "every": "weekly", "weekday": 0, "hour": 23, "minute": 0 }), 120),
            "every Tuesday at 01:00",
            "a UTC evening is the next day east of it"
        );
        assert_eq!(describe_cadence(&json!({ "every": "self_paced" }), 0), "self-paced");
    }

    /// The next run prints as a short local timestamp; an unreadable one prints as itself.
    #[test]
    fn the_next_run_prints_as_a_local_stamp() {
        assert_eq!(local_stamp("2026-09-29T07:30:00Z", 120), "2026-09-29 09:30");
        assert_eq!(local_stamp("not a timestamp", 0), "not a timestamp");
    }

    /// `loop stop` reads the loop's fields back and PUTs them with `enabled: false` — the exact
    /// edit the cockpit's switch makes — and an unknown id is a 404 (exit 4), not a silent ok.
    #[tokio::test]
    async fn stop_puts_the_loops_own_fields_back_disabled() {
        use axum::{Json, Router, routing::get, routing::put};
        use std::sync::{Arc, Mutex};

        let sent = Arc::new(Mutex::new(None));
        let saved = sent.clone();
        let app = Router::new()
            .route(
                "/api/loops",
                get(|| async {
                    Json(json!([{
                        "id": "loop_a", "name": "Triage", "repo": "acme/web", "prompt": "Triage new issues",
                        "cadence": {"every": "daily", "hour": 7, "minute": 0}, "kind": "colony",
                        "tz_offset_minutes": 120, "autopilot": true, "enabled": true,
                    }]))
                }),
            )
            .route(
                "/api/loops/loop_a",
                put(move |Json(body): Json<Value>| {
                    let saved = saved.clone();
                    async move {
                        *saved.lock().unwrap() = Some(body);
                        Json(json!({ "id": "loop_a", "enabled": false, "ended_reason": null }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let machine = Machine::for_tests(format!("http://{addr}"), "col".into());
        assert_eq!(set_loop_enabled(&machine, "loop_a", false, false).await.unwrap(), EXIT_OK);
        let body = sent.lock().unwrap().take().unwrap();
        assert_eq!(body["name"], json!("Triage"));
        assert_eq!(body["repo"], json!("acme/web"));
        assert_eq!(body["prompt"], json!("Triage new issues"));
        assert_eq!(body["cadence"], json!({"every": "daily", "hour": 7, "minute": 0}));
        assert_eq!(body["kind"], json!("colony"));
        assert_eq!(body["tz_offset_minutes"], json!(120));
        assert_eq!(body["autopilot"], json!(true));
        assert_eq!(body["enabled"], json!(false), "the only change a pause makes");

        let err = set_loop_enabled(&machine, "loop_z", false, false).await.unwrap_err();
        assert_eq!(err.exit_code(), EXIT_NOT_FOUND, "an unknown loop reads as not-found");
    }

    /// A stub mothership whose one colony reads as `reading(n)` — `n` counts the polls so far — on
    /// a loopback port. Returns the client and the read counter, so a test can tell looping from a
    /// single answer.
    async fn scripted_pr(
        reading: impl Fn(usize) -> Value + Clone + Send + Sync + 'static,
    ) -> (Machine, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use axum::{Json, Router, routing::get};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let app = Router::new().route(
            "/api/sessions/abc",
            get(move || {
                let reading = reading.clone();
                let seen = seen.clone();
                async move {
                    let n = seen.fetch_add(1, Ordering::SeqCst);
                    Json(reading(n))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (Machine::for_tests(format!("http://{addr}"), "col".into()), hits)
    }

    /// `pr --wait` follows a pending first reading to a settled one instead of ending on it:
    /// success reads as 0 (after really looping), failure as 7.
    #[tokio::test]
    async fn pr_wait_follows_pending_checks_until_they_settle() {
        use std::sync::atomic::Ordering;

        let (machine, hits) = scripted_pr(|n| {
            json!({
                "id": "abc",
                "status": "pr_opened",
                "pr_url": "https://github.com/acme/app/pull/9",
                "ci_state": (if n == 0 { "pending" } else { "success" }),
            })
        })
        .await;
        let code = wait_pr(&machine, "abc", false, None, Duration::from_millis(5)).await.unwrap();
        assert_eq!(code, EXIT_OK);
        assert!(
            hits.load(Ordering::SeqCst) >= 2,
            "a pending reading must loop, not end the wait"
        );

        let (machine, _) = scripted_pr(|n| {
            json!({
                "id": "abc",
                "status": "pr_opened",
                "pr_url": "https://github.com/acme/app/pull/9",
                "ci_state": (if n == 0 { "pending" } else { "failure" }),
            })
        })
        .await;
        let code = wait_pr(&machine, "abc", false, None, Duration::from_millis(5)).await.unwrap();
        assert_eq!(code, EXIT_CHECKS_FAILED);
    }

    /// Out of time is its own ending: a colony whose checks stay pending reads as 8, and a short
    /// timeout cuts the wait short instead of sitting out the whole poll interval.
    #[tokio::test]
    async fn pr_wait_times_out_while_checks_stay_pending() {
        use std::sync::atomic::Ordering;

        let (machine, hits) = scripted_pr(|_| {
            json!({
                "id": "abc",
                "status": "pr_opened",
                "pr_url": "https://github.com/acme/app/pull/9",
                "ci_state": "pending",
            })
        })
        .await;
        let started = std::time::Instant::now();
        let code = wait_pr(
            &machine,
            "abc",
            false,
            Some(Duration::from_millis(40)),
            Duration::from_millis(5),
        )
        .await
        .unwrap();
        assert_eq!(code, EXIT_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(2), "the deadline cuts the wait short");
        assert!(hits.load(Ordering::SeqCst) >= 1, "at least one reading before giving up");
    }

    /// A colony that ends without opening a pull request has nothing to wait for: 1, not 0.
    #[tokio::test]
    async fn pr_wait_refuses_a_colony_that_ends_without_a_pull_request() {
        let (machine, _) = scripted_pr(|_| json!({"id": "abc", "status": "no_changes"})).await;
        let code = wait_pr(&machine, "abc", false, None, Duration::from_millis(5)).await.unwrap();
        assert_eq!(code, EXIT_ERROR);
    }

    /// A pull request that was merged or closed before its checks settled, and a parked colony
    /// (paused, but publishing nothing), both mean no verdict is coming: 1.
    #[tokio::test]
    async fn pr_wait_refuses_a_pull_request_that_never_got_a_verdict() {
        use axum::{Json, Router, routing::get};

        let app = Router::new()
            .route(
                "/api/sessions/merged",
                get(|| async {
                    Json(json!({
                        "id": "merged",
                        "status": "merged",
                        "pr_url": "https://github.com/acme/app/pull/9",
                        "ci_state": "pending",
                    }))
                }),
            )
            .route(
                "/api/sessions/parked",
                get(|| async { Json(json!({"id": "parked", "status": "parked"})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let machine = Machine::for_tests(format!("http://{addr}"), "col".into());
        let code = wait_pr(&machine, "merged", false, None, Duration::from_millis(5))
            .await
            .unwrap();
        assert_eq!(code, EXIT_ERROR);
        let code = wait_pr(&machine, "parked", false, None, Duration::from_millis(5))
            .await
            .unwrap();
        assert_eq!(code, EXIT_ERROR);
    }

    /// The merge train (#720) merges a pull request as soon as its own reading of the checks is
    /// green, so a `--wait` can first see the colony already `merged`. A settled verdict still wins
    /// over the status: merged with checks green is 0, and a pull request closed after its checks
    /// failed is 7, not the "moved on untested" 1.
    #[tokio::test]
    async fn pr_wait_reads_a_verdict_even_after_the_merge_train_moved_the_pull_request() {
        let (machine, _) = scripted_pr(|n| {
            json!({
                "id": "abc",
                "status": (if n == 0 { "pr_opened" } else { "merged" }),
                "pr_url": "https://github.com/acme/app/pull/9",
                "ci_state": (if n == 0 { "pending" } else { "success" }),
                "merged_at": (if n == 0 { Value::Null } else { json!("2026-09-29T10:00:00Z") }),
            })
        })
        .await;
        let code = wait_pr(&machine, "abc", false, None, Duration::from_millis(5)).await.unwrap();
        assert_eq!(code, EXIT_OK, "merged by the train with checks green");

        let (machine, _) = scripted_pr(|_| {
            json!({
                "id": "abc",
                "status": "closed",
                "pr_url": "https://github.com/acme/app/pull/9",
                "ci_state": "failure",
            })
        })
        .await;
        let code = wait_pr(&machine, "abc", false, None, Duration::from_millis(5)).await.unwrap();
        assert_eq!(code, EXIT_CHECKS_FAILED, "a failed verdict is reported even on a closed PR");
    }

    /// A colony still on its way to a pull request is waited through; one that has settled
    /// without one is not. The refused spellings are exactly the terminal statuses plus `parked`,
    /// checked against the model so a new status cannot slip past either list.
    #[test]
    fn pr_still_coming_matches_the_session_model() {
        use crate::sessions::SessionStatus::*;
        for status in [
            Queued,
            Starting,
            Running,
            WaitingForAnswer,
            Idle,
            Publishing,
            PrOpened,
            Merged,
            Closed,
            NoChanges,
            Parked,
            Stopped,
            Failed,
        ] {
            assert_eq!(
                pr_still_coming(status.as_str()),
                !(status.is_terminal() || status == Parked),
                "{}",
                status.as_str()
            );
        }
    }

    /// A `--timeout` too long for the clock waits without a deadline instead of panicking on the
    /// instant arithmetic; the first settled reading still ends it.
    #[tokio::test]
    async fn pr_wait_with_an_overlong_timeout_does_not_panic() {
        let (machine, _) = scripted_pr(|_| {
            json!({
                "id": "abc",
                "status": "pr_opened",
                "pr_url": "https://github.com/acme/app/pull/9",
                "ci_state": "no_checks",
            })
        })
        .await;
        let huge = parse_duration("5000000000000000h").unwrap();
        let code = wait_pr(&machine, "abc", false, Some(huge), Duration::from_millis(5))
            .await
            .unwrap();
        assert_eq!(code, EXIT_OK, "no checks is nothing to wait for");
    }

    /// `loop merge-train …` edits change one thing in the settings it read back, and `show`
    /// prints the switch, the opted-in repositories and the last report.
    #[test]
    fn merge_train_edits_change_only_what_they_name() {
        let mut s = json!({"enabled": false, "allow": ["acme/web"], "never": [], "held": [], "max_merges": 4});
        edit_merge_loop(&mut s, &MergeTrainCommand::On).unwrap();
        edit_merge_loop(
            &mut s,
            &MergeTrainCommand::Allow {
                target: "Acme/App".into(),
            },
        )
        .unwrap();
        edit_merge_loop(
            &mut s,
            &MergeTrainCommand::Never {
                target: "acme/fork".into(),
            },
        )
        .unwrap();
        edit_merge_loop(&mut s, &MergeTrainCommand::Hold { session: "abc".into() }).unwrap();
        edit_merge_loop(
            &mut s,
            &MergeTrainCommand::Set {
                every: Some(120),
                max_merges: Some(2),
                repo_cap: vec!["acme/web=1".into()],
                cooldown_secs: None,
                ci_wait_minutes: None,
                flaky: Some("e2e*, lint".into()),
                self_heal: Some(Toggle::On),
                revert_on_red: None,
                redo: Some(Toggle::Off),
            },
        )
        .unwrap();
        assert_eq!(s["enabled"], json!(true));
        assert_eq!(s["allow"], json!(["acme/web", "acme/app"]));
        assert_eq!(s["never"], json!(["acme/fork"]));
        assert_eq!(s["held"], json!(["abc"]));
        assert_eq!(s["cadence"], json!({"every": "interval", "minutes": 120}));
        assert_eq!(s["repo_max_merges"], json!({"acme/web": 1}));
        assert_eq!(s["flaky_checks"], json!(["e2e*", "lint"]));
        assert_eq!(
            (s["self_heal"].clone(), s["redo_on_conflict"].clone()),
            (json!(true), json!(false))
        );
        assert!(s.get("revert_on_red").is_none(), "an untouched flag stays as it was");
        edit_merge_loop(
            &mut s,
            &MergeTrainCommand::Disallow {
                target: "acme/app".into(),
            },
        )
        .unwrap();
        assert_eq!(s["allow"], json!(["acme/web"]));
        let bad = MergeTrainCommand::Set {
            every: None,
            max_merges: None,
            repo_cap: vec!["acme/web".into()],
            cooldown_secs: None,
            ci_wait_minutes: None,
            flaky: None,
            self_heal: None,
            revert_on_red: None,
            redo: None,
        };
        assert!(edit_merge_loop(&mut s, &bad).is_err());

        let view = json!({
            "settings": s, "next_run_at": null, "writes_blocked": true,
            "repos": {"acme/web": {"paused": "main went red after the train merged #3"}},
            "last_report": {"lines": ["merged 1 · updated (CI running) 0 · red 0 · redo dispatched 0 · skipped 0"]},
        });
        let text = describe_merge_loop(&view).join("\n");
        assert!(text.contains("merge-train loop: on"), "{text}");
        assert!(text.contains("opted in: acme/web"), "{text}");
        assert!(text.contains("never: acme/fork"), "{text}");
        assert!(text.contains("every run is a dry run"), "{text}");
        assert!(text.contains("paused in acme/web"), "{text}");
        assert!(text.contains("merged 1"), "{text}");
    }
}
