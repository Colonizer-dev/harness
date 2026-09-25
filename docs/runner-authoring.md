# Runner authoring

The checklist a new agent module has to pass before it ships — the list the OpenCode module was
held to ([#202](https://github.com/Colonizer-dev/harness/issues/202), which the per-org module pick
[#201](https://github.com/Colonizer-dev/harness/issues/201) makes swappable). The wire contract
itself is [protocol.md](protocol.md) §2; this page is everything beside it.

1. **module.json.** Declare the module per [protocol.md](protocol.md) §2: name, kind, settings
   schema, secrets.
2. **Egress declaration.** Add `egress: { "api": [...], "auth": [...], "telemetry": [...],
   "extra": [...] }` — bare hostnames, a leading `*.` allowed for wildcards. A test walks every
   `modules/agents/*/module.json` and fails with "missing egress declaration" if absent, and every
   `secrets[].hosts` entry must be covered by the egress union.

   **Capture procedure.** The declaration should come from observation, not memory:

   - Boot a test colony on your agent with a permissive network profile and a logging proxy in the
     path (point the agent's model endpoint at the proxy). What the harness itself passes is
     `public` plus port-scoped host rules, and it has no network-log facility of its own — the
     proxy, or msb's userspace-stack logs, is the capture point. What the policy allows and denies
     is in [sandbox-network.md](sandbox-network.md).
   - Run a representative task: the login/auth flow, model listing, and one tool call that reaches
     the network (a package install, a fetch).
   - Collect the distinct hostnames from the proxy log.
   - Classify them: `api` is the model/agent service, `auth` its sign-in and token hosts,
     `telemetry` its usage and error reporting, `extra` the rest (update checks, CDNs).
   - Drop hosts the task's providers already cover: model traffic goes through the provider
     gateway, so a host only reached through a `<provider>/` route needs no entry.

   The claude-code backfill in this PR came from the CLI's documented requirements and the
   manifest's secret hosts, not yet from a live capture; replace it when a capture runs.
3. **Denial-hint hooks.** Classify errored tool results and attach `denial: {class, hint}` to the
   `tool_result` event — classes `egress`, `read_only`, `tool_disabled`, following
   `modules/agents/claude-code/denials.mjs`. Deliver the hint twice: on the event for the record,
   and to the agent itself through a `PostToolUseFailure` hook returning
   `hookSpecificOutput.additionalContext` — mid-turn, so it costs no extra turn — at most once per
   class per session, and never with a `decision` or permission field: the hint is advice, not a
   verdict. Never change `is_error`, the content or any return code. Ship a strip test — with the
   layer off, every event is identical apart from the `denial` field. Known limits: masked-path
   empty reads are not detectable from text, and tools the agent spawns through `sh` never reach
   your classifier — they rely on the runner-level hints.
4. **Preflight, naming the error.** Check the agent binary and everything it needs before the
   first turn, and refuse with a message naming the missing thing and the way out: codex's
   preflight returns `MISSING_BINARY` with the path and the pinned install command
   (`modules/agents/codex/runner.mjs:56-73`); Hermes refuses a terminal backend other than `local`
   by name. Warn-and-degrade is against the house rule ([architecture.md](architecture.md),
   "Configuration: refuse loudly, never degrade silently").
5. **Fixtures.** Extend `test/fixtures/events.jsonl` in your module — claude-code keeps its event
   fixtures there — and test against them.
6. **Tests.** `"test": "node --test"` in `package.json`, tests in `test/`; no framework, no
   custom runner.
7. **Schema and protocol.** New event types go into
   [agent-events.schema.json](agent-events.schema.json) and
   `crates/colonizer/src/protocol.rs` in the same change — the mothership and agentd parse from
   both.
8. **CI.** `cargo test --workspace` walks every `modules/agents/*/module.json` (the egress test
   included) and runs the runner's `npm test`; both must pass before merge.
