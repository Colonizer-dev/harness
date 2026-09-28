# acp (agent module)

Drives any [Agent Client Protocol](https://agentclientprotocol.com) agent (Zed's ACP: JSON-RPC 2.0
over stdio, newline-delimited) as a Colonizer agent module on the `colonizer-runner/1` protocol.
**Status: PLANNED — the runner is in-tree and verified against the real Gemini CLI handshake
([#509](https://github.com/Colonizer-dev/harness/issues/509)); nothing stages the `gemini` binary
into the colony image yet, and no end-to-end colony run has happened.**

The runner is `runner.mjs`: one long-lived ACP agent process per colony. At boot it negotiates
`initialize` (protocolVersion 1, clientCapabilities `fs.readTextFile`/`fs.writeTextFile` and
`terminal`) and opens one `session/new` in the workspace; every `user_message` becomes one
`session/prompt` turn, queued when one is already running. When the agent advertises
`agentCapabilities.loadSession` at `initialize`, the runner announces its session id as
`agent_session`, and a resume boot (`COLONIZER_RESUME_SESSION`: the harness continuing a colony
that waited on its user) reloads that session with `session/load` instead — the `session/update`s
the agent replays of the old conversation are history the harness already logged, never new
events. A load that fails, or an agent without `loadSession`, falls back to a fresh `session/new`.

## Mapping

| ACP | colonizer-runner/1 |
| :--- | :--- |
| `session/update` `agent_message_chunk` | `assistant_text_delta`, plus one final `assistant_text` at turn end |
| `agent_thought_chunk` | accumulated into one `thinking` at turn end (as the grok-build runner does) |
| `tool_call` | `tool_call` (`title`/`kind` as the name, `rawInput` as the input) |
| `tool_call_update` `completed`/`failed` | `tool_result` (`is_error` on `failed`; content blocks and `rawOutput` flattened to text) |
| `plan` | a `thinking` checklist (`Plan:\n- [x] …`) — the runner protocol has no plan event |
| `session/request_permission` | a `question` (below), `status waiting_for_answer` |
| `user_message_chunk`, `available_commands_update`, `current_mode_update` | ignored (no counterpart) |
| unknown update type | a `warn` log naming it, never fatal |
| `session/prompt` response `stopReason` | `turn_end`: `refusal` and `cancelled` (an interrupt) end the turn as an error; `end_turn`/`max_tokens`/`max_turn_requests` do not |
| `fs/read_text_file`, `fs/write_text_file` | served from the workspace (below) |
| `terminal/create\|output\|wait_for_exit\|kill\|release` | child processes started with a workspace cwd (below) |
| any other agent→client method | JSON-RPC error `-32601` |

Interrupt sends `session/cancel` and answers every open permission request `cancelled`, so the turn
ends as `interrupted by the user` and the runner keeps serving turns. `set_model` rides
`session/set_model` — but only when the agent advertised model selection at `session/new`
(`models.currentModelId`, which is also announced once as `model_changed`); otherwise it is a
warning. On `shutdown` or stdin EOF the agent gets a `session/cancel` if a turn is running, then
SIGTERM (SIGKILL after 2 s), `status exited`, exit 0. An agent that dies on its own is a named
`ACP_AGENT_FAILED` log plus `status error`, the turn in flight ends as an error, and the runner
exits 1.

Before the first turn, the runner checks its setup and names what is wrong: `ACP_AGENT_UNKNOWN`
(an `agent` that is not a preset, or `custom` with an empty command) or `ACP_CREDENTIAL_MISSING`
(the Gemini preset without `GEMINI_API_KEY`). Every turn then ends as an error carrying that name.

## Questions

`session/request_permission` becomes a `question`: the ACP options (clamped to 2–4 — a synthetic
`Cancel` pads a one-option card) as labels, the tool call's `title` as the question text, and the
risk class from the tool call's ACP `kind` — `read`/`search`/`fetch`/`think` are `read_only`,
everything else `workspace_write` (the higher §2 classes need a story ACP cannot tell, so they are
never claimed). The `answer` command resolves the request: the option whose label matches the
chosen answer is replied as `{outcome: {outcome: "selected", optionId}}`; an unmatched label or a
free-text `response` replies `{outcome: {outcome: "cancelled"}}` — ACP has no free-text answer —
and a `question_answered` event travels back like the Claude module's. An interrupt replies
`cancelled` and answers nothing.

## Workspace confinement

`fs/*` requests resolve every path against the runner's working directory (the workspace): the
longest existing ancestor is resolved through `realpath`, so `../`, an absolute path outside, and a
symlink pointing out of the tree are all refused with JSON-RPC error `-32602` before the filesystem
is touched — and a file over 16 MiB is refused rather than buffered whole. Terminal commands are a
weaker fence: they are only *started* with a `cwd` inside the workspace (default: the workspace
root) — what a command then does with its arguments, env and paths is the agent's business, and the
colony VM is the boundary. Their combined output is capped at `outputByteLimit` (default 16 000
bytes), truncated from the beginning past the limit with the `truncated` flag set.

## Settings

| Setting | Env | What it does |
| :--- | :--- | :--- |
| `agent` | `COLONIZER_ACP_AGENT` | `gemini` (the verified preset: `gemini --experimental-acp`) or `custom` |
| `command` | `COLONIZER_ACP_COMMAND` | With `custom`: the full command line including arguments, quotes respected |
| `model` | `COLONIZER_MODEL` | Meant to pick the model with `session/set_model` at boot, but **not applied yet**: the runner never reads `COLONIZER_MODEL`, so the agent runs on its own default. Switching the model from the cockpit (a `set_model` command) does work, when the agent advertises models |

## Verified and planned agents

- **Gemini CLI (`gemini --experimental-acp`) — handshake verified.** The real CLI 0.61.0 completed
  `initialize` and `session/new` and sent a prompt to the API; a full colony run with a real key,
  tool calls and a pull request has not been done. Pinned in `module.json` (`requires.pins`
  carries the npm version and its sha512 integrity; install with
  `npm install -g @google/gemini-cli@<pinned>`). The colony authenticates with a `GEMINI_API_KEY`
  secret for `generativelanguage.googleapis.com` (declared in `secrets`/`egress`; add the value in
  the cockpit's Secrets view, as a colony secret for that host); the runner
  refuses to boot the preset without it (`ACP_CREDENTIAL_MISSING`) — the colony never runs the
  CLI's interactive OAuth login. Manual end-to-end: install the pinned CLI, export
  `GEMINI_API_KEY=…`, run `node runner.mjs`, and send
  `{"type":"user_message","id":"initial","text":"hello"}` on stdin — see the handshake the tests
  assert for what comes back.
- **Grok Build (`grok agent stdio`) — planned.** Grok speaks ACP too (see the grok-build module's
  README), but its ACP mode is unverified here; before listing it as a preset, its hosts belong in
  `egress` and its credential in `secrets`.
- **Any other ACP agent — by configuration.** Pick `agent: custom`, set the command line, and give
  the deployment the egress hosts and secret env the agent needs: this manifest only declares
  Gemini's, so a custom agent's network access is exactly what you declare for it.

## Limits

- No `plan` event type in colonizer-runner/1: plans render as a `thinking` checklist.
- Resume rides the module's `session_resume` dir: the harness persists `/root/.gemini` outside the
  VM, so the gemini preset keeps its conversation across a suspended colony's stop and
  continuation (`session/load`). A custom agent resumes only if it keeps its sessions there too;
  otherwise the resumed boot starts a fresh session.
- The autopilot/exec policy from [#471](https://github.com/Colonizer-dev/harness/issues/471) is not
  wired in: tool calls arrive with the colony's own egress and path policy as the only fence, and
  every permission question surfaces to the user.
- ACP names no token or cost figures, so `turn_end.cost_usd` is always null.

## Tests

`npm test` (no dependencies; the module's `package.json` has none on purpose) boots the real
`runner.mjs` over stdio against `test/fake-acp-agent.mjs`, a scriptable fake ACP agent, driven by
the custom-command setting. Every `session/update` type, the permission flow, the fs and terminal
surface, and the failure paths are covered. CI runs `npm test` in this directory (the "Test the ACP
runner" step). These stubbed tests are all CI exercises: the end-to-end colony job runs only the
claude-code module.
