# grok-build (agent module)

Drives xAI's [Grok Build](https://github.com/xai-org/grok-build) CLI (`grok`) as a Colonizer agent
module on the `colonizer-runner/1` protocol. **Status: PLANNED / experimental — the first slice of
[#333](https://github.com/Colonizer-dev/harness/issues/333).** It is pickable in Settings (module
discovery lists every `modules/agents/*/module.json`), but nothing mothership-side knows about it
yet; see "What remains".

The runner is `runner.mjs`: one headless `grok` process per turn (`--prompt-file`,
`--output-format streaming-json`), the first turn's `end` event yields the grok `sessionId`, and
every later turn resumes it with `-r`, so a colony is one continuous grok session. Streaming events
map to protocol events (`text` → `assistant_text_delta`/`assistant_text`, `thought` → `thinking`,
`tool_call`/`tool_call_update` → `tool_call`/`tool_result`, `end`/`error` → `turn_end`); unknown
event types are logged, never fatal. Every flag is from the upstream user guide
(`14-headless-mode.md` unless another file is named), and so are the event fields — except that
upstream never shows a `cacheCreationInputTokens` bucket in `modelUsage`, so that one is read
leniently and counts as zero when absent.

## Headless, not ACP

Grok also speaks ACP (`grok agent stdio`, bidirectional — tool approvals and questions). This slice
uses headless mode instead: one process per turn, read-only stream, no SDK to vendor, and a mapping
small enough to test against a stub CLI. The cost is that nothing interactive can cross the
boundary: an `answer` command is logged and ignored (the agent cannot ask the user anything yet),
and `interrupt` SIGINTs the child (grok saves session state and exits 130), ends the turn as an
error, and the runner keeps serving turns. Question routing — via ACP (`session/request_permission`)
or a colonizer MCP ask tool — is the follow-up.

## Pinned binary

`module.json` pins `grok` **1.0.34** (upstream `SOURCE_REV` `036a5d8348cd744767cd0b08518ab17bf608fa7f`;
install: `curl -fsSL https://x.ai/cli/install.sh | bash -s 1.0.34`). The runner resolves the binary
from `COLONIZER_GROK_BIN`, else `grok` on the colony's PATH, and reads the pin back from
`module.json` so the two cannot drift.

## Preflight

Before any grok process is spawned (a colony must fail loudly, not hang on a prompt nobody can
answer), each named problem emits a `log` error plus `status error` with the name as `detail`:

- `GROK_CREDENTIAL_MISSING` — `XAI_API_KEY` unset/empty. Fix below.
- `GROK_BINARY_MISSING` — no grok at `COLONIZER_GROK_BIN`/PATH; the log carries the pinned install command.
- `GROK_VERSION_DRIFT` — `grok --version` (parsed leniently for X.Y.Z) is not the pinned version.

## Credential story

Like the Claude module's #30 precedent: the colony holds only a placeholder; the mothership holds
the real xAI key and swaps it in on TLS to `api.x.ai` (declared in `module.json` `secrets`). The
colony never authenticates interactively: the runner refuses to spawn grok without `XAI_API_KEY`,
never runs `grok login`, and additionally starts every grok child with `BROWSER=/bin/false`
(belt-and-braces — not a documented grok switch) so nothing can open a browser. A fresh `GROK_HOME`
also means no cached OAuth token (02-authentication.md: the API key authenticates when no session
token is active).

Honest scope: the mothership-side push of the xAI key into boot secrets (`sessions.rs`, alongside
the Claude/TypeSafe keys) and gateway routing are **follow-ups, not in this slice**. Today the key
reaches a colony only if you add `XAI_API_KEY` for host `api.x.ai` as a colony secret in Settings →
Secrets (`crates/colonizer/src/colony_secrets.rs`; `XAI_API_KEY` is not on that file's reserved
list), and grok then talks to `api.x.ai` directly.

## Nesting decisions

The microVM is the boundary. Inside it:

| Surface | Decision | How |
| :--- | :--- | :--- |
| grok OS sandbox | **off** | `--sandbox off` explicitly (18-sandbox.md) |
| Tool approval | **auto** | `--always-approve`: headless cannot answer a prompt; the microVM is the boundary |
| Web search/fetch | **off** | `--disable-web-search` (backend host unverified; egress denies it anyway) |
| Cross-session memory | **off** | `GROK_MEMORY=0` (05-configuration.md) |
| Telemetry | **off** | `GROK_TELEMETRY_ENABLED=0` (05-configuration.md) |
| Auto-update | **off** | `--no-auto-update` + `GROK_DISABLE_AUTOUPDATER=1` |
| Hooks / plugins / MCP / skills (user scope) | **off in practice** | a fresh, empty `GROK_HOME`: all of these live under it (14-headless-mode.md "File Locations") |
| Hooks / plugins / MCP / skills (project scope) | **not yet enforced** | no verified global off-switch; a colonized repo's `.grok/` could still contribute them (26-config-reference.md limits project config to MCP servers, plugins and permission) |

Subagents and plan mode are left at grok's defaults; `--no-subagents`/`--no-plan` exist if a later
slice wants them off.

## Tests

`npm test` (no dependencies; the module's `package.json` has none on purpose) boots the real
`runner.mjs` over stdio against `test/fake-grok.mjs`, a stub grok CLI that records the argv and env
it received, and checks the happy path's events against the required fields of
`docs/agent-events.schema.json`. CI covers only these stubbed contract tests.

## What remains

- Mothership-side xAI key push into boot secrets (`sessions.rs`) and provider-gateway routing for
  `xai-grok` models; today only a user-added `XAI_API_KEY` colony secret works.
- Binary fetch/lock/mount like `scripts/fetch-agent-binary.sh` + `vendor/claude-code.lock`, so a
  colony does not depend on grok being preinstalled in the image.
- Generic `requires.binaries` (and pins) preflight in Rust, so the harness fails a boot before the
  runner has to.
- Question routing via ACP or a colonizer MCP ask tool; `answer` is ignored today.
- The colonizer MCP tools (findings, memory, wait) that the Claude module serves.
- A compat-table row wherever the docs list agent modules, and a manual end-to-end run on a real
  colony with a real key.
