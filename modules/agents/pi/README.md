# Pi agent module

Runs [Pi](https://github.com/earendil-works/pi-coding-agent) (`@earendil-works/pi-coding-agent`, pinned
in `package.json`) in its RPC mode and speaks the Colonizer runner contract (`docs/protocol.md` §2):
commands as JSON lines on stdin, events as JSON lines on stdout, diagnostics on stderr. The runner
spawns Pi once per session and drives it as a child process; Pi's framing splits records on LF bytes
only, so both of its streams are read through a splitter that leaves U+2028/U+2029 inside lines.

- Every `user_message` becomes a prompt; a message arriving mid-run joins it (`streamingBehavior:
  followUp`) instead of being refused.
- Text streams as `assistant_text_delta` and settles as `assistant_text`; tool calls, tool results
  (capped at 20 000 characters) and `turn_end` (duration, per-model token counts, and a cost that is
  normally 0: the gateway prices routed spend, and the runner's `models.json` gives Pi no prices)
  follow the protocol. One colonizer turn spans a Pi turn and every message queued into it (follow-ups): it
  ends at `agent_settled`, with the last assistant text as its `result`.

## Selection

Pick Pi as the mothership's agent module in the cockpit's agent picker (`PUT /api/modules/agent`,
saved as `agent.provider` `"pi"` in `~/.config/colonizer/modules.json`), or for one org only under
Org settings → Agent module; an org without a pick uses the mothership's choice.

## Status

The runner has driven the real Pi binary (0.87.1, pinned in `package.json`) through a tool turn and
a text turn, but only against a stand-in gateway outside a microVM; no colony launched from the
cockpit has run on it yet. CI runs the fake-Pi tests (`npm test` in this directory).

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `COLONIZER_MODEL` | none | The model Pi runs on, as `<provider>/<model>` for a provider configured under Settings → Model providers |
| `COLONIZER_EFFORT` | none | Thinking level passed as `--thinking`: `off`, `minimal`, `low`, `medium`, `high`, `xhigh` or `max` |
| `COLONIZER_DISABLED_TOOLS` | none | Pi tool names passed as `--exclude-tools`, e.g. `bash`, `write`, on top of the default read, bash, edit, write set; grep, find and ls are never enabled in a colony, so listing them disables nothing |
| `COLONIZER_MODEL_ROUTES` | none | JSON provider routes (`docs/protocol.md` §6.1); set by the mothership, not by hand |

All three settings are edited in the cockpit as the module's `model`, `effort` and `disabled_tools` settings.

## Models

Pi reaches models only through the provider gateway: there is no Claude login inside the VM and no
credential for Anthropic, so a provider must be configured under Settings → Model providers and the `model`
setting must name one of its models (`deepseek/deepseek-chat`). An empty or unmatched setting stops
the colony with instructions instead of falling back. The runner writes the colony's route into a
private `models.json` (mode 0600, deleted with the session): one provider speaking the gateway's
`anthropic-messages` wire, authenticated by the `x-colonizer-colony` header the gateway itself
verifies — the `apiKey` field is a placeholder the gateway drops. `contextWindow` comes from the
route's `context_tokens` when it has one. A route's `fallback_model` is a Claude model retried against
Anthropic outside the gateway, which Pi has no credential for, so it is not listed. `set_model` to
anything but the configured model warns: Pi reads `models.json` only at startup.

`effort` maps to `--thinking`, and the model is marked `reasoning` only when a level is requested — a
reasoning model without `--thinking` defaults to a thinking level, and a non-reasoning one clamps the
flag away.

Pi's own startup network (model-catalog refresh, version check, telemetry) is switched off with
`PI_OFFLINE`, `PI_SKIP_VERSION_CHECK` and `PI_TELEMETRY=0`: a colony's network allows nothing but the
gateway. `COLONIZER_MODEL_ROUTES` carries the colony's gateway token and is removed from Pi's
environment, so it never reaches the model's own shell commands.

## Shared memory

With `COLONIZER_MEMORY_DIR` mounted, the runner loads `memory-extension.mjs` by explicit path
(`--extension`; `--no-extensions` only stops discovery), and the extension registers three Pi
tools (issue #766): `memory_briefing` (a short, sourced summary, optionally on a topic),
`memory_changes` (entries added, and entries revoked or removed, since the colony last asked) and
`memory_search`. They read the mounted `notes.json` and note files and frame their answer as data to
verify, so a revoked note is gone from the next answer. Memory is pulled, never injected: the
system prompt gains one fixed line naming the tools (a second `--append-system-prompt`), and no
note text. The logic is `memory.mjs` (a copy of the claude-code module's) and `memory-mcp.mjs` (a
copy of the ACP module's), both kept byte-identical by `test/memory.test.mjs`, which also drives
the real Pi against a stand-in model endpoint to check the tools reach the model and the prompt
carries no note. `COLONIZER_DISABLED_TOOLS` can exclude them like any other Pi tool.

## What does not apply from the Claude Code module

Pi has no subagents, so `subagent_model`, `background_model` and `delegate` have no counterpart, and
neither does model tier routing (`route_per_task` with `model_low`/`model_high`): the `model` setting
is the only model. Pi has no way to ask a question — `answer` commands warn, and the appended system
prompt tells the model to choose and say so — and of the in-process MCP tools only shared memory's
read tools exist (below): `memory_propose`, the findings tool and `wait` do not. Briefs that name
them ask for the equivalent work done directly. The [exec policy](../claude-code/README.md#exec-policy) is
not applied either: the harness refuses to launch a Pi colony while one is set (the install's
`exec_policy` setting, or a repo `.colonizer/exec-policy.json`).

## Develop

```sh
npm ci
npm test          # fake-Pi tests, no network
printf '%s\n' '{"type":"user_message","id":"initial","text":"hello"}' | node runner.mjs
```
