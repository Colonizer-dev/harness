# codex (agent module)

Drives OpenAI's [Codex CLI](https://developers.openai.com/codex) (`codex`, the `@openai/codex` npm
package) as a Colonizer agent module on the `colonizer-runner/1` protocol. **Status: SHIPPING as a
runner — the runner fetches the pinned `codex` binary into its cache on first boot, so a launch on
the stock preset images no longer stops at preflight. It has not yet been exercised in a real
colony run with that fetched binary.**

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

## The colonizer MCP server

Each turn registers a `colonizer` MCP server with codex (`-c mcp_servers.colonizer.*` overrides,
whose values are JSON and therefore valid TOML): `mcp.mjs`, a dependency-free stdio server that the
runner points at a loopback HTTP bridge held for the colony's life. `wait` (block instead of
polling; the server's `tool_timeout_sec` is raised to 3600 so a wait can run to its 1800 s cap),
`memory_briefing`, `memory_changes` and `memory_search` (shared memory is pulled through these, never
put into the prompt; issue #766) run inside the server; `finding_file` and `memory_propose` cross the bridge and
leave the colony as `finding` and `memory_proposal` events, and so do a loop colony's pacing tools:
`loop_next` (the next run's delay in minutes, clamped to 15–1440 like the mothership clamps it) and
`loop_stop`. The tool list follows the same switches as the other modules: findings only under
`COLONIZER_FINDINGS=true`, memory only when `COLONIZER_MEMORY_DIR` is mounted, the loop tools only
under `COLONIZER_LOOP=true` — `loop_next` additionally when `COLONIZER_LOOP_SELF_PACED=true` — and
`wait` always.

The first tool in the list follows no switch: **`ask_user`** is the question channel
(docs/protocol.md §2). A call POSTs to the bridge, which emits the `question` event, moves the
status to `waiting_for_answer`, and holds the HTTP response until the matching `answer` command
resolves it with `{answers, response}` and the status settles back. The `mcp_tool_call` item pair
codex streams for the call is dropped — a question is never also a `tool_call`/`tool_result` — and
while the call is parked `mcp.mjs` sends MCP progress notifications to hold it open (with
`tool_timeout_sec` at 3600, codex's own timeout outlives any human). An `answer` naming an unknown
id is warned about, and an interrupt, a turn end or a shutdown cancels every open ask
(`{cancelled: true}`).

## Headless, not app-server

Codex also speaks a bidirectional app-server protocol. This slice uses `codex exec` instead: one
process per turn, a read-only JSONL stream, no daemon to keep alive. The cost is that nothing
interactive crosses the boundary natively: `interrupt` SIGINTs the child (codex saves the session
rollout continuously, so the partial turn stays resumable) and ends the turn as an error.
Questions still reach the user — through the colonizer MCP server's `ask_user` (above), which the
`answer` command answers.

## Binary

At boot the runner uses `COLONIZER_CODEX_BIN`, then `codex` on `PATH`, else downloads the pinned
build for its architecture from the GitHub release assets, checks it against `codex.lock` (sha256,
before extraction), and reuses it on later boots. The tarball and the binary are cached on disk
under `$XDG_CACHE_HOME/colonizer/codex` (or `~/.cache/…`), not in `/tmp`: the colony's `/tmp` is a
small tmpfs. The rows are the static musl builds, `linux-x64` and `linux-arm64` (Codex publishes no
AVX2 split, unlike OpenCode), and each archive holds a single binary at its root. The download comes
from `github.com` release assets, served via `release-assets.githubusercontent.com` /
`objects.githubusercontent.com`, so an allowlist-egress colony needs all three hosts allowed (the
module declares them under `egress.extra`).

## Preflight

`module.json` pins `@openai/codex` **0.156.1** (upstream tag `rust-v0.156.1`), read back from
`module.json`. Before any codex process is spawned, each named problem emits a `log` error plus
`status error` with the name as `detail`:

- `CODEX_CREDENTIAL_MISSING` — neither `CODEX_API_KEY` nor `OPENAI_API_KEY` is set. Fix below.
- `CODEX_BINARY_MISSING` — the binary could not be resolved: no `codex` at `COLONIZER_CODEX_BIN`/PATH and the pinned build could not be fetched (no cache row for the platform, or a failed download); the log carries `npm install -g @openai/codex@0.156.1`.
- `CODEX_VERSION_DRIFT` — `codex --version` (prints `codex-cli X.Y.Z`) is not the pinned version.
- `CODEX_MODEL_PROVIDER` — a model setting that names a provider with no gateway route, or one whose route speaks the anthropic wire (see below).

## Credential story

`codex exec` reads **`CODEX_API_KEY`** from the environment and authenticates to
`api.openai.com` with it — no `codex login`, no browser (verified against 0.156.1; the upstream
docs name it as the variable for non-interactive runs). On the same probe, `OPENAI_API_KEY` alone
was **not** picked up by `codex exec` in a fresh `CODEX_HOME`, so the runner re-exports it to the
child as `CODEX_API_KEY` — either variable works for the runner, but name the colony secret
`CODEX_API_KEY`: the cockpit refuses colony secrets whose names start with `OPENAI_`, which are
reserved for the mothership's own credentials. The mothership also pushes the key in itself at boot
when it has one: the stored key of a provider named `openai`, else its own `OPENAI_API_KEY`, arrives
as `CODEX_API_KEY` for host `api.openai.com`, so a manual secret is only needed when neither is
configured. A ChatGPT plan sign-in is still not a credential
([#30](https://github.com/Colonizer-dev/harness/issues/30), [docs/decisions.md](../../../docs/decisions.md)).

A `<provider>/<model>` whose prefix matches a model route (`docs/protocol.md` §6.5) rides the
mothership's provider gateway instead: the runner points a codex `model_provider` at the gateway's
OpenAI passthrough and puts the per-colony token in its headers, so no OpenAI key is needed on those
turns and the stored key never leaves the mothership. The gateway meters the tokens and prices them,
so spend accounting and the colony's budgets apply. Only `openai`-wire routes fit — the gateway
serves the OpenAI paths on those alone — so an `anthropic`-wire route, or a prefix nobody
configured, is refused with `CODEX_MODEL_PROVIDER` before codex runs.

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
| Colony-disabled tools | **per setting** | the `disabled_tools` setting: `shell`, `web_search` and `view_image` become `-c features.shell_tool=false`, `-c web_search="disabled"` and `-c features.view_image=false`, passed with `--strict-config` (an exec flag) so a key codex stops recognising fails the turn loudly; `apply_patch` and MCP tools have no switch |
| Host config / OAuth token | **off in practice** | `CODEX_HOME` is the persisted `/root/.codex`, which each boot strips back to the session rollouts ([below](#session-resume)): no `config.toml`, no `auth.json` (auth is env-only), nothing else codex loads as config or instructions; `BROWSER=/bin/false` as belt-and-braces |
| Colonizer MCP tools | **on** | `-c mcp_servers.colonizer.*` overrides registering `node mcp.mjs`; gated on `COLONIZER_FINDINGS` and `COLONIZER_MEMORY_DIR` like every module, the loop tools on `COLONIZER_LOOP`/`COLONIZER_LOOP_SELF_PACED` (above, "The colonizer MCP server") |

The model setting (`COLONIZER_MODEL`) is passed as `-m <model>` when set: a bare id, `openai/<model>`
without a route, or the bare remainder of a routed `<provider>/<model>` (the gateway expects the
bare model, like the hermes runner). **Empty (the default) passes no `-m`, so Codex runs on the
CLI's own default model** — that is the module's documented default. `subagent_model` and
`background_model` are accepted for parity with the other modules but unused: headless `codex exec`
has no subagent or background-worker split.

## Session resume

`module.json` declares `/root/.codex` — the whole `CODEX_HOME` — as the module's `session_resume`
directory: the harness persists it outside the microVM and mounts it back on every boot, so the
session rollouts `codex exec resume` reads survive a stopped VM. The runner announces the thread id
as `agent_session` the moment `thread.started` names it (once per id, the same rule `model_changed`
follows), so a colony that is waiting on its user can be suspended
([#562](https://github.com/Colonizer-dev/harness/issues/562)) and booted again with
`COLONIZER_RESUME_SESSION` set: its first turn then runs `exec [options] resume <thread_id> -` and
the conversation continues in the same codex thread. If that thread's rollout is gone (the mount
changed under the colony), that first turn falls back to a fresh thread once instead of failing,
with a warning log and no `turn_end` for the failed attempt. The fallback has a cost worth knowing:
the harness delivers a held answer as the restored boot's only prompt, trusting the transcript to
carry the task brief ([boot.rs](../../../crates/colonizer/src/boot.rs)) — so a turn that continues
on a fresh thread starts from the answer alone, without the earlier conversation.

Persisting the home is safe because a boot keeps only the rollout store (`sessions/`,
`archived_sessions/`): everything else is deleted before any codex process runs — `config.toml`,
`auth.json`, `AGENTS.md` global instructions, `prompts/`, `skills/` and codex's sqlite state, none
of which resume needs (codex rebuilds its rollout index from the files it finds). That is the
whole "no config" row of the table above, enforced rather than assumed: whatever a previous boot
or the agent's own shell left in the writable home cannot survive as config, credentials or
instructions. Auth rides the `CODEX_API_KEY` environment variable and prompt history is off, so
the rollouts that do persist carry no credentials.

## Tests

`npm test` (no dependencies; the module's `package.json` has none on purpose) boots the real
`runner.mjs` over stdio against `test/fake-codex.mjs`, a stub codex CLI that records the argv,
stdin prompt and env it received, models the rollout store resume needs (a fresh thread writes
`$CODEX_HOME/sessions/<thread id>.jsonl`; `resume <id>` fails without its file), and — when a test
scripts `CODEX_FAKE_MCP_CALLS` — plays the
model against the registered colonizer MCP server, so findings, memory, the loop tools, wait and an
ask-and-answer round trip are tested end to end. `test/mcp.test.mjs` drives `mcp.mjs` directly. The
happy path's events are checked against the
required fields of `docs/agent-events.schema.json`. The binary resolver is tested with an injected
`fetchImpl`/`runTar` (no network, no tar): it prefers `COLONIZER_CODEX_BIN` and `PATH`, caches the
download, reuses it on a second call, and refuses a sha256 mismatch before extraction. CI covers
only these stubbed contract tests.

## What is not supported yet

- The fetched `codex` binary has not been through a real colony run yet: the tests cover the
  resolve-and-fetch path with a stub, but no end-to-end colony has booted on the downloaded build
  ([#602](https://github.com/Colonizer-dev/harness/issues/602)).
- The [exec policy](../claude-code/README.md#exec-policy) is not applied: the harness refuses to
  launch a codex colony while one is set (the install's `exec_policy` setting, or a repo
  `.colonizer/exec-policy.json`).
