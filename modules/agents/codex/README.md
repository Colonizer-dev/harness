# codex (agent module)

Drives OpenAI's [Codex CLI](https://developers.openai.com/codex) (`codex`, the `@openai/codex` npm
package) as a Colonizer agent module on the `colonizer-runner/1` protocol. **Status: SHIPPING as a
runner — the `codex` binary is not staged into the colony image yet, so a colony stops at this
module's preflight until you put the pinned CLI on the image's PATH.**

The runner is `runner.mjs`: one headless `codex exec --json` process per turn, the prompt on stdin
(`-` as the prompt argument — an issue brief can be far larger than an argv slot), the first turn's
`thread.started` event yields the codex thread id, and every later turn resumes it with
`exec [options] resume <thread_id> -`, so a colony is one continuous codex thread. Streaming events
map to protocol events (`item.completed` with `agent_message` → `assistant_text`, `reasoning` →
`thinking`, the tool items (`command_execution`, `file_change`, `mcp_tool_call`, `web_search`) →
`tool_call` on `item.started` and `tool_result` on `item.completed`; `turn.completed` → `turn_end`).
`codex exec --json` has no text deltas — each agent message arrives whole — and reports tokens,
never cost, so `turn_end.cost_usd` stays null (the hermes runner's precedent). Top-level `error`
events are transient upstream (reconnect notices) and are logged, not fatal; only `turn.failed`
fails the turn. Unknown event types and non-JSON lines are logged, never fatal. Every flag and
event field below is from the upstream [non-interactive mode
docs](https://developers.openai.com/codex/noninteractive) and the [config
reference](https://developers.openai.com/codex/config-reference), verified against `codex-cli
0.156.1`.

## Headless, not app-server

Codex also speaks a bidirectional app-server protocol. This slice uses `codex exec` instead: one
process per turn, a read-only JSONL stream, no daemon to keep alive. The cost is that nothing
interactive crosses the boundary: an `answer` command is logged and ignored (the agent cannot ask
the user anything yet), and `interrupt` SIGINTs the child (codex saves the session rollout
continuously, so the partial turn stays resumable) and ends the turn as an error. Question routing
is the follow-up.

## Pinned binary and preflight

`module.json` pins `@openai/codex` **0.156.1** (upstream tag `rust-v0.156.1`). The runner resolves
the binary from `COLONIZER_CODEX_BIN`, else `codex` on the colony's PATH, and reads the pin back
from `module.json`. Before any codex process is spawned, each named problem emits a `log` error
plus `status error` with the name as `detail`:

- `CODEX_CREDENTIAL_MISSING` — neither `CODEX_API_KEY` nor `OPENAI_API_KEY` is set. Fix below.
- `CODEX_BINARY_MISSING` — no codex at `COLONIZER_CODEX_BIN`/PATH; the log carries `npm install -g @openai/codex@0.156.1`.
- `CODEX_VERSION_DRIFT` — `codex --version` (prints `codex-cli X.Y.Z`) is not the pinned version.
- `CODEX_MODEL_PROVIDER` — a model setting naming another provider than `openai/<model>`.

## Credential story

`codex exec` reads **`CODEX_API_KEY`** from the environment and authenticates to
`api.openai.com` with it — no `codex login`, no browser (verified against 0.156.1; the upstream
docs name it as the variable for non-interactive runs). On the same probe, `OPENAI_API_KEY` alone
was **not** picked up by `codex exec` in a fresh `CODEX_HOME`, so the runner re-exports it to the
child as `CODEX_API_KEY` — either secret works, but name the colony secret `CODEX_API_KEY`. Like every colony credential, the key is added in Settings →
Secrets for host `api.openai.com`; a ChatGPT plan sign-in is still not a credential
([#30](https://github.com/Colonizer-dev/harness/issues/30), [docs/decisions.md](../../docs/decisions.md)).

## Nesting decisions

The microVM is the boundary. Inside it:

| Surface | Decision | How |
| :--- | :--- | :--- |
| Tool approval | **auto** | `--dangerously-bypass-approvals-and-sandbox`: headless cannot answer a prompt |
| Codex OS sandbox (landlock/seccomp) | **off** | the same flag; the microVM is the colony's boundary |
| Git-repo check | **skipped** | `--skip-git-repo-check`: the runner may sit anywhere |
| Update check | **off** | `-c check_for_update_on_startup=false` |
| Prompt history | **off** | `-c history.persistence="none"`; the session rollout persists (resume needs it) |
| Telemetry (statsig metrics) | **off** | `-c otel.metrics_exporter="none"` |
| Host config / OAuth token / MCP | **off in practice** | a fresh, empty `CODEX_HOME` (`mkdtemp`): all of these live under it; `BROWSER=/bin/false` as belt-and-braces |

The model setting (`COLONIZER_MODEL`) is passed as `-m <model>` when set, as `openai/<model>` or a
bare model id; **empty (the default) passes no `-m`, so Codex runs on the CLI's own default model**
— that is the module's documented default. `subagent_model` and `background_model` are accepted
for parity with the other modules but unused: headless `codex exec` has no subagent or
background-worker split.

## Tests

`npm test` (no dependencies; the module's `package.json` has none on purpose) boots the real
`runner.mjs` over stdio against `test/fake-codex.mjs`, a stub codex CLI that records the argv,
stdin prompt and env it received, and checks the happy path's events against the required fields of
`docs/agent-events.schema.json`. CI covers only these stubbed contract tests.

## What is not supported yet

- The `codex` binary in the colony image: nothing fetches or stages it (see the grok-build module's
  "What remains" for the same gap); until then the preflight fails a codex colony at boot.
- Mothership-side push of the OpenAI key into boot secrets (`sessions.rs`), like grok-build's.
- Questions (`answer` is ignored), the colonizer MCP tools (memory, findings, wait), and resuming a
  codex thread across a runner restart: the thread id lives in the runner's memory and its session
  rollout in the runner's fresh `CODEX_HOME`, both gone when the colony's VM is.
