# Claude Code agent module

Runs Claude Code through the [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk) and speaks
the Colonizer runner contract (`docs/protocol.md` §2): commands as JSON lines on stdin, events as JSON
lines on stdout, diagnostics on stderr.

- Streaming-input session: every `user_message` command becomes a turn (or joins the current one).
- Questions: Claude is told to ask only via `AskUserQuestion`. The call is routed through `canUseTool`
  and surfaced as a `question` event keyed by the tool-use id; the matching `answer` command resolves
  it. Under the default `delegate = enforce` the orchestrator itself is limited to planning, asking
  and delegating; every other tool stays with its subagents (the microVM is the sandbox).
- Text streams as `assistant_text_delta` and settles as `assistant_text`; tool calls, tool results
  (capped at 20 000 characters) and `turn_end` (cost, duration) follow the protocol.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `COLONIZER_CLAUDE_BIN` | `/opt/claude/bin/claude` | Native Claude Code binary |
| `COLONIZER_MODEL` | Claude Code default | Orchestrator model (carries nearly all the traffic): alias, ID or `<provider>/<model>` |
| `COLONIZER_SUBAGENT_MODEL` | orchestrator model | Default subagent model (`CLAUDE_CODE_SUBAGENT_MODEL`); used only when the agent delegates to one |
| `COLONIZER_BACKGROUND_MODEL` | Claude Code default | Background model for small auxiliary calls (`ANTHROPIC_DEFAULT_HAIKU_MODEL`) |
| `COLONIZER_MODEL_ROUTES` | none | JSON provider routes (`docs/protocol.md` §6.1) |
| `COLONIZER_MEMORY_DIR` | unset | Mounted shared memory; enables the memory tools (§6.2) |
| `COLONIZER_EFFORT` | model default | Orchestrator effort: `low`, `medium`, `high`, `xhigh` or `max` |
| `COLONIZER_SUBAGENT_EFFORT` | orchestrator effort | Effort for the `general-purpose` and `Explore` subagents, redefined with it (`subagents.mjs`); plugin agents keep the orchestrator's |
| `COLONIZER_ENFORCE_CHOICES` | on | Re-ask a plain-text question as a choice card once |

Credentials come from `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY` (a microsandbox placeholder in
the VM).

## Model routing

`COLONIZER_MODEL` is the orchestrator's model, and the orchestrator does nearly all of the work in a
colony, so this setting carries almost all of the traffic. `COLONIZER_SUBAGENT_MODEL` only applies
when the agent delegates to a subagent, and colonies rarely do: four recent colonies of 413 to 2 095
events spawned 0, 0, 0 and 1 between them. `COLONIZER_BACKGROUND_MODEL` carries Claude Code's small
auxiliary calls. A provider reachable only through the subagent and background settings is configured
correctly and will still see almost nothing; to put real traffic on your own hardware, point
`COLONIZER_MODEL` at it.

Which model `COLONIZER_MODEL` carries can be chosen per task. With the agent module's `route_per_task`
setting on (the default), the mothership reads the issue in front of a colony and boots it on one of
three tiers: `low` runs on the `model_low` setting, `high` on `model_high`, and `medium` on `model`.
Both tier settings accept the same forms as `model` — a Claude alias or ID, or `<provider>/<model>` —
and a blank tier setting falls back to `model`, so with neither tier model set nothing changes about
which model a colony runs on; `route_per_task: false` puts every colony that was not started with an
explicit tier on `model`. Only the orchestrator model is routed — the subagent and background settings
are untouched — and the tier settings are read on the mothership alone: their env vars are stripped
from the colony's environment once the tier is chosen, so only the provider actually in use is probed
at boot. What the rule reads off an issue, and what it records, is `docs/protocol.md` §6.1b.

When a route or a `<provider>/<model>` model is configured, `router.mjs` listens on `127.0.0.1` and
Claude Code's `ANTHROPIC_BASE_URL` points at it. A request whose `model` starts with a route prefix
(`deepseek/deepseek-flash`) goes to that route's `base_url` with the prefix stripped, the route's key
(`x-api-key`, `Bearer`, or none), and without the Anthropic credential or `oauth-*` betas. Everything
else passes through to `https://api.anthropic.com` unchanged, so a subscription login keeps working for
the orchestrator. Routed `count_tokens` calls the provider doesn't support get an estimate. Provider key
variables are removed from Claude Code's own environment.

Claude Code sends its full request shape to routed providers, including `thinking`, `context_management`,
`output_config`, `metadata`, every tool definition and betas such as `context-management-*` and
`advisor-tool-*`. Providers that reject unknown fields need to ignore them; a provider on the gateway's
`openai` wire gets a rebuilt request without them (docs/protocol.md §6.5).

## Shared memory

With `COLONIZER_MEMORY_DIR` set, the agent gets two auto-allowed tools from an in-process MCP server
(`colonizer_memory`): `memory_search` searches `{repo,org,global}/notes/*.md`, and `memory_propose`
emits a `memory_proposal` event for review on the mothership (with review off, a repo note is stored
straight away; org and global notes always wait for review). Nothing is written inside the colony.

## Waiting

Every colony also gets `mcp__colonizer_wait__wait` from an in-process MCP server (`colonizer_wait`),
with no setting to switch it on: a colony that cannot block burns model turns polling. It takes a
one-line `reason` (so the transcript says what the wait was for) and exactly one of: `seconds`, to
sleep; `file` and `pattern` (a JavaScript regex), to return as soon as a line of the file matches —
the file need not exist yet, it is read incrementally from the last byte offset, and the read starts
over when the file shrinks or its inode changes under the same path (truncated, or rotated by
rename). The watcher assumes the file is appended to: a same-inode rewrite that leaves the file at
least as long as the offset already read cannot be detected. A path that exists but is not a regular
file — directory, FIFO, socket, device — is refused as plain text, because opening a FIFO with no
writer blocks inside the threadpool and would never reach the timeout or an abort. A final line with
no trailing newline still matches, and is flagged as unterminated. Or `pid`, to return when that
process is gone (a disappearance, not an exit status — the colony cannot reap a process it did not
spawn, and an unreaped zombie still answers the liveness check, so a wait on one reports it as still
running). `seconds` and the `pid`/`file` timeout (default 300 s) cap at 1800 s and clamp with a note
rather than erroring. A timeout on a file wait returns the file's last lines, read from at most its
final 64 KiB, so one call is enough to see where a stuck build is. The tool description itself
carries the "use this instead of a grep poll loop or `Bash true`" guidance, because subagents see
descriptions but not the system prompt.

One honest limit on the `pattern`: it is evaluated by the runner's own event loop, so the timeout
bounds the waiting, not the regex evaluation. A pathological pattern — catastrophic backtracking,
the classic `(a+)+$` against a long line — can freeze the runner for far longer than any timeout.
Keep patterns simple: a literal substring or a simple regex.

## Develop

```sh
npm ci            # production: npm ci --omit=dev
npm test          # fake-SDK tests, no network
printf '%s\n' '{"type":"user_message","id":"initial","text":"hello"}' | node runner.mjs
```
