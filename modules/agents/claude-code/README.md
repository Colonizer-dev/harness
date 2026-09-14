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
| `COLONIZER_MODEL` | Claude Code default | Model alias or ID |
| `COLONIZER_EFFORT` | model default | `low`, `medium`, `high`, `xhigh` or `max` |

Credentials come from `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY` (a microsandbox placeholder in
the VM).

## Develop

```sh
npm ci            # production: npm ci --omit=dev
npm test          # fake-SDK tests, no network
printf '%s\n' '{"type":"user_message","id":"initial","text":"hello"}' | node runner.mjs
```
