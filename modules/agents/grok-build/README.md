# grok-build (agent module)

Drives xAI's [Grok Build](https://github.com/xai-org/grok-build) CLI (`grok`) as a Colonizer agent
module on the `colonizer-runner/1` protocol. **Status: experimental, from
[#333](https://github.com/Colonizer-dev/harness/issues/333).** It is pickable in Settings and per
org (module discovery lists every `modules/agents/*/module.json`), the mothership pushes the xAI key
into its colonies when configured and routes prefixed models through the provider gateway, but
nothing stages the `grok` binary into the colony image and it has not run in a real colony; see
"What remains".

The runner is `runner.mjs`: one headless `grok` process per turn (`--prompt-file`,
`--output-format streaming-json`), the first turn's `end` event yields the grok `sessionId`, and
every later turn resumes it with `-r`, so a colony is one continuous grok session. Streaming events
map to protocol events (`text` → `assistant_text_delta`/`assistant_text`, `thought` → `thinking`,
`tool_call`/`tool_call_update` → `tool_call`/`tool_result`, `end`/`error` → `turn_end`); unknown
event types are logged, never fatal. Every flag is from the upstream user guide
(`14-headless-mode.md` unless another file is named), and so are the event fields — except that
upstream never shows a `cacheCreationInputTokens` bucket in `modelUsage`, so that one is read
leniently and counts as zero when absent.

## The colonizer MCP server

At startup the runner registers a `colonizer` MCP server by writing `[mcp_servers.colonizer]` with
`command`, `args` and `env` into `$GROK_HOME/config.toml` (26-config-reference.md; `GROK_CONFIG`
overlays cannot add MCP servers): `mcp.mjs`, a dependency-free stdio server that the runner points at
a loopback HTTP bridge held for the colony's life. `wait` (block instead of polling; grok's default
`tool_timeout_sec` of 6000 s covers a wait's 1800 s cap), `memory_briefing`, `memory_changes` and
`memory_search` (shared memory is pulled through these, never put into the prompt; issue #766) run
inside the server;
`finding_file` and `memory_propose` cross the bridge and leave the colony as `finding` and
`memory_proposal` events, and so do a loop colony's pacing tools: `loop_next` (the next run's delay
in minutes, clamped to 15–1440 like the mothership clamps it) and `loop_stop`. The model sees the
tools as `colonizer__<tool>`, and `--always-approve` auto-approves their calls. The tool list
follows the same switches as the other modules: findings only under `COLONIZER_FINDINGS=true`,
memory only when `COLONIZER_MEMORY_DIR` is mounted, the loop tools only under `COLONIZER_LOOP=true`
— `loop_next` additionally when `COLONIZER_LOOP_SELF_PACED=true` — and `wait` always.

The first tool in the list follows no switch: **`ask_user`** is the question channel
(docs/protocol.md §2). A call POSTs to the bridge, which emits the `question` event, moves the
status to `waiting_for_answer`, and holds the HTTP response until the matching `answer` command
resolves it with `{answers, response}` and the status settles back — the turn continues on the
answer. A colonizer question is never also a `tool_call`/`tool_result`, and no `tool_timeout_sec`
override is needed: grok's default (6000 s) outlives any human. An `answer` naming an unknown id is
warned about, and an interrupt, a turn end or a shutdown cancels every open ask
(`{cancelled: true}`).

## Headless, not ACP

Grok also speaks ACP (`grok agent stdio`, bidirectional — tool approvals and questions). This slice
uses headless mode instead: one process per turn, read-only stream, no SDK to vendor, and a mapping
small enough to test against a stub CLI. Nothing interactive crosses the grok boundary itself:
`interrupt` SIGINTs the child (grok saves session state and exits 130), ends the turn as an error,
and the runner keeps serving turns. Questions still reach the user — through the colonizer MCP
server's `ask_user` (above), which the `answer` command answers.

## Pinned binary

`module.json` pins `grok` **1.0.34** (upstream `SOURCE_REV` `036a5d8348cd744767cd0b08518ab17bf608fa7f`;
install: `curl -fsSL https://x.ai/cli/install.sh | bash -s 1.0.34`). The runner resolves the binary
from `COLONIZER_GROK_BIN`, else `grok` on the colony's PATH, and reads the pin back from
`module.json` so the two cannot drift.

## Preflight

Before any grok process is spawned (a colony must fail loudly, not hang on a prompt nobody can
answer), each named problem emits a `log` error plus `status error` with the name as `detail`:

- `GROK_CREDENTIAL_MISSING` — `XAI_API_KEY` unset/empty. Fix below.
- `GROK_WORKSPACE_UNTRUSTABLE` — the workspace is the home directory or the filesystem root, which grok's folder trust auto-trusts instead of gating (an unrecordable trust root); run the colony from a dedicated worktree.
- `GROK_BINARY_MISSING` — no grok at `COLONIZER_GROK_BIN`/PATH; the log carries the pinned install command.
- `GROK_VERSION_DRIFT` — `grok --version` (parsed leniently for X.Y.Z) is not the pinned version.
- `GROK_MODEL_PROVIDER` — a model setting that names a provider with no gateway route, or one whose route speaks the anthropic wire (see "Credential story").

## Credential story

Like the Claude module's #30 precedent: the colony holds only a placeholder; the mothership holds
the real xAI key and swaps it in on TLS to `api.x.ai` (declared in `module.json` `secrets`). The
mothership pushes the key in itself at boot when it has one — the stored key of a provider named
`xai-grok`, else its own `XAI_API_KEY`, arriving as `XAI_API_KEY` for host `api.x.ai`; a user-added
colony secret keeps working for when neither is configured. The colony never authenticates
interactively: the runner refuses to spawn grok without a credential (a routed model counts),
never runs `grok login`, and additionally starts every grok child with `BROWSER=/bin/false`
(belt-and-braces — not a documented grok switch) so nothing can open a browser. A fresh `GROK_HOME`
also means no cached OAuth token (02-authentication.md: the API key authenticates when no session
token is active).

A `<provider>/<model>` whose prefix matches a model route (`docs/protocol.md` §6.5) rides the
mothership's provider gateway instead of `api.x.ai`: the runner sets `GROK_MODELS_BASE_URL` to the
route's `/v1` and `GROK_CODE_XAI_API_KEY` to the per-colony token from the route's headers — when
that base URL is set, Grok Build sends the API key as `Authorization: Bearer`
([Vercel AI Gateway docs for Grok Build](https://vercel.com/docs/ai-gateway/coding-agents/grok-build),
which document both variable names) — and drops `XAI_API_KEY` from the child env, so the placeholder
key never reaches the gateway. The gateway meters the tokens and prices them, so spend accounting and
the colony's budgets apply, and no xAI key is needed on those turns. Only `openai`-wire routes fit —
the gateway serves the OpenAI paths on those alone — so an `anthropic`-wire route, or a prefix nobody
configured, is refused with `GROK_MODEL_PROVIDER` before grok runs.

## Nesting decisions

The microVM is the boundary. Inside it:

| Surface | Decision | How |
| :--- | :--- | :--- |
| grok OS sandbox | **off** | `--sandbox off` explicitly (18-sandbox.md) |
| Tool approval | **auto** | `--always-approve`: headless cannot answer a prompt; the microVM is the boundary |
| Web search/fetch | **off** | `--disable-web-search` (backend host unverified; egress denies it anyway) |
| Colony-disabled tools | **per setting** | the `disabled_tools` setting rides as grok's own headless denylist, `--disallowed-tools <ids>` (14-headless-mode.md); the colonizer MCP tools are not on it |
| Cross-session memory | **off** | `GROK_MEMORY=0` (05-configuration.md) |
| Telemetry | **off** | `GROK_TELEMETRY_ENABLED=0` (05-configuration.md) |
| Auto-update | **off** | `--no-auto-update` + `GROK_DISABLE_AUTOUPDATER=1` |
| Host config / OAuth token / hooks / plugins / skills (user scope) | **off in practice** | a fresh, empty `GROK_HOME`: these live under it (14-headless-mode.md "File Locations") |
| Colonizer MCP tools | **on** | the runner-written `$GROK_HOME/config.toml` registers `node mcp.mjs`; gated on `COLONIZER_FINDINGS` and `COLONIZER_MEMORY_DIR` like every module, the loop tools on `COLONIZER_LOOP`/`COLONIZER_LOOP_SELF_PACED` (above, "The colonizer MCP server") |
| Hooks / plugins / MCP / skills (project scope) | **enforced off** | the folder-trust gate forced on with `GROK_FOLDER_TRUST=1` (env beats a `[folder_trust] enabled` kill-switch in any config), and the fresh `GROK_HOME` has an empty trust store: a headless run (no TTY, no `--trust`) resolves the workspace untrusted, and grok then skips project `.grok/` MCP servers, plugins, hooks and skills, plus project LSP and instructions (AGENTS.md) — the repo must be re-briefed through the prompt. Release-stamped binaries only: a self-built, unstamped grok never gates (the pinned install.sh build is stamped). Verified live by the contract test behind `COLONIZER_GROK_LIVE_BIN` ("Tests") |

Subagents and plan mode are left at grok's defaults; `--no-subagents`/`--no-plan` exist if a later
slice wants them off.

## Tests

`npm test` (no dependencies; the module's `package.json` has none on purpose) boots the real
`runner.mjs` over stdio against `test/fake-grok.mjs`, a stub grok CLI that records the argv, env,
TTY state and trust store it received, and — when a test scripts `GROK_FAKE_MCP_CALLS` — plays the
model against the registered colonizer MCP server, so findings, memory, the loop tools, wait and an
ask-and-answer round trip are tested end to end (`mcp.mjs` itself is byte-identical to the codex
module's and covered by its
`test/mcp.test.mjs`). The happy path's events are checked against the required fields of
`docs/agent-events.schema.json`. CI covers only these stubbed contract tests.

One contract test is live: with `COLONIZER_GROK_LIVE_BIN` pointing at the pinned binary it drives
the real runner (a wrapper turns the turn into `grok inspect --json` in the workspace, which needs
no key) and asserts grok itself reports `projectTrusted: false` with none of a planted `.grok/`
loadable:

```
COLONIZER_GROK_LIVE_BIN=/path/to/grok npm test
```

Without the variable the test skips. `inspect` lists project MCP servers even when untrusted — it
is a discovery report; the gate that removes them sits at spawn (`filter_untrusted_project_mcp_with`),
keyed on the same verdict the test asserts.

## What remains

- Binary fetch/lock/mount like `scripts/fetch-agent-binary.sh` + `crates/colonizer/claude-code.lock`
  ([#602](https://github.com/Colonizer-dev/harness/issues/602)), so a colony does not depend on grok
  being preinstalled in the image. (The harness now refuses a launch
  or boot on a stock preset image, where grok is never present; a custom image is still only
  checked by the runner's in-VM preflight.)
- A manual end-to-end run on a real colony with a real key. (The module already has its row in the
  README's module table and in [docs/providers.md](../../../docs/providers.md).)
- The [exec policy](../claude-code/README.md#exec-policy) is not applied: the harness refuses to
  launch a grok-build colony while one is set (the install's `exec_policy` setting, or a repo
  `.colonizer/exec-policy.json`).
