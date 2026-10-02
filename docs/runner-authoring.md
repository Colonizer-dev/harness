# Runner authoring

The checklist a new agent module has to pass before it ships — the list the OpenCode module was
held to ([#202](https://github.com/Colonizer-dev/harness/issues/202), which the per-org module pick
[#201](https://github.com/Colonizer-dev/harness/issues/201) makes swappable). The wire contract
itself is [protocol.md](protocol.md) §2: commands on the runner's stdin, events on its stdout, one
JSON object per line. This page is everything beside it.

Before you write a runner, check whether your agent speaks the Agent Client Protocol. If it does,
the existing `acp` module may already drive it; see [The ACP runner](#the-acp-runner) below.

1. **module.json.** The mothership discovers every `modules/agents/<id>/module.json` at start-up
   (`read_agent` in `crates/colonizer/src/modules.rs`). A manifest it cannot read is named as a
   problem and the module is not offered. The keys:

   | Key | Read by the harness? | What it does |
   | :--- | :--- | :--- |
   | `id` | yes, required | The module id: what Settings, `agent.provider` and an org's `agent.module` name |
   | `entry` | yes, required | A non-empty array of strings, the runner command. An argument that is a file in the module directory is rewritten to its in-VM path under `/opt/colonizer/agent/` (the module mounts there read-only); a command starting with `node` also gets the vendored Node runtime mounted |
   | `name`, `description` | yes | What the pickers show |
   | `settings` | yes | The settings schema, the JSON Schema subset in [protocol.md](protocol.md) §4. Each property's `env` names the environment variable the runner receives the value in; an empty value is not passed at all |
   | `secrets` | partly | `[{ "env": [...], "hosts": [...] }]`: the credentials the agent uses and the hosts they are for. Every host must be covered by `egress` (item 2). A secret env named `CLAUDE_CODE_OAUTH_TOKEN` marks the module as needing a Claude login |
   | `requires` | mostly | `binaries` names what must be on the colony's `PATH`: a list containing `claude` marks the module as needing a Claude login, and the whole list is preflighted at launch and boot. On a stock preset image a binary is accepted when the harness stages it (`claude`), the runner fetches it (listed under `fetched_by_runner`), or it is one of the base tools every preset carries (`bash`, `gzip`, `sh`, `tar`, `wget`); anything else — no agent CLI is staged or shipped — refuses with the binary and, when the module pins it, the pinned version and install command. `pins` holds the versions, applied to a binary when the pin's key is the binary's name; a pin whose key is a package name (`@google/gemini-cli`) is carried for runners to read. A custom `sandbox.image` is trusted here and only checked by the runner's in-VM preflight. The free-text `image` description is not interpreted |
   | `egress` | yes | Item 2 |
   | `session_resume` | yes, optional | Item 9 |
   | `loop_tools` | yes, optional | `"loop_tools": true` when the runner serves the loop MCP tools `loop_next` and `loop_stop` ([loops.md](loops.md)): a loop's brief only names the tools then, and the agent rows of `GET /api/modules` repeat the flag so the cockpit can warn. Anything but a boolean is a manifest error |
   | `kind`, `protocol` | no | Every shipped module sets `"kind": "agent"` and `"protocol": "colonizer-runner/1"`; do the same |

   Declaring an `exec_policy` setting is a promise that the runner enforces the [exec policy](../modules/agents/claude-code/README.md#exec-policy)
   in that variable (Claude Code and ACP do). Without it, the harness refuses to launch the module
   while a policy is set — the install's `exec_policy` setting or a repo `.colonizer/exec-policy.json`
   — rather than run the agent unguarded.

2. **Egress declaration.** Add `egress: { "api": [...], "auth": [...], "telemetry": [...],
   "extra": [...] }` — bare hostnames, a leading `*.` allowed for wildcards (subdomains only:
   `*.sentry.io` covers `o1.sentry.io`, not `sentry.io`). Any other key, or a scheme, port or
   path in a host, is a manifest error. A manifest whose `secrets[].hosts` names a host the egress
   union does not cover is refused at discovery. A test walks every `modules/agents/*/module.json`
   and fails with "missing egress declaration" if the section is absent.

   The declaration is enforced in `allowlist` mode (#601): the running module's `api`, `auth` and
   `extra` hosts join the colony's allow list (the union with the operator's `egress_allow`), so
   the agent reaches the hosts you capture below without the operator restating them. Keep
   `telemetry` out of the fence — it is recorded but never added, because there is no opt-in for
   telemetry; the operator lists a telemetry host in `egress_allow` if they want it. A host you
   declare is lowercased and must still parse as an operator entry (at least two labels); one that
   does not is dropped, which can only narrow reach. A declared host never reopens a blocked
   destination: the always-blocked deny set and the operator's `egress_block` compile ahead of every
   allow. Each boot records `module: {agent, allow}` — what the module contributed — in
   `<session dir>/egress.json` ([sandbox-network.md](sandbox-network.md#hosts-an-agent-module-declares)).
   A colony in the default `open` egress mode reaches the public internet anyway; the declaration
   there changes nothing, because the `public` profile already covers it.

   **Capture procedure.** The declaration should come from observation, not memory:

   - Boot a test colony on your agent in the default `open` egress mode (the harness passes
     `--net public` plus port-scoped host rules) with a logging proxy in the path (point the
     agent's model endpoint at the proxy). The harness has no network-log facility of its own —
     the proxy, or msb's userspace-stack logs, is the capture point. What the policy allows and
     denies is in [sandbox-network.md](sandbox-network.md).
   - Run a representative task: the login/auth flow, model listing, and one tool call that reaches
     the network (a package install, a fetch).
   - Collect the distinct hostnames from the proxy log.
   - Classify them: `api` is the model/agent service, `auth` its sign-in and token hosts,
     `telemetry` its usage and error reporting, `extra` the rest (update checks, CDNs).
   - Drop hosts the task's providers already cover: model traffic goes through the provider
     gateway, so a host only reached through a `<provider>/` route needs no entry.

   The claude-code declaration comes from a live capture of the pinned CLI (2026-09-28, Claude Code
   2.1.280 — [modules/agents/claude-code/README.md](../modules/agents/claude-code/README.md#egress));
   a credentialed model turn is still unobserved, so re-check it when the pin moves.
3. **Denial-hint hooks.** Classify errored tool results and attach `denial: {class, hint}` to the
   `tool_result` event — classes `egress`, `read_only`, `tool_disabled`, following
   `modules/agents/claude-code/denials.mjs`. Deliver the hint twice: on the event for the record,
   and to the agent itself through a `PostToolUseFailure` hook returning
   `hookSpecificOutput.additionalContext` — mid-turn, so it costs no extra turn — at most once per
   class per session, and never with a `decision` or permission field: the hint is advice, not a
   verdict. Never change `is_error`, the content or any return code. Ship a strip test — with the
   layer off, every event is identical apart from the `denial` field. Known limits: masked-path
   empty reads are not detectable from text, and tools the agent spawns through `sh` never reach
   your classifier — they rely on the runner-level hints. (The masked-path *attempt* itself is
   reported separately, as the `path_policy` event of issue #647 — `pathpolicy.mjs`.) Only the
   claude-code runner implements this layer today (`modules/agents/claude-code/runner.mjs`, the
   `PostToolUseFailure` hook).
4. **Preflight, naming the error.** Check the agent binary and everything it needs before the
   first turn, and refuse with a message naming the missing thing and the way out: the codex
   runner's `preflight` (`modules/agents/codex/runner.mjs:55-84`) refuses a missing credential, a
   missing binary (`CODEX_BINARY_MISSING`, with the path and the pinned install command) and a
   version other than the pin; Hermes refuses a terminal backend other than `local` by name.
   Report a refusal as an `error` log plus `status` `error` with the code as its `detail`. If the
   runner itself cannot start, agentd reports `cannot start agent runner` with the program name.
   Warn-and-degrade is against the house rule ([architecture.md](architecture.md),
   "Configuration: refuse loudly, never degrade silently").
5. **Fixtures.** Pin your runner's real output down in a test. The claude-code runner commits
   `test/fixtures/events.jsonl`, which its own tests and the Rust tests in
   `crates/colonizer/src/protocol.rs` and `events.rs` read. The other runners (codex, grok-build,
   hermes, opencode, pi, acp) check every event they emit against the required fields in
   [agent-events.schema.json](agent-events.schema.json) instead. Either is fine; do one.
6. **Tests.** A `"test"` script in `package.json` that runs `node --test` (most modules point it at
   `test/runner.test.mjs`); tests in `test/`; no framework, no custom runner. Drive the real
   `runner.mjs` over stdio against a stub of the agent's CLI, so the tests need no install, no key
   and no network. A module without dependencies may skip `package.json` entirely, as OpenCode does.
7. **Schema and protocol.** A new event type goes into
   [agent-events.schema.json](agent-events.schema.json) and into the web's event types
   (`AgentEventBody` in `web/src/types.ts`, listed in `web/src/agentEvents.test.ts`, which fails
   when the schema and the web disagree), in the same change. Add it to
   `crates/colonizer/src/protocol.rs` only if the harness itself acts on it; anything else lands on
   `AgentEvent::Other` and is still forwarded to the browser. agentd does not check event types: it
   forwards every stdout line that is a JSON object with a string `type`, and turns any other line
   into a `warn` log.
8. **CI.** `cargo test --workspace` walks every `modules/agents/*/module.json`: the egress test
   above, and a test that fails unless [providers.md](providers.md) has a compatibility-table row
   starting `` | `<id>` | `` for your module. Runner tests are not part of `cargo test`: add a named
   step for your module to the `runner` job in `.github/workflows/ci.yml` ("Test the ACP runner"
   is the most recent one). Both jobs must pass before merge.
9. **Session resume (optional).** If the runner can pick an old conversation back up, declare
   `session_resume: { "dir": "/absolute/in-vm/path" }` in `module.json`: the harness mounts a
   writable host directory over that path (the colony session dir's `transcripts/`), the runner
   announces its session id with an `agent_session` event as soon as it knows its conversation id, and
   a colony suspended while it waits on its user boots again with `COLONIZER_RESUME_SESSION` set
   ([#562](https://github.com/Colonizer-dev/harness/issues/562)) — the id goes to the backend's
   resume mechanism (Claude Code: the SDK's `options.resume`; codex: `codex exec … resume`; ACP:
   `session/load`) and the held answer arrives as the first user message.
   A colony is only suspended when both hold: the module declares `session_resume` and the runner
   has announced a session. Otherwise the colony keeps its microVM while its question waits, said
   once in the colony log. Shipped modules that declare it: Claude Code (`/root/.claude/projects`),
   Codex (`/root/.codex`) and ACP (`/root/.gemini`).

## The ACP runner

`modules/agents/acp` ([#509](https://github.com/Colonizer-dev/harness/issues/509)) drives any
[Agent Client Protocol](https://agentclientprotocol.com) agent — JSON-RPC 2.0 over stdio — and maps
it onto the runner protocol. It is a useful example of the checklist above, and it may save you
writing a runner at all. Its status is **planned**: the runner and its tests are in the tree, but
nothing stages the `gemini` binary into the colony image yet and no end-to-end colony run has
happened (module README).

- **Settings.** `agent` (`COLONIZER_ACP_AGENT`): `gemini` runs `gemini --experimental-acp` and
  `grok` runs `grok agent stdio` (both verified at the handshake; a full turn with a real key has
  not been run for either); `custom` runs the command line in `command`
  (`COLONIZER_ACP_COMMAND`). `model` (`COLONIZER_MODEL`) is sent with `session/set_model`, only
  when the agent advertises model selection.
- **Manifest.** `secrets` and `egress` declare Gemini's host
  (`generativelanguage.googleapis.com`, secret `GEMINI_API_KEY`) and grok's (`api.x.ai`, secret
  `XAI_API_KEY`). A `custom` agent's hosts and credential are whatever you set up for it as colony
  secrets.
- **Preflight.** It refuses an unknown preset or an empty custom command (`ACP_AGENT_UNKNOWN`) and
  a preset without its credential (`ACP_CREDENTIAL_MISSING`: `GEMINI_API_KEY` for `gemini`,
  `XAI_API_KEY` for `grok`); a credential the agent refuses at the handshake is
  `ACP_AUTH_FAILED`. It does not probe the binary first: a missing `gemini` shows up as
  `ACP_AGENT_FAILED` when the spawn fails.
- **Questions.** `session/request_permission` becomes a `question` with 2–4 options, its `risk`
  taken from the ACP tool kind (`read`, `search`, `fetch`, `think` are `read_only`, everything
  else `workspace_write`). ACP has no free-text answer, so "Other" replies `cancelled`.
- **Workspace.** `fs/read_text_file` and `fs/write_text_file` are confined to the workspace;
  `terminal/*` commands only start with a workspace `cwd`, and the VM is the boundary.
- **Limits.** Resume rides `session/load` when the agent advertises it (item 9); no denial hints, no
  `plan` event (plans arrive as a `thinking` checklist), and `turn_end.cost_usd` is always null.
- **Tests.** `npm test` drives the real `runner.mjs` against `test/fake-acp-agent.mjs`, and
  checks every event against the schema's required fields.

The full ACP-to-runner mapping is in `modules/agents/acp/README.md`.
