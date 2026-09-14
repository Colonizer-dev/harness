# Claude Code agent module

Runs Claude Code through the [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk) and speaks
the Colonizer runner contract (`docs/protocol.md` §2): commands as JSON lines on stdin, events as JSON
lines on stdout, diagnostics on stderr.

- Streaming-input session: every `user_message` command becomes a turn (or joins the current one).
- Questions: Claude is told to ask only via `AskUserQuestion`. The call is routed through `canUseTool`
  and surfaced as a `question` event keyed by the tool-use id; the matching `answer` command resolves
  it. All other tools are allowed (the microVM is the sandbox).
- Text streams as `assistant_text_delta` and settles as `assistant_text`; tool calls, tool results
  (capped at 20 000 characters) and `turn_end` (cost, duration) follow the protocol.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `COLONIZER_CLAUDE_BIN` | `/opt/claude/bin/claude` | Native Claude Code binary |
| `COLONIZER_MODEL` | Claude Code default | Orchestrator model: alias, ID or `<provider>/<model>` |
| `COLONIZER_SUBAGENT_MODEL` | orchestrator model | Default subagent model (`CLAUDE_CODE_SUBAGENT_MODEL`) |
| `COLONIZER_BACKGROUND_MODEL` | Claude Code default | Background model (`ANTHROPIC_DEFAULT_HAIKU_MODEL`) |
| `COLONIZER_MODEL_ROUTES` | none | JSON provider routes (`docs/protocol.md` §6.1) |
| `COLONIZER_MEMORY_DIR` | unset | Mounted shared memory; enables the memory tools (§6.2) |
| `COLONIZER_EFFORT` | model default | `low`, `medium`, `high`, `xhigh` or `max` |
| `COLONIZER_ENFORCE_CHOICES` | on | Re-ask a plain-text question as a choice card once |

Credentials come from `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY` (a microsandbox placeholder in
the VM).

## Model routing

When a route or a `<provider>/<model>` model is configured, `router.mjs` listens on `127.0.0.1` and
Claude Code's `ANTHROPIC_BASE_URL` points at it. A request whose `model` starts with a route prefix
(`deepseek/deepseek-flash`) goes to that route's `base_url` with the prefix stripped, the route's key
(`x-api-key`, `Bearer`, or none), and without the Anthropic credential or `oauth-*` betas. Everything
else passes through to `https://api.anthropic.com` unchanged, so a subscription login keeps working for
the orchestrator. Routed `count_tokens` calls the provider doesn't support get an estimate. Provider key
variables are removed from Claude Code's own environment.

Claude Code sends its full request shape to routed providers, including `thinking`, `context_management`,
`output_config`, `metadata`, every tool definition and betas such as `context-management-*` and
`advisor-tool-*`. Providers that reject unknown fields need to ignore them.

## Shared memory

With `COLONIZER_MEMORY_DIR` set, the agent gets two auto-allowed tools from an in-process MCP server
(`colonizer_memory`): `memory_search` searches `{repo,org,global}/notes/*.md`, and `memory_propose`
emits a `memory_proposal` event for review on the mothership. Nothing is written inside the colony.

## Develop

```sh
npm ci            # production: npm ci --omit=dev
npm test          # fake-SDK tests, no network
printf '%s\n' '{"type":"user_message","id":"initial","text":"hello"}' | node runner.mjs
```
