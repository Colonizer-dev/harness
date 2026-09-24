# Pi agent module

Runs [Pi](https://github.com/earendil-works/pi-coding-agent) (`@earendil-works/pi-coding-agent`, pinned
in `package.json`) in its RPC mode and speaks the Colonizer runner contract (`docs/protocol.md` §2):
commands as JSON lines on stdin, events as JSON lines on stdout, diagnostics on stderr. The runner
spawns Pi once per session and drives it as a child process; Pi's framing splits records on LF bytes
only, so both of its streams are read through a splitter that leaves U+2028/U+2029 inside lines.

- Every `user_message` becomes a prompt; a message arriving mid-run joins it (`streamingBehavior:
  followUp`) instead of being refused.
- Text streams as `assistant_text_delta` and settles as `assistant_text`; tool calls, tool results
  (capped at 20 000 characters) and `turn_end` (cost, duration, per-model token counts) follow the
  protocol. One colonizer turn spans a Pi turn and every message queued into it (follow-ups): it
  ends at `agent_settled`, with the last assistant text as its `result`.

## Selection

One agent module per install: `agent.provider` in `~/.config/colonizer/modules.json` (`"pi"`), set in
the cockpit's agent picker (`PUT /api/modules/agent`).

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `COLONIZER_MODEL` | none | The model Pi runs on, as `<provider>/<model>` for a provider configured under Settings → Providers |
| `COLONIZER_EFFORT` | none | Thinking level passed as `--thinking`: `off`, `minimal`, `low`, `medium`, `high`, `xhigh` or `max` |
| `COLONIZER_MODEL_ROUTES` | none | JSON provider routes (`docs/protocol.md` §6.1); set by the mothership, not by hand |

Both settings are edited in the cockpit as the module's `model` and `effort` settings.

## Models

Pi reaches models only through the provider gateway: there is no Claude login inside the VM and no
credential for Anthropic, so a provider must be configured under Settings → Providers and the `model`
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

## What does not apply from the Claude Code module

Pi has no subagents, so `subagent_model`, `background_model` and `delegate` have no counterpart, and
neither does model tier routing (`route_per_task` with `model_low`/`model_high`): the `model` setting
is the only model. Pi has no way to ask a question — `answer` commands warn, and the appended system
prompt tells the model to choose and say so — and none of the in-process MCP tools exist: shared
memory (`memory_search`/`memory_propose`), the findings tool and `wait`. Briefs that name them ask
for the equivalent work done directly.

## Develop

```sh
npm ci
npm test          # fake-Pi tests, no network
printf '%s\n' '{"type":"user_message","id":"initial","text":"hello"}' | node runner.mjs
```
