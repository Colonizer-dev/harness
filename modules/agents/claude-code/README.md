# Claude Code agent module

Runs Claude Code through the [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk) and speaks
the Colonizer runner contract (`docs/protocol.md` §2): commands as JSON lines on stdin, events as JSON
lines on stdout, diagnostics on stderr. It is the default agent module, and the only one CI runs
in a real colony end to end (`scripts/colony-e2e.mjs`); the other modules are tested against stubs.

- Streaming-input session: every `user_message` command becomes a turn (or joins the current one).
- Questions: Claude is told to ask only via `AskUserQuestion`. The call is routed through `canUseTool`
  and surfaced as a `question` event keyed by the tool-use id; the matching `answer` command resolves
  it. Under the default `delegate = enforce` the orchestrator itself is limited to planning, asking
  and delegating; every other tool stays with its subagents (the microVM is the sandbox).
- Text streams as `assistant_text_delta` and settles as `assistant_text`; tool calls, tool results
  (capped at 20 000 characters) and `turn_end` (cost, duration) follow the protocol.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `COLONIZER_CLAUDE_BIN` | `/opt/claude/bin/claude` | Native Claude Code binary |
| `COLONIZER_MODEL` | Claude Code default | Orchestrator model (carries nearly all the traffic): alias, ID or `<provider>/<model>` |
| `COLONIZER_SUBAGENT_MODEL` | orchestrator model | Default subagent model (`CLAUDE_CODE_SUBAGENT_MODEL`); used only when the agent delegates to one |
| `COLONIZER_BACKGROUND_MODEL` | Claude Code default | Background model for small auxiliary calls (`ANTHROPIC_DEFAULT_HAIKU_MODEL`) |
| `COLONIZER_MODEL_ROUTES` | none | JSON provider routes (`docs/protocol.md` §6.1) |
| `COLONIZER_ACCOUNT_ROUTE` | none | JSON `{url, headers}` of the mothership's `GET /account-route` (set when the module's `account_fallback_model` is): before a request that would go to Anthropic, the router asks whether the Claude account is out and, if so, sends it to the fallback model's route instead (issue #1130) |
| `COLONIZER_MEMORY_DIR` | unset | Mounted shared memory; enables the memory tools (§6.2) |
| `COLONIZER_EFFORT` | model default | Orchestrator effort: `low`, `medium`, `high`, `xhigh` or `max` |
| `COLONIZER_SUBAGENT_EFFORT` | orchestrator effort | Effort for the `general-purpose` and `Explore` subagents, redefined with it (`subagents.mjs`); the first-party read-only `repo-explorer` is added either way; plugin agents keep the orchestrator's |
| `COLONIZER_ENFORCE_CHOICES` | on | Re-ask a plain-text question as a choice card once |
| `COLONIZER_DELEGATE` | `enforce` | `enforce`, `encourage` or `off`: how far the orchestrator hands work to subagents (see above) |
| `COLONIZER_DISABLED_TOOLS` | none | Comma-separated Claude Code tool names the colony never gets |
| `COLONIZER_PLUGIN_DIRS` | `archify` | Plugin directories to load, read-only; the mothership rewrites them to their in-VM paths |
| `COLONIZER_SCAN`, `COLONIZER_SCAN_COMMAND` | `off`, none | Pre-flight scan of the workspace before the agent starts: `warn` or `block`, with a scanner you mount yourself (`preflight.mjs`; advisory, not a boundary) |
| `COLONIZER_CAVEMAN`, `COLONIZER_CAVEMAN_LEVEL` | off, `full` | Terse replies (`lite`, `full` or `ultra`) |
| `COLONIZER_RTK` | off | Shell output shortened by rtk before the agent reads it |
| `COLONIZER_HEADROOM` | off | Model requests pass through Headroom inside the colony (`headroom.mjs`) |
| `COLONIZER_JEV_COMPACTION`, `COLONIZER_JEV_KEEP_THRESHOLD`, `COLONIZER_JEV_PRESERVE_RECENT` | off, 0.5, 6 | Compaction by Jev score instead of Claude Code's summary |
| `COLONIZER_TASK_LABELS` | unset | Comma-separated task labels (set by the mothership from the issue) for `.colonizer/instructions.toml` label rules |
| `COLONIZER_EXEC_POLICY` | unset | The install layer's exec policy as JSON (the `exec_policy` setting); see Exec policy below |
| `COLONIZER_EXEC_POLICY_ORG` | unset | The org layer's exec policy as JSON (the org's workspace setting); narrows the install layer, is narrowed by the repo file |
| `COLONIZER_FINDINGS`, `COLONIZER_LOOP`, `COLONIZER_LOOP_SELF_PACED`, `COLONIZER_RESUME_SESSION`, `COLONIZER_IMAGE` | set by the mothership | The findings tool, a loop colony's tools, the Claude Code session to resume (a suspended colony picks up where it stopped), and the image named in the prompt |

The module's settings in the cockpit (Settings → Modules) set most of these; `summaries`,
`summary_model`, `route_per_task`, the tier models and the `route_cost_*`/`jev_shadow_mode` settings
are read by the mothership; the runner does not use them.

Credentials come from `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY` (a microsandbox placeholder in
the VM).

## Egress

The `egress` declaration in `module.json` comes from a live capture, not from the CLI's documented
requirements: on 2026-09-28, Claude Code 2.1.280 (the `crates/colonizer/claude-code.lock` pin) ran inside a
colony sandbox behind a logging forward proxy (`HTTPS_PROXY`, HTTP CONNECT), with tcpdump on udp/53
and socket sampling as backstops for anything bypassing the proxy and a fresh `HOME` per run. The
scenarios were the runner's own invocation, CLI defaults, no credentials at all, `claude auth login`
and `claude setup-token` under a pty, and `claude doctor`, `auth status` and `claude update` on a
copy of the binary.

| Host | Category | Evidence |
| --- | --- | --- |
| `api.anthropic.com` | `api` | Observed: the only host on the wire in the runner invocation, CLI defaults, the no-credential run and `claude doctor`; the binary also carries `/v1/messages`, `/v1/models`, first-party event logging, WebFetch's domain check, OAuth profile/roles and managed settings on it |
| `platform.claude.com` | `auth` | In the binary, not observed: the OAuth token exchange. Authorize pages open in the user's browser, and the mothership runs `claude setup-token` host-side, never in-colony |
| `http-intake.logs.us5.datadoghq.com` | `telemetry` | In the binary (Datadog logs intake); one DNS query during the capture that could not be attributed to the CLI |
| `*.sentry.io` | `telemetry` | In the binary (`o1158394.ingest.us.sentry.io`), never contacted |
| `api.typesafe.ai` | `extra` | Not the CLI: this module's own Jev compaction and shadow-routing plugin, running inside the CLI process (also a secrets host) |

What the old declaration dropped, and why: `console.anthropic.com` and `statsig.anthropic.com` have
zero occurrences in the 2.1.280 binary (statsig survives only as a local cache directory name),
`claude.ai` was only ever the login page in a browser, `downloads.claude.ai` is reached by
`claude update` alone — never at startup, even with updater defaults, and colonies disable it with
`DISABLE_AUTOUPDATER=1` — and `mcp-proxy.anthropic.com` serves remote MCP connectors the runner does
not configure.

The capture could not exercise a credentialed model turn, WebFetch execution, a completed OAuth
login or a forced Sentry/Datadog export; re-run it when the pin moves.

## Model routing

`COLONIZER_MODEL` is the orchestrator's model, and the orchestrator does nearly all of the work in a
colony, so this setting carries almost all of the traffic. `COLONIZER_SUBAGENT_MODEL` only applies
when the agent delegates to a subagent, and colonies rarely do: four recent colonies of 413 to 2 095
events spawned 0, 0, 0 and 1 between them. `COLONIZER_BACKGROUND_MODEL` carries Claude Code's small
auxiliary calls. A provider reachable only through the subagent and background settings is configured
correctly and will still see almost nothing; to put real traffic on your own hardware, point
`COLONIZER_MODEL` at it.

Which model `COLONIZER_MODEL` carries can be chosen per task. With the agent module's `route_per_task`
setting on (the default), the mothership reads the issue in front of a colony and boots it on one of
three tiers: `low` runs on the `model_low` setting, `high` on `model_high`, and `medium` on `model`.
Both tier settings accept the same forms as `model` — a Claude alias or ID, or `<provider>/<model>` —
and a blank tier setting falls back to `model`, so with neither tier model set nothing changes about
which model a colony runs on; `route_per_task: false` puts every colony that was not started with an
explicit tier on `model`. Only the orchestrator model is routed — the subagent and background settings
are untouched — and the tier settings are read on the mothership alone: their env vars are stripped
from the colony's environment once the tier is chosen, so only the provider actually in use is probed
at boot. What the rule reads off an issue, and what it records, is `docs/protocol.md` §6.1b.

When a route or a `<provider>/<model>` model is configured, `router.mjs` listens on `127.0.0.1` and
Claude Code's `ANTHROPIC_BASE_URL` points at it. A request whose `model` starts with a route prefix
(`deepseek/deepseek-flash`) goes to that route's `base_url` with the prefix stripped, the route's key
(`x-api-key`, `Bearer`, or none), and without the Anthropic credential or `oauth-*` betas. Everything
else passes through to `https://api.anthropic.com` unchanged, so a subscription login keeps working for
the orchestrator. Routed `count_tokens` calls the provider doesn't support get an estimate. Provider key
variables are removed from Claude Code's own environment.

With `COLONIZER_ACCOUNT_ROUTE` set, a request for a Claude model first asks the mothership whether the
Claude account is out (the answer is reused for 5 s; a slow, failed or unrecognised answer means
Claude, as without the feature). When it is out and the install names an `account_fallback_model`, the
request goes to that model's route like any `<provider>/<model>` request, with the same stripping of the
Anthropic credential; at the reset it goes back to Anthropic by itself. A task the fallback may not
carry (restricted work, an untrusted provider) stays on Anthropic and parks on the limit.

Responses stream through as they arrive, with no overall time limit. The router gives an upstream 30 s
to connect and lets it stay silent for up to `router_idle_timeout_secs` (600 s by default) before its
answer starts or between two chunks of it. A failure is answered for what it is — unreachable (DNS,
connect, TLS) as a 502, a timeout as a 504 `timeout_error`, a broken connection as a 502 with its error
code — and logged with `source: "model_router"` (provider, class, status, elapsed time); upstream 401/403,
429 and 5xx answers pass through unchanged. A request whose connection is closed under it before any
of the answer arrives (`UND_ERR_SOCKET`, the keep-alive race on a pooled socket) is sent once more on a
new connection and logged as `class=connection_retry`. See docs/protocol.md §6.1 (issue #983).

Claude Code sends its full request shape to routed providers, including `thinking`, `context_management`,
`output_config`, `metadata`, every tool definition and betas such as `context-management-*` and
`advisor-tool-*`. Providers that reject unknown fields need to ignore them; a provider on the gateway's
`openai` wire gets a rebuilt request without them (docs/protocol.md §6.5).

## Shared memory

With `COLONIZER_MEMORY_DIR` set, the agent gets four auto-allowed tools from an in-process MCP server
(`colonizer_memory`), and the system prompt gets one fixed line saying they exist. No note text is
ever put into the prompt (issue #766):

- `memory_briefing(topic?)`: a short, sourced summary from each scope's mounted `notes.json`, one entry
  per line with its scope, kind and source (colony, repository, commit, reviewed or not).
- `memory_changes(since?)`: entries added, and entries revoked or removed, since the colony last asked.
- `memory_search(query)`: snippets from `{repo,org,global}/notes/*.md`.
- `memory_propose(scope, title, content, kind?, confidence?, tags?)`: emits a `memory_proposal` event
  for review on the mothership (with review off, a repo note is stored straight away; org notes always
  wait for review, and a global note is a candidate until colonies in two repositories propose it at
  confidence 0.8 or more). Nothing is written inside the colony.

## Exec policy

Every Bash command meets the layered rules in `execpolicy.mjs` (issue #471), before it runs — and
for `bash x.sh` / `python x.py` / `node x.js`-style commands, the contents of the script it runs are
read (capped at 256 KiB) and meet them too. A rule matches when all of its predicates hold; the
first match in a layer wins, and across layers the strictest decision wins (`deny > ask > allow`),
so a layer can only ever narrow. A `deny` refuses the call with the rule named; an `ask` becomes a
colony question (Allow / Deny) that the operator — or the autonomy judge, within its risk ceiling —
answers. Every decision leaves one `exec policy: <decision> rule=… layer=… command=…` line in the
harness log.

The question event carries `kind: "exec_policy"` (issue #759). The Bash call that asked is blocked in
flight until the answer, so the mothership never suspends a colony while such a question waits — a
suspension would kill the call and the agent that made it. An **Allow** is remembered for the rest
of the colony's run, keyed on the rule, its layer and the command with its whitespace collapsed:
the same command is not asked about again, whether the same agent retries it or a subagent spawned
later runs it. A different command, or the same one under another rule, still asks; a **Deny** is
never remembered, and a `deny` rule is refused before the memory is consulted. The memory lives
only in the runner process, never on disk where the agent could write itself an approval, so a
colony restarted in a fresh microVM asks again. The ACP runner does the same.

A question the runner puts to the colony while a tool call is blocked on the answer inside a live
agent also carries `blocking: true`: every exec-policy ask, and an `AskUserQuestion` asked by a
**subagent** — named by canUseTool's `agentID`, or by the tool_use having arrived in a message with a
`parent_tool_use_id`. The mothership does not suspend such a colony (up to a two-hour cap): the
subagent is blocked in its Task call, and a resumed lead transcript would get an answer to a question
it never asked. The lead's own `AskUserQuestion` is unmarked; it resumes cleanly, so it still suspends.

```json
{ "rules": [ { "id": "no-deploys", "decision": "deny", "reason": "deploys go through CI",
               "script": ["\\bkubectl\\s", "\\bterraform\\s"] },
             { "id": "ask-before-eject", "decision": "ask", "writes_outside": true } ] }
```

Predicates: `command` (regex over the command), `touches` (path globs matched against the path-like
tokens of the command and its scripts; `~` is $HOME; components at any depth, `*` never crosses
`/`; an entry starting with `!` excludes the tokens it matches), `script` (regex over script
contents), `writes_outside` (a redirect or cp/mv/rm/tee-style target that is an absolute path
outside the repository *and on a host-backed mount*, or a write onto a read-only host mount
(`/colonizer`, `/opt/colonizer`) or into the checkout's own `.git` — `/tmp`, the `/dev` sinks and
the microVM's own root filesystem don't count) and `writes_git` (a write into the checkout's own
`.git`, named or not, or a `git add`/`git commit`/`git stash` invocation, #1258).
`writes_outside` is `true` for that set; the string
`"strict"` widens it to *every* absolute path outside the repository, as before issue #877 (see
below). The reason on the card says which: a host-backed path, a read-only mount by name, or the
`.git` internals.
Layers, in order: **default** (built in: deny `secret-paths` — `~/.ssh`, `.env*` and the files the
path policy masks, with committed env templates (`*.example`, `*.sample`, `*.template`, `*.dist`)
not counting; deny `script-egress` — network calls in a script, while a direct `curl` command
stays the egress policy's business. A syntax check (`bash -n`, `node --check`, `ruby -c`) runs
nothing and is not read, and the repository's own scripts, byte for byte as the base commit has
them (the boot writes their object ids to `/colonizer/tracked-scripts`, #1239), are left to the
egress policy too; a script the colony adds or edits is read as before, and other layers still see
every script; deny `git-read-only` — `git add`/`git commit`/`git stash` and any write under the
checkout's `.git`, since the mount is read-only by design and the denial carries the instruction
to leave the changes in the working tree (#1258); ask `writes-outside-repo`), **install** (the agent module's
`exec_policy` setting, `COLONIZER_EXEC_POLICY`), **org** (the org's workspace settings → Exec
policy, stored as `exec_policy` in `orgs.json` and passed as `COLONIZER_EXEC_POLICY_ORG`) and
**repo** (`.colonizer/exec-policy.json` in the worktree, read once at start so the agent cannot
rewrite it mid-run). A malformed layer is dropped with a warning; the default always holds. The
org layer is checked when it is saved (owner-only, `PUT /api/orgs/{org}`), so it never gets that
far: the save is refused unless the runner would keep every rule of it — JSON of at most 64 KiB,
an object with a `rules` array, each rule a `deny`/`ask`/`allow` decision with at least one
usable predicate (`crates/colonizer/src/exec_policy.rs`, sharing
`test/fixtures/execpolicy-valid.json` with this parser). A pattern's regex syntax is the one thing
it cannot check there. Note
this is guidance in front of the model, like the delegation gate — not a boundary; the microVM is.

`writes_outside` reads the boot's writable-bind list (`/colonizer/host-mounts`; env override
`COLONIZER_HOST_MOUNTS`, a file path). The microVM's root filesystem is discarded when the colony
stops, so a write there — `mkdir -p /root/target`, a rustup install under `/root/.cargo`, an install
under `/usr`, a `CARGO_TARGET_DIR` like `/root/colonizer-target` — is not the host's to protect:
only a path at, under or above a listed mount (`/workspace`, `/harness/out`, the resume directory,
`/colonizer/services`) asks. A write *above* a mount asks too, so `rm -rf /root` still asks while
`/root/.claude/projects` is mounted. The guest's own read-only host mounts — `/colonizer` (the
mothership's `host-mounts`, memory scopes, the services mount point) and `/opt/colonizer` (the
agent's binaries, runner and plugins) and the vendored runtime binaries (`/opt/node/bin/node`,
`/opt/claude/bin/claude`) — ask regardless of the list, since a write there is the host's even
though the mount refuses it; a write into the checkout's `.git` is not an ask but the `git-read-only`
deny, whatever the list says (#1258). When the list is
absent — an older mothership, or a runner outside a VM — every absolute path outside the repository
asks, as before.

An org (or install, or repo) layer can restore that conservative behaviour with a rule whose
predicate is the string `"strict"`, e.g.
`{ "id": "strict-writes", "decision": "ask", "writes_outside": "strict" }`: it asks for a write to
any absolute path outside the repository — the microVM's owned root filesystem included — even with
the mount list present. `/tmp` and a write inside the repository never ask, strict or not, and the
layering is unchanged (a stricter decision still wins, and a later layer still cannot widen).

Coverage: Claude Code and the ACP runner apply the policy. Codex, Grok Build, Hermes, OpenCode and
Pi do not — the harness refuses to launch a colony on one of them while a policy is set (the
install's `exec_policy` setting, the org's exec policy, or a repo `.colonizer/exec-policy.json`),
naming the module and where the policy came from — the org by name, so a set policy is never silently ignored.

## Path policy

Every path-taking tool call (`Read`, `Write`, `Edit`, `MultiEdit`, `NotebookEdit`, `Grep`, `Glob`)
meets the mounted bind list (`pathpolicy.mjs`, issue #647) after it is resolved through any
symlink, the way the boot resolved its binds. A call that lands on a masked path — or writes to a
masked or protected one — emits one `path_policy` event per distinct (access, path); reads of
protected paths are allowed, so they are not attempts. Reporting only: the event carries no
decision, the tool runs exactly as it would have, and the mount (docs/path-policy.md) is what
enforces. The harness logs each attempt on the colony and in the History log. A masked path
reached through `Bash` never gets here — that is the exec policy's `secret-paths` rule above.

## Waiting

Every colony also gets `mcp__colonizer_wait__wait` from an in-process MCP server (`colonizer_wait`),
with no setting to switch it on: a colony that cannot block burns model turns polling. It takes a
one-line `reason` (so the transcript says what the wait was for) and exactly one of: `seconds`, to
sleep; `file` and `pattern` (a JavaScript regex), to return as soon as a line of the file matches —
the file need not exist yet, it is read incrementally from the last byte offset, and the read starts
over when the file shrinks or its inode changes under the same path (truncated, or rotated by
rename). The watcher assumes the file is appended to: a same-inode rewrite that leaves the file at
least as long as the offset already read cannot be detected. A path that exists but is not a regular
file — directory, FIFO, socket, device — is refused as plain text, because opening a FIFO with no
writer blocks inside the threadpool and would never reach the timeout or an abort. A final line with
no trailing newline still matches, and is flagged as unterminated. Or `pid`, to return when that
process is gone (a disappearance, not an exit status — the colony cannot reap a process it did not
spawn, and an unreaped zombie still answers the liveness check, so a wait on one reports it as still
running). `seconds` and the `pid`/`file` timeout (default 300 s) cap at 1800 s and clamp with a note
rather than erroring. A timeout on a file wait returns the file's last lines, read from at most its
final 64 KiB, so one call is enough to see where a stuck build is. The tool description itself
carries the "use this instead of a grep poll loop or `Bash true`" guidance, because subagents see
descriptions but not the system prompt.

One honest limit on the `pattern`: it is evaluated by the runner's own event loop, so the timeout
bounds the waiting, not the regex evaluation. A pathological pattern — catastrophic backtracking,
the classic `(a+)+$` against a long line — can freeze the runner for far longer than any timeout.
Keep patterns simple: a literal substring or a simple regex.

## Conditional instructions

`CLAUDE.md`/`AGENTS.md` load once at colony start, and a compaction can summarise them away. Two
repo files add instructions that are injected exactly when they apply, once per fragment per
context window (`instructions.mjs`):

- `FOOTGUNS.md` in any directory applies to work on files in or under it. An `AGENTS.md` in a
  subdirectory does the same — the repo root one already loads as project instructions.
- `.colonizer/instructions.toml` maps conditions to instruction files:

  ```toml
  [[rule]]
  file = "docs/STYLE.md"        # relative to the repo root
  paths = ["web/**", "*.css"]   # gitignore-style globs, single-line arrays
  labels = ["frontend"]         # this task's labels, any-of
  ```

  Globs follow gitignore semantics: a pattern holds for a path that matches it directly or through
  an ancestor directory (`web/*` covers `web/src/App.tsx`), and a pattern without a slash is matched
  against the basename, so `*.css` is a file type. A rule holds while one of the last 20 distinct
  paths the agent touched matches, or the task carries one of its labels.

Conditions are watched through harness hooks: PreToolUse on the tools whose input names a path
(`Read`, `Edit`, `Write`, `MultiEdit`, `NotebookEdit`, `Glob`, `Grep`, and `Bash`, where each
whitespace-separated token that names a file in the worktree counts), plus `UserPromptSubmit` for
the paths a prompt names. Injection only ever adds context — no hook here denies or rewrites
anything. A fragment is capped at 16 KiB, resolved against the real worktree (a rule file outside
it, including one reached through `..` or a symlink, is refused with a logged warning), and each
load is logged to the colony log.

After a compaction every fragment whose condition still holds is re-injected, and the rest are
dropped until they are relevant again. A repo with no `FOOTGUNS.md` anywhere and no
`instructions.toml` pays only a couple of failed stat calls per directory the agent touches.

## Develop

```sh
npm ci            # production: npm ci --omit=dev
npm test          # fake-SDK tests, no network
printf '%s\n' '{"type":"user_message","id":"initial","text":"hello"}' | node runner.mjs
```
