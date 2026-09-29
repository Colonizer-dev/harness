# Model providers: connections and backends

A *connection* is one entry in Model providers (`providers.json` on the mothership): a `base_url`, a
credential, and a `wire` — `anthropic` (the default) or `openai`. Model traffic reaches a connection
through the provider gateway at `/providers/<id>/` (docs/protocol.md §6.5), which presents the
Anthropic Messages wire to the colony and translates an `openai`-wire connection in both directions.
This page is the compatibility map between those connections and the shipped agent backends, the
two switches that take tools away from a colony, and the settings that decide what a connection may
spend and carry. Every field and route is in
[docs/protocol.md §6.5](protocol.md#65-provider-gateway-v12-issue-5).

## Connection → backends

| Backend | `anthropic` wire | `openai` wire | Harness-level `disabled_tools` |
| :--- | :--- | :--- | :--- |
| `claude-code` | yes — unrouted models go straight to Anthropic; `<provider>/<model>` rides the gateway's Anthropic Messages route | yes — same route; the gateway translates the openai wire | yes — the `disabled_tools` setting (`COLONIZER_DISABLED_TOOLS`) becomes the SDK session's `disallowedTools` |
| `acp` | no | no — talks to the agent's own API host (Gemini: `generativelanguage.googleapis.com`, grok: `api.x.ai`) with the colony's own secret; the runner passes model ids to `session/set_model` and reads no model routes | no — ACP names no per-tool switch, so the module declares no `disabled_tools` setting |
| `codex` | no | no — talks to `api.openai.com` directly with `CODEX_API_KEY` (or `OPENAI_API_KEY`); it refuses every provider prefix but `openai/` and reads no model routes | yes — native names (`shell`, `web_search`, `view_image`) become `-c features.shell_tool=false`, `-c web_search="disabled"` and `-c features.view_image=false`, and the runner passes `--strict-config` so a key codex stops recognising fails the turn instead of silently keeping the tool; `apply_patch` (how codex writes files) and MCP tools cannot be turned off |
| `grok-build` | no | no — talks to `api.x.ai` directly with `XAI_API_KEY`; it refuses every provider prefix but `xai-grok/` and reads no model routes | yes — native tool ids (`run_terminal_cmd`, `read_file`, `write_file`, `search_replace`, `grep`, `list_dir`, `web_fetch`, `Agent`) go to `--disallowed-tools`; `web_search` is always off already |
| `hermes` | yes — one config provider per gateway route, `transport: anthropic_messages` | yes — same route; the gateway translates | yes, toolset-granular — the names are Hermes toolsets (`terminal`, `file`, `web`, `browser`, `vision`, `code_execution`, `todo`, `session_search`, `image_gen`, …) added to `agent.disabled_toolsets`; single tools inside a toolset (only `write_file`, say) cannot be turned off |
| `opencode` | yes — one `@ai-sdk/anthropic` provider per gateway route at `<base_url>/v1` | yes — same route; the gateway translates | yes — native tool ids become `permission: {"*": "allow", <id>: "deny"}` entries in the generated inline config; `edit` covers write/edit/apply_patch (the three share one permission) |
| `pi` | yes — the runner writes `models.json` from the routes, `api: anthropic-messages` | yes — same; the gateway presents anthropic-messages to every guest | yes — native tool names (`read`, `bash`, `edit`, `write`, `grep`, `find`, `ls`) go to `--exclude-tools` |

`codex` and `grok-build` run against their vendor API from the colony's own secret, so no route
reaches them yet: no `model_map`, no spend accounting through the gateway, no connection-level tool
strip.

## `model_map` and wire names

A connection's optional `model_map` maps a canonical model name to the name sent on the wire:
`{"<canonical>": "<wire_name>"}`. An empty `wire_name` sends the canonical name as-is; a non-empty one
is sent verbatim, so a provider that only knows its own branding can be addressed by the canonical
name every setting uses. A non-empty `model_map` is authoritative: the connection serves only the
canonicals it lists, and boot refuses a model that is not among them.

The cockpit's provider form does not show `model_map` or `disabled_tools` yet. Set them with
`PUT /api/providers/{id}` or by editing `providers.json`; saving the provider from the cockpit keeps
whatever is there, because a `PUT` that leaves a field out keeps its saved value (an explicit empty
value clears it).

## Taking tools away: two levels

- **Connection (`disabled_tools` on the provider).** A list of Claude Code tool names. The gateway
  strips those tools from the `tools[]` of every request served through that connection — `WebSearch`
  also strips the `web_search` server tool, `WebFetch` also `web_fetch`. The strip applies per
  connection, so it reaches exactly the backends whose traffic rides the gateway (see the table).
- **Harness (`disabled_tools` on the agent module).** Per backend, regardless of endpoint. The
  setting reaches the runner as `COLONIZER_DISABLED_TOOLS`, and every shipped backend but `acp`
  turns it into its own CLI's switch — the table above says which, and what the names are. Each
  module declares the valid names itself, as the `x-known-tools` list on the setting's schema
  property, and the boot check validates the setting against that list — falling back to Claude
  Code's tool names for a module that declares none.

Boot logs what was taken away, per colony, into `harness.jsonl`:
`tool '<name>' disabled (level: connection|harness, …)`.

## Boot fails fast

A colony that cannot reach the model it was configured with is refused at boot, not left to flail:

`backend '<agent id>' has no provider for model '<value>': <fix>`

with a fix in the message, when any of these holds:

1. the model's `<provider>/` prefix names no configured connection;
2. the connection's non-empty `model_map` does not list the model;
3. the connection is unreachable at launch and the route has no `fallback_model`. Before refusing, the
   boot probes the connection again rather than trusting the cached answer, so an outage that ended a
   moment ago does not block the launch. A connection with a `fallback_model` only gets a warning in
   the colony log: its requests go to that Claude model instead.

Only the connections this colony's model settings name (`model`, `subagent_model`,
`background_model`, `small_model` where the agent module has one, and `model_low`/`model_high` when
per-task routing picks one) are checked, so an
unrelated connection that is down blocks nothing.

Two more misconfigurations refuse the launch the same way, before any probe runs:

- a connection-policy row a save would have refused, in a hand-edited `providers.json` — the message
  names the file, the provider and the row:
  `providers.json: provider '<id>': model_map['<canonical>']: …` (or `disabled_tools['<tool>']: …`);
- an unknown tool in the harness `disabled_tools` setting — the message names the agent module and
  the tools it knows: `agent module '<id>' setting 'disabled_tools': unknown tool '<name>' (known: …)`.

## Plans, quotas and trust

A connection carries a few more settings. `pricing` and `quota` are edited in Settings → Providers.
`trusted`, like `model_map` and `disabled_tools`, is not in the cockpit yet: the API accepts it
(`PUT /api/providers/{id}`) and `providers.json` holds it, but `GET /api/providers` does not return it.

- **Which colonies may use it.** A colony's gateway token opens only the connections its model settings
  route to, and only for the models they name: both are recorded at boot, before the token is written,
  and a colony with no recorded set reaches nothing. A request for any other connection or model is
  refused with `403`, so one colony cannot spend on a provider or a model it was not configured for.
- **`pricing`** — dollars per million input, output, cache-read, cache-write and thinking tokens. Unset,
  routed requests cost `$0` but their tokens are still counted, so the sandbox module's `budget_tokens`
  still holds a colony on a prepaid plan that `budget_usd` never can.
- **`quota`** — `{"url", "pointer"}`, where to read what is left in a prepaid token plan. The gateway sends
  a `GET` to `url` with the connection's own credential, so `url` must have the same scheme, host and
  port as `base_url`, and reads the number (or numeric string) at `pointer`, an RFC 6901 JSON pointer.
  The provider health check (`GET /api/providers/{id}/health`) then answers `quota: {remaining, error}`,
  and the provider card shows "N left in plan". A failed quota read never marks the connection
  unreachable. `quota` omitted on a `PUT` keeps the saved probe; an empty `url` clears it.
- **Moving a connection.** The credential is sent wherever `base_url` points, so a `PUT` that moves a
  keyed connection to a different origin — scheme, host or port — is refused unless the API key is
  entered again (`api_key`) or removed (`""`) with the save. A path change on the same origin is the
  same party and keeps the saved key, and a connection with no key stored moves freely. A saved
  `quota` probe left on the old origin is refused the same way: move or clear it in the same save.
- **Quota exhaustion.** When a provider answers `429` or `403` with a message that says the plan ran out
  (not a plain rate limit), the gateway records it as exhausted until the reset the message names, or
  for 15 minutes when it names none. With a `fallback_model`, the request is retried on that Claude
  model. `COLONIZER_QUOTA_FALLBACK=0` turns that failover off for every connection at once. When every
  connection the colonies route to is exhausted, the queue pauses, and a colony whose turn died on the
  plan is stopped with its worktree kept until the provider recovers.
- **Out-of-quota card.** Colonies blocked on an exhausted connection (every request since their last
  success answered with the quota error, and no `fallback_model` retry) show on one "Provider out of
  quota" card per connection — in the inbox's "Needs you" list and at the top of Settings →
  Providers — with the model, the reset and a countdown. **Switch model** moves those colonies (or,
  with "this org", their orgs' model settings too) to a healthy model from the picker, which shows
  each model's failure rate, and restarts them on it; "remember" saves a Claude pick as the
  connection's `fallback_model`, so the next exhaustion retries on it by itself. **Wait until reset**
  parks them and resumes them at the reset. **Stop** stops them. The API is
  `GET /api/attention` and `POST /api/providers/{id}/quota-action` (docs/protocol.md §6.5).
- **`trusted`** — off by default. A colony whose task names restricted paths (secrets, `.env` files,
  infrastructure config) may only reach a connection marked `trusted: true`; any other answers `403`
  and the colony log says why.
- **`vetted`** — off by default, and implied by `trusted`. Paths a repository classifies `vetted`
  in `.colonizer/sensitivity.toml` need a connection marked `vetted: true` or better; the looser
  classes (`open`, `standard`, `custom`) run on any connection unless an org's settings raise their
  bar. Omitted on a `PUT` — or `null`, which counts as omitted — it keeps the saved value.
- **`vendor`** — the organisation that actually runs the model behind the endpoint (`"anthropic"`,
  …), as the operator records it. It gates nothing on its own: an org can pin restricted work to a
  list of vendors, and a connection with no vendor recorded never matches such a list. Omitted on a
  `PUT` — or `null` — it keeps the saved value; only an empty string clears it.
- **Sensitivity overrides** — an org's workspace settings can set the minimum mark per class:
  `"sensitivity": {"standard": "vetted", "restricted": "vetted", "restricted_vendors":
  ["anthropic"]}` in the org's `orgs.json` entry, each class one of `any`, `vetted` or `trusted`,
  `null` inheriting the built-in default. `restricted` can be loosened to `vetted` but never to
  `any`, and a vendor list, when set, must name at least one vendor, matched case-insensitively
  against the recorded `vendor` (docs/audit.md, Security-aware routing).
