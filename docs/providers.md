# Model providers: connections and backends

A *connection* is one entry in Model providers (`providers.json` on the mothership): a `base_url`, a
credential, and a `wire` — `anthropic` (the default) or `openai`. Model traffic reaches a connection
through the provider gateway at `/providers/<id>/` (docs/protocol.md §6.5), which presents the
Anthropic Messages wire to the colony and translates an `openai`-wire connection in both directions.
This page is the compatibility map between those connections and the shipped agent backends, plus the
two switches that take tools away from a colony.

## Connection → backends

| Backend | `anthropic` wire | `openai` wire | Harness-level `disabled_tools` |
| :--- | :--- | :--- | :--- |
| `claude-code` | yes — unrouted models go straight to Anthropic; `<provider>/<model>` rides the gateway's Anthropic Messages route | yes — same route; the gateway translates the openai wire | yes — the `disabled_tools` setting (`COLONIZER_DISABLED_TOOLS`) becomes the SDK session's `disallowedTools` |
| `codex` | no | no — talks to `api.openai.com` directly with `CODEX_API_KEY`; it refuses every provider prefix but `openai/` and reads no model routes | not yet — the runner already passes `-c` config overrides; a tool switch would ride those |
| `grok-build` | no | no — talks to `api.x.ai` directly with `XAI_API_KEY`; it refuses every provider prefix but `xai-grok/` and reads no model routes | not yet — the runner sets only `GROK_*` env toggles (memory, telemetry, updater) |
| `hermes` | yes — one config provider per gateway route, `transport: anthropic_messages` | yes — same route; the gateway translates | not yet — the runner hardcodes `agent.disabled_toolsets` (whole toolsets, not a per-colony setting) |
| `opencode` | yes — one `@ai-sdk/anthropic` provider per gateway route at `<base_url>/v1` | yes — same route; the gateway translates | not yet — the generated inline config carries no per-tool entries |
| `pi` | yes — the runner writes `models.json` from the routes, `api: anthropic-messages` | yes — same; the gateway presents anthropic-messages to every guest | not yet — the runner configures models only |

`codex` and `grok-build` run against their vendor API from the colony's own secret, so no route
reaches them yet: no `model_map`, no spend accounting through the gateway, no connection-level tool
strip.

## `model_map` and wire names

A connection's optional `model_map` maps a canonical model name to the name sent on the wire:
`{"<canonical>": "<wire_name>"}`. An empty `wire_name` sends the canonical name as-is; a non-empty one
is sent verbatim, so a provider that only knows its own branding can be addressed by the canonical
name every setting uses. A non-empty `model_map` is authoritative: the connection serves only the
canonicals it lists, and boot refuses a model that is not among them.

## Taking tools away: two levels

- **Connection (`disabled_tools` on the provider).** A list of Claude Code tool names. The gateway
  strips those tools from the `tools[]` of every request served through that connection — `WebSearch`
  also strips the `web_search` server tool, `WebFetch` also `web_fetch`. The strip applies per
  connection, so it reaches exactly the backends whose traffic rides the gateway (see the table).
- **Harness (`disabled_tools` on the agent module).** Per backend, regardless of endpoint. Only
  `claude-code` ships one so far: the setting is passed to the runner as `COLONIZER_DISABLED_TOOLS`
  and becomes Claude Code's session `disallowedTools`.

Boot logs what was taken away, per colony, into `harness.jsonl`:
`tool '<name>' disabled (level: connection|harness, …)`.

## Boot fails fast

A colony that cannot reach the model it was configured with is refused at boot, not left to flail:

`backend '<agent id>' has no provider for model '<value>': <fix>`

with a fix in the message, when any of these holds:

1. the model's `<provider>/` prefix names no configured connection;
2. the connection's non-empty `model_map` does not list the model;
3. the connection is unreachable at launch and the route has no `fallback_model`.

Two more misconfigurations refuse the launch the same way, before any probe runs:

- a connection-policy row a save would have refused, in a hand-edited `providers.json` — the message
  names the file, the provider and the row:
  `providers.json: provider '<id>': model_map['<canonical>']: …` (or `disabled_tools['<tool>']: …`);
- an unknown tool in the harness `disabled_tools` setting — the message names the agent module and
  the known tools: `agent module '<id>' setting 'disabled_tools': unknown tool '<name>' (known: …)`.
