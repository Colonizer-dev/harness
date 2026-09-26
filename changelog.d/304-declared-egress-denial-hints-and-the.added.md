- **Declared egress, denial hints, and the boundary/guidance split.** Every agent module's
  `module.json` now declares the hosts its agent may reach — `egress: {api, auth, telemetry,
  extra}`, bare hostnames, a leading `*.` allowed — validated by a test that walks every manifest
  and requires each `secrets[].hosts` entry to be covered; deriving a colony's enforceable
  allowlist from it is a follow-up egress-policy issue. The claude-code runner classifies denied
  tool calls (`classifyDenial(text)` → `egress`, `read_only` or `tool_disabled`) and adds
  `denial: {class, hint}` to the errored `tool_result` event — `is_error` and content unchanged —
  and repeats the hint to the agent itself through a `PostToolUseFailure` hook's
  `additionalContext`, at most once per class per session.
  Two docs: [docs/boundaries.md](docs/boundaries.md) — what the harness enforces (boundary) versus
  what it only suggests (guidance), how a finding classifies on arrival, and the planned watchdog
  signatures on denial events — and [docs/runner-authoring.md](docs/runner-authoring.md), the
  checklist for a new agent module. ([#304])

[#304]: https://github.com/Colonizer-dev/harness/issues/304
