# 6.5 Provider gateway (v1.2, issue #5)

Part of the [Colonizer protocol](../protocol.md).

Routed (non-Anthropic) model traffic goes through a gateway on the mothership instead of straight from
the colony. The mothership is on the operator's networks (tailnet, LAN), holds the provider keys, and
sees every colony, so it can enforce per-provider concurrency, long timeouts, health, fallback and the
per-colony spend budget.

The gateway listens on `127.0.0.1:41750` by default (`COLONIZER_GATEWAY_BIND`, an IP:port socket
address parsed once at startup; a malformed value refuses startup). Colonies reach it as
`http://host.microsandbox.internal:<port>` (41750 unless the bind says otherwise); a colony with any
route gets one extra allow rule for that port on top of its egress policy
([sandbox-network.md](../sandbox-network.md)). Provider keys never enter colonies.

**Routes.** `COLONIZER_MODEL_ROUTES` entries gain fields:

```json
[{"provider": "strix", "prefix": "strix/",
  "base_url": "http://host.microsandbox.internal:41750/providers/strix", "auth": "none",
  "wire": "openai",
  "headers": {"x-colonizer-colony": "<per-colony token>"},
  "timeout_secs": 900, "context_tokens": 131072, "fallback_model": "claude-sonnet-5"}]
```

`wire` is the provider's wire (`anthropic`, the default, or `openai`): a runner that speaks the OpenAI
wire itself reads it to know which routes serve its own paths untranslated (issue #629).

**Runner.**

- Adds a route's `headers` to every request routed through it.
- For routes referenced by `COLONIZER_MODEL`, `COLONIZER_SUBAGENT_MODEL`, `COLONIZER_BACKGROUND_MODEL`
  or, where the agent module has one, `COLONIZER_SMALL_MODEL`
  (the "used" routes), sets in Claude Code's environment:
  - when the largest `timeout_secs` is above 300: `CLAUDE_STREAM_IDLE_TIMEOUT_MS` = min(t·1000, 1800000),
    `API_TIMEOUT_MS` = t·1000 + 60000, `API_FORCE_IDLE_TIMEOUT` = `0`,
    `CLAUDE_ASYNC_AGENT_STALL_TIMEOUT_MS` = t·1000;
  - when any used route has `context_tokens`: `CLAUDE_CODE_MAX_CONTEXT_TOKENS` = the smallest of them.
- **Fallback.** When a routed request returns 502, 503 or 504 with an `x-colonizer-fallback` header and
  the route has `fallback_model`, resend the same request to Anthropic (as for unrouted models, with the
  headers Claude Code sent) with `model` set to `fallback_model`, and emit
  `{"type":"log","level":"warn","message":"provider strix unavailable (queue_timeout); used claude-sonnet-5"}`. A gateway that can't be reached at all also falls back (reason `gateway unreachable`). `thinking: {type: "enabled"}` is rewritten to `{type: "adaptive"}`, which current Claude models require.
  Without `fallback_model`, return the gateway's response unchanged.

**Gateway endpoint** `POST /providers/{id}/v1/messages`, plus `POST
/providers/{id}/v1/messages/count_tokens` on the `wire: anthropic`, and `POST /providers/{id}/v1/responses`
and `POST /providers/{id}/v1/chat/completions` on the `wire: openai` (below; the route is registered for any
method and path so that a refusal is audited like real traffic; the handler answers `405` to a wrong
method and `404` to any other path, and forwards a query string only if it stays within the path's
character set plus `=` and `&`):

- Requires `x-colonizer-colony` to match a live colony's token; otherwise `401`. The same token is also
  accepted as `Authorization: Bearer <token>` — what a runner speaking the OpenAI wire sends — and the
  gateway's own header keeps precedence when a request carries both. Neither credential is ever forwarded
  to a provider.
- What a token admits is recorded at boot, before the token is written: the providers and the
  `<provider>/<model>` pairs the colony's model settings name. A colony with no recorded set reaches
  nothing, and `403` `permission_error` is answered, before anything is sent upstream, when the
  provider is not in the record or the body's `model` — matched on the requested name, before any
  `model_map` renaming — is not.
- A colony past its spend budget is refused `403` `permission_error` before it waits for a slot, with no
  `x-colonizer-fallback`: there is nothing to fall back to. The same check stops the colony on the host,
  worktree kept, so raising the budget and resuming continues it.
- `wire: anthropic` (the default): forwards to the provider's `base_url` + `/{path}` + query, with `content-type`, `accept`,
  `anthropic-version` and `anthropic-beta` (minus `oauth-*` betas) plus the provider credential. It never
  forwards the client's `authorization` or `x-api-key`.
- `max_concurrent`: waits up to `queue_timeout_secs` for a slot, then answers `503`
  `{"type":"error","error":{"type":"overloaded_error","message":"…"}}` with `x-colonizer-fallback: queue_timeout`.
  A colony has at most 16 requests waiting for slots at once; past that a further request is refused
  `429` `overloaded_error` on arrival instead of joining the queue (agents fan out through parallel
  subagents, so bursts are routine, but a wait without bound is not).
- A request that waited re-checks once it holds its slot and is refused `403` `permission_error`
  without being sent — releasing what it held — if its token no longer matches that live colony or
  the budget no longer admits it.
- Other refusals, none of them with `x-colonizer-fallback`: `404` `not_found_error` for an unknown
  provider; `403` `sensitivity_error` when the task's sensitivity class exceeds the provider's mark —
  `vetted` work needs a provider marked `vetted`, `restricted` work one marked `trusted` (`trusted`
  implies `vetted`), with an org's sensitivity overrides able to move the bar (docs/providers.md).
  The launch resolves the orchestrator, subagent and background models against this class first
  (§6.1b), substituting an eligible model rather than letting a colony boot into calls this gate can
  only refuse; what it substituted is on the session's `model_substitutions`;
  `502` `api_error` when a keyed provider has no saved key; a second budget `403` when recorded spend
  plus in-flight estimates plus this request would pass the budget; `400`/`404` for a path or body
  the wire cannot carry.
- Connection or send failure: `502` `api_error` with `x-colonizer-fallback: unreachable`. No response headers within
  `timeout_secs`: `504` with `x-colonizer-fallback: timeout`. A response body silent for `timeout_secs`
  is ended.
- For a `text/event-stream` response, a silence of 15 s between events gets a `: keep-alive\n\n`
  comment (ignored by any SSE parser) instead of ending the body; this covers a long, silent GGUF
  prefill. Pings are only sent at an event boundary, never inside a partly forwarded event. They don't
  reset the `timeout_secs` deadline, so a provider that never sends a real byte still times out. Non-SSE
  bodies are never pinged.
- Errors use the Anthropic error shape so Claude Code reports them normally.
- A colony with a request in flight through the gateway counts as making progress for the watchdog.
- Every request that passes colony auth is one JSON line in the colony's `<session dir>/gateway.jsonl`,
  with its outcome and, on failure, the failure code `health.last_failure` reports.

**`openai` wire.** A provider with `wire: "openai"` speaks OpenAI's Chat Completions API. `POST /v1/messages`
is translated in both directions (`crates/colonizer/src/openai.rs`), and two of OpenAI's own routes pass
through untranslated (issue #629). The runner's fallback resends its own, untranslated request, so it is
unaffected.

- `POST /v1/responses` and `POST /v1/chat/completions` are forwarded to `{base_url}` + the same path (query
  kept; a `base_url` that already ends in `/v1` does not grow a second one, so the presets that store it
  that way — xai-grok's `https://api.x.ai/v1` — join normally), sent with `content-type` and the provider
  credential only. The connection policy's `model_map`
  renames the model exactly as on `/v1/messages`, and a streaming chat completion gains
  `stream_options.include_usage` unless the colony asked for usage itself — accounting needs the final
  chunk. Everything else forwards as sent; a body neither rewrite touches goes out byte-for-byte. Any
  other path on this wire — including `/v1/messages/count_tokens`, which has no OpenAI equivalent — answers
  `404` (the runner then estimates the token count itself), and a non-POST to a served path `405`, as on
  every wire. The passthrough is held to the same token, provider, model, budget and queue checks as
  `/v1/messages`: the body's top-level `model` is what the colony's recorded `<provider>/<model>` pairs
  are matched against, and an `anthropic`-wire provider does not serve these two paths at all.
- The request is rebuilt from an allowlist. `system`, and system messages inside `messages`, become
  `system` messages; text, images (base64 or URL) and PDF documents become content parts; `tool_use` becomes
  `tool_calls`, and `tool_result` becomes `tool` messages directly after them (images in a tool result move
  to a user message after the tool messages); tools with an `input_schema` become functions (server tools
  are dropped); `tool_choice` and `disable_parallel_tool_use` map across; `max_tokens` becomes
  `max_completion_tokens`; `stream` adds `stream_options.include_usage`. Everything else is dropped:
  `thinking`, `context_management`, `output_config`, `metadata`, `cache_control`, thinking blocks, and
  `temperature`, `top_p` and `stop_sequences`, which OpenAI's reasoning models refuse unless left at their
  defaults.
- A stream becomes the Anthropic event sequence, one content block at a time; each parallel tool call gets
  its own `tool_use` block. Usage arrives in `message_delta`, with cached prompt tokens reported as
  `cache_read_input_tokens`. `finish_reason` maps `stop` → `end_turn`, `length` → `max_tokens`,
  `tool_calls` → `tool_use`, `content_filter` → `refusal`.
- Once the stream has started there is no fallback. An error inside the stream, a stream that ends without
  a finish reason, or a provider that interleaves the arguments of parallel tool calls ends with an
  `event: error`, never a silently truncated message. Keep-alive pings and the silence deadline work as
  above; upstream chunks that translate to nothing still count as activity.
- Error responses keep their status and map to Anthropic error types. `context_length_exceeded` becomes
  `400` "prompt is too long: …", so Claude Code compacts; `insufficient_quota` becomes `403`
  `permission_error`, so it isn't retried. Of the upstream headers only `retry-after` is kept. None of
  these errors carries `x-colonizer-fallback`, except quota exhaustion (`insufficient_quota`; see
  **Quota exhaustion** below).

**Spend accounting.** Every response the gateway serves is counted, priced with the provider's `pricing`,
and added to the colony's `routed_cost_usd`; its tokens are counted too, whether or not the response was
priced, and added to the colony's `routed_tokens`. The budget is re-checked after each addition and before a
request is served; Claude's own `cost_usd` landing at a turn end re-checks it too. The sandbox module's
`budget_tokens` holds a colony to its routed tokens the same way (global, no per-org override), and passing
it stops the colony like an overspend. The two wires are
counted differently but on one scale, Anthropic's token names:

- `wire: anthropic`: the body is tapped while it forwards; the bytes the colony receives are never
  changed. An SSE stream is read event by event (`message_start` fixes the input side, `message_delta`
  carries the running output total); a non-streaming JSON body is buffered only to count, up to 4 MiB,
  past which the response forwards unpriced. Anything the tap cannot parse counts as zero, so an
  estimate can only undercount.
- `wire: openai`: the usage the translation already extracted is reused; the body is never read twice. A
  passthrough response is tapped like the anthropic wire instead, in the route's own spelling: Responses'
  `response.completed` event — or the `usage` of a finished non-streaming body — and a chat completion's
  final chunk (or non-streaming `usage`), with cached input coming back out of the input total either way,
  so both wires account on Anthropic's scale.

`pricing` is five rates in dollars per million tokens: `input_per_mtok`, `output_per_mtok`,
`cache_read_per_mtok`, `cache_write_per_mtok` and `thinking_per_mtok`. A save checks that the first
four are `0` or more. A provider
without it (or with all five at `0`) still counts its tokens, which reach `model_usage` as usual, but
contributes nothing to `routed_cost_usd`. `PUT /api/providers/{id}` with `pricing` omitted keeps the saved
rates, like the key; an all-`0` object clears them in effect. Claude traffic does not pass through the gateway at all:
microsandbox injects the credential straight to `api.anthropic.com`, so Claude's spend is only seen when
a turn ends, as the runner's `cost_usd`. A colony's dollar budget answers to the two added together, and both
are estimates.

**Provider fields** (all optional): `timeout_secs` (30-3600, default 600), `max_concurrent` (1-64, absent =
unlimited), `queue_timeout_secs` (1-3600, default `timeout_secs`), `context_tokens` (1024-2000000),
`fallback_model` (a Claude model; the aliases `opus`, `sonnet`, `haiku` and `fable` are resolved to model IDs in routes,
because a fallback request goes to the API as is — or, for quota exhaustion only, `<provider>/<model>` on another
configured provider that lists the model and speaks the same wire, which the gateway retries itself; see Quota
exhaustion below. A cross-wire, unknown-provider, own-provider or unlisted fallback is a `400` on save, and only a
Claude fallback reaches the colony's route, so only it covers an unreachable, timed-out or full connection). `quota` is where to read what is left in a prepaid token
plan: `{url, pointer, limit_pointer?}` — a `GET` the health check makes with the provider's own credential, a non-empty
RFC 6901 JSON pointer starting with `/` into its answer for the remaining count, and optionally a second
pointer to the plan's total (answered as `quota.limit`) — so `url` must sit on the base URL's origin (scheme,
host and port, since the credential is sent there) and is refused at save time anywhere
else. `PUT /api/providers/{id}` with `quota` omitted keeps the saved probe, like `pricing`; an empty `url`
clears it. The origin rule reaches the base URL itself: a save that moves a keyed provider to another
origin is refused unless `api_key` brings the key again (`""` removes it), and a saved probe the move
leaves behind is refused with it. Leaving `max_concurrent` unset really does mean unlimited: the
provider gets asked for as many requests at once as are made of it. With `delegate = enforce` — the delegation
default — every colony works through subagents, so the request rate arriving at a provider is roughly the number
of running colonies times their subagents; on a server that handles one or two requests at a time, set the limit.
`GET /api/providers` also returns `pricing`, `quota`, `in_flight`, `queued`, `usage`, `health`, `used_by` and — when
the probe has discovered what the endpoint publishes (see **Discovered models**) — `discovered_models`, `new_models`
and `discovered_at`.

**Integration notes (Meta Model API).** Adding the `meta` preset as a first-class `wire: anthropic`
provider surfaced a few quirks worth carrying into the next such integration. `base_url` for an
anthropic-wire provider must be scheme and host only, with no `/v1` suffix: the gateway appends the
request's own path itself (`/v1/messages`, and `/v1/models` for the health probe), so a base already
ending in `/v1` doubles it and 404s silently until the first live call. `PUT /api/providers/{id}` now
rejects that shape at save time for `wire: anthropic` (an `openai`-wire base_url ending in `/v1`, like
`xai-grok`'s, is legitimate — the gateway's join there skips the guest path's repeated `/v1`). Meta enforces
`max_tokens >= 16`, answering `400` `invalid_request_error` below it, so a colony or provider default for
this preset must respect that floor. Meta is also a heavy reasoner: thinking tokens are spent from the
output budget before any text, so `max_tokens` should be set generously here, and its
`usage.output_tokens_details.thinking_tokens` is now tracked as `Usage.thinking_tokens`, priced by
`Pricing.thinking_per_mtok` alongside the other four rates. Thinking itself arrives as opaque
`redacted_thinking` blocks — standard Anthropic wire, passed through by Claude Code unchanged — and
nothing in the harness inspects their contents; only the text blocks are visible to diagnosis and event
streams. Contributor-tier pricing for Meta is currently unknown and unverified, so the preset ships with
`pricing` unset: honest (tokens are still counted; nothing is guessed), but it means spend on this
provider has to be watched manually rather than assumed free.

**Per-provider quirks.** Dialect gaps like Meta's live as data in `providers.rs` (`ProviderQuirks`,
one row per preset in `PRESET_QUIRKS`), not as `if id == …` branches: the next such gap becomes a new
row. Meta's row says `strip_cache_ttl` (its API rejects `cache_control` blocks carrying `ttl`) and
`min_max_tokens: 16`. On the `wire: anthropic` path the gateway normalizes preemptively — it parses the
request body and rewrites `{"type":"ephemeral","ttl":…}` blocks (system blocks, message content blocks,
tools) to `{"type":"ephemeral"}`, raising `max_tokens` below the floor — logging what it changed with the
field named. Providers without quirks skip this entirely: their bodies proxy byte-identical. An upstream
`400` is logged with provider and status rather than proxied invisibly, and any upstream 4xx/5xx flags the
colony for attention under the `model_error` reason (leaving an existing watchdog/autopilot flag alone),
which usage telemetry buckets as a closed failure label.

**Usage.** `usage` is the provider's cumulative counters: what says a request has ever actually gone to it,
which the momentary `in_flight`/`queued` gauges cannot:

```json
{"requests": 12, "failures": 2, "fallbacks": 1, "duration_ms": 48021, "last_request_at": "…", "since": "…"}
```

`requests` counts every request the gateway accepted for the provider, from the moment everything that can
refuse a request locally has passed (colony auth, provider lookup, path and body translation), queueing,
the upstream call and the streamed body are included, a request the gateway itself refuses is not, and an
attempt that queued past `queue_timeout_secs` without ever reaching the provider still counts. `failures`
is the subset that produced no usable upstream response: one of the gateway's three fallback answers (queue
timeout, unreachable, timeout), an upstream status ≥ 400, or an openai-wire response whose body failed or
never finished. `fallbacks` is the subset of `failures` the
gateway predicts will fall back to Claude: it answered with `x-colonizer-fallback` and the provider has a
`fallback_model`, which is exactly when the colony's router retries on Claude; the retry never comes back
through the gateway, so this is a prediction, not an observation. `duration_ms` is the cumulative
wall-clock of dispatched requests, streamed body included, timed from when a request's slot was acquired,
so time spent queued is not. `last_request_at` is RFC 3339, `null` before
the first request. `since` is when this tally started — the first counted request — in the same form,
`null` for a tally with no requests yet or one kept by an older build. The counters live in
`provider-usage.json` in the mothership's data directory, written
by a background task every 5 s when they changed and once more at shutdown, so a crash loses at most 5 s
of the tally and a restart carries on where it left off; `DELETE /api/providers/{id}` also removes the
provider's tally.

`used_by` names the model settings (`model`, `subagent_model`, `background_model`, and per-task
routing's `model_low`/`model_high`, whose env vars a colony's environment never sees) whose resolved value
(schema default, global setting or org override) routes to this provider as `<provider>/<model>`, across
the global agent env and every org override, e.g. `["subagent_model"]`. Empty means the provider is
configured but no model setting points at it: wired only to `subagent_model`, say, on a harness whose
colonies never spawn subagents: unused so far, not broken. A bare alias or a partial id prefix is
another provider's model and doesn't match, same rule as the "used" routes above.

**Usage health.** `GET /api/providers` also carries each provider's `health`, the mothership's read on
`usage`, computed by one rule shared with the notify module:

```json
{"failure_pct": 29.4, "avg_latency_ms": 480, "rated": true, "degraded": true, "last_failure": "timeout"}
```

`failure_pct` is `failures/requests` as a percentage rounded to one decimal place (`0` with no
requests), `avg_latency_ms` is `duration_ms/requests` (`0` with no requests), `rated` is whether
there are at least 50 requests — enough to judge a provider by its failure rate; a handful of early
failures is noise — and `degraded` is `rated` with a `failure_pct` of 10% or more, so an unrated
provider is never degraded. One rule, so a provider the fan-out is drowning looks the same
everywhere: `GET /api/status` carries a `model_providers` array of
`{id, name, requests, failure_pct, avg_latency_ms, degraded}`, so the status poll answers "is it the
provider?" without opening the providers screen, and the notify module's `provider_degraded` event
announces the same verdict when it first appears (§6.3).

**Quota exhaustion.** An upstream 429/403 — or turn text with no status at all, from the colony-side
turn-end scan — whose message says the plan ran out — "quota has been exhausted",
"weekly limit", "token-plan" with limit/reset phrasing, `insufficient_quota`, billing/plan quota
wording beside an exhaustion verb, or "usage limit" with reset phrasing, never a bare rate limit or
a refused model — is quota exhaustion, and the gateway treats it apart from transport failure. Any
other status is not exhaustion, whatever it says. The anthropic wire forwards the error body
verbatim; the OpenAI wire forwards the translated body (the classifier reads the raw error code
there, since translation drops `insufficient_quota`). Either way the provider is recorded as
exhausted (in `provider-quota.json` in the mothership's data directory, written on every change, so a
restart keeps it; a record whose reset has passed or whose TTL has run out is dropped on load) with
the reset the message named — or, when the message names none, a 15-minute TTL after which the record
lapses and the queue re-probes — and the answer carries `x-colonizer-quota-exhausted` (the reset
words, or `exhausted`). An upstream 2xx clears the record at once. When the
provider has a Claude `fallback_model` the answer also carries `x-colonizer-fallback:
provider_quota_exhausted`, and the colony router retries on Claude exactly as for 502/503/504. When
its `fallback_model` is `<provider>/<model>` ([#767]) the gateway retries the request itself, once:
the same body with `model` set to the fallback's model, to that provider, which must speak the same
wire (anthropic to anthropic, openai to openai — the gateway has no path that re-shapes a response for
the other wire mid-request). The retry is the operator's hop, so the colony's recorded
`allowed_providers`/`allowed_models` do not refuse it; sensitivity, key and budget checks apply as
to any request, and it is audited as its own request to the fallback provider. The colony gets the
fallback's answer; a fallback provider that is itself exhausted, missing or on another wire is not
tried, and the original answer (without `x-colonizer-fallback`) stands. Failover happens at request level, so an operator opts a role out by unsetting that role's
provider's `fallback_model`, or everything at once with `COLONIZER_QUOTA_FALLBACK=0`.
`GET /api/providers` carries `quota_exhausted` (`{reset_at, reset_unix}`, null while healthy) per
provider, and there a quota-exhausted provider reads `health.degraded: true` whatever its failure rate
says (the status poll's `model_providers` applies the plain rate rule only). `GET /api/status` carries `quota`: `{paused, kind, reason, reset_at, reset_unix, providers, provider_details}` —
`paused` when every routable provider (every `used_by` non-empty one, or every provider when none
is used) is exhausted — `kind: "provider"` — or when the Claude account itself hit a session,
weekly, hourly or Opus limit — `kind: "account"`, with or without providers — with the earliest reset and the queue holder's own `reason`, which names providers by their display
name. `provider_details` lists each exhausted plan as `{id, name, used_by}` — `used_by` in plain words
(`orchestrator`, `subagents`, `background`, `small model`, `small tasks`, `large tasks`); an account
pause carries one `anthropic` entry named `Claude` with the roles still on Claude models. A paused
queue admits nothing; the cockpit banners it in its own words: the plan by name ("BytePlus plan
limit reached" — only the Claude account itself is ever called Claude), the roles it affects, the
reset as a local time and a countdown, and a parked-colony count and Resume all only when a colony
is parked. A colony whose turn dies on an exhausted provider
is parked ([#213]): `status` `parked` with a `parked` record (see `Session` above), the worktree
kept, its slot released and `attention.reason` `provider_quota_exhausted`. The queue's 5 s tick
resumes parked colonies whose provider recovered — reset passed, or the provider deleted — requeueing
ones whose park discarded the microVM and routing a kept-VM park through the resume endpoint, which
resumes it warm when it can and falls back to cold otherwise. Colonies whose provider is still
exhausted stay parked.

**Claude account fallback** ([#1130]). The Claude account's own cap is a record under the id
`claude-account` and has no provider to give it a `fallback_model`; the Claude Code module's
`account_fallback_model` (`<provider>/<model>`, install-wide) stands in. Claude's own traffic does not
pass through the gateway, so a colony given the setting gets `COLONIZER_ACCOUNT_ROUTE`
(`{"url": ".../account-route", "headers": {"x-colonizer-colony": <token>}}`) and its model router asks
`GET /account-route` before forwarding an unrouted request to Anthropic. The answer is JSON,
decided from the live account record, the setting and the colony's sensitivity class:

| `action` | Meaning |
| --- | --- |
| `claude` | The account works, no usable fallback is set, or the fallback's own plan is out: the request goes to Anthropic as today. |
| `fallback` | The account is out: `model` (`<provider>/<model>`), `provider_name`, `reset_at`, `reset_unix`. The router sends the request to that provider's route with the model rewritten. |
| `parked` | The account is out and the fallback may not carry this task (restricted work, an untrusted provider): `reason` reads "needs a trusted provider: Claude is out until 19:51; MiniMax is not marked trusted". The request stays on Claude, the turn dies on the limit, and the colony parks with the reason added to its error. |

The colony's recorded `allowed_providers`/`allowed_models` include the fallback, so the gateway carries
the rerouted requests; every other check — sensitivity, key, budget — applies as to any request. The
colony's log gets one line per change of answer, and the router reuses an answer for five seconds. At
the reset the record lapses and the next answer is `claude`: no saved setting changes. While the
fallback can carry the work, `GET /api/status` `quota` has `paused: false` and a `fallback` object
(`{model, provider_name, reset_at, reset_unix}`) instead of an account pause, colonies the cap parked
resume (those it may not carry stay parked until the reset), and `GET /api/models/plans` adds
`fallback: {model, provider_name}` to the Claude row. `POST /api/models/switch` returns
`leftover_claude` (`{colonies, orgs, cleared}`) listing the Claude names the switch left in per-colony
and org overrides; `clear_leftovers: true` removes them.

**Provider out of quota cards** ([#767]). The maintainer answers an exhausted provider on one
dedicated card per provider, not on a free-form question from an agent. The gateway ties colonies to
the exhaustion: a colony whose request came back quota-exhausted with no `fallback_model` retry on
offer is *blocked* on that provider until one of its own requests succeeds (in memory; the provider's
record above is what survives a restart). A card lists every colony in play (live, queued or parked,
not cleaned up) that is blocked on the provider, or parked with `attention.reason`
`provider_quota_exhausted` naming it (in `attention.provider`, or in the park's `error`). Only a
provider with an active record and at least one such colony gets a card; the Claude account's own cap
keeps its banner (`quota.kind: "account"`).

`GET /api/attention` answers `{"quota_cards": [card]}`, and `GET /api/status` carries the same list as
`quota_cards`, so the cockpit needs no second poll:

```json
{"provider": "bailian", "provider_name": "Bailian", "models": ["qwen3.8-max"],
 "title": "bailian · qwen3.8-max is out of quota", "reset_at": "Oct 1, 16:00 UTC", "reset_unix": 1790870400,
 "colonies": [{"id": "…", "repo": "acme/webshop", "org": "acme", "issue": 42, "issue_title": "…",
               "status": "running", "hits": 3, "waiting": false, "resume_unix": null}],
 "orgs": ["acme"], "waiting": 0, "resume_unix": null, "fallback_model": null, "wire": "anthropic",
 "alternatives": [{"id": "sonnet", "label": "Claude Sonnet (latest)", "provider": "anthropic", "wire": null,
                   "failure_pct": 0.0, "rated": false, "degraded": false, "healthy": true}]}
```

`models` are the provider's models the colonies run, most used first; `hits` is a colony's quota
answers in a row; `waiting`/`resume_unix` count the colonies parked for the reset and the earliest
scheduled resume. `alternatives` is every model `/api/models` offers that is not on an exhausted
provider, with its provider's usage health (Claude's models read degraded only while the account's
own cap holds), healthy ones first.

`POST /api/providers/{id}/quota-action` answers the card with
`{action: "switch"|"wait"|"stop", model?, scope?: "colonies"|"org"|"all", colonies?: [id], org?, remember?}`
and replies `{action, provider, colonies: [id], failed: [{id, ok: false, error}], changes: [change]}`. `colonies` limits
the action to some of the card's colonies (an id not on the card is a `400`); `org` limits it to one
org.

- `switch` needs `model`, one of the card's `alternatives` (a model on the exhausted provider, or one
  not on offer, is a `400`). Each colony's role on the provider moves: `model_override` when its
  orchestrator model is there (or no role visibly is — a tier or background model), and
  `subagent_model_override` when its subagent model is. The colony is then restarted — a live or
  parked one stopped and resumed cold, a stopped one resumed, a queued one simply boots with it — so
  boot re-derives `allowed_providers`/`allowed_models` from the new settings. `scope: "org"` also
  moves the colonies' orgs' `model`, `subagent_model` and `background_model` overrides that route to
  the provider, so the orgs' next colonies start on the new model. `remember: true` saves `model` as
  the provider's `fallback_model`: a Claude model, or a model on another provider of the same wire
  (the rules of Provider fields above; `400` otherwise, before anything changes). `scope: "all"`
  ([#767]) moves every model role on the provider install-wide: each of the agent module's
  `model`, `subagent_model`, `background_model`, `summary_model`, `model_low` and `model_high`
  settings that routes to the provider (saved through `PUT /api/modules/agent`'s own handler and
  validation), every org's `model`, `subagent_model` and `background_model` override that does (every
  org, not only the card's), and the card's colonies as above. Every role is checked before anything
  changes — the module's schema, a `model_map` that must list the model, and `summary_model` on a
  Claude model needing an Anthropic provider or API key (summaries never use the login) — so a model
  one role cannot take is a `400` with nothing moved. Any other `scope` is a `400`.

  A switch's reply also carries `changes: [{scope, target, key, was, now}]`, one per setting it
  moved, with the value it replaced (`was`, null when unset): `scope` is `install` (target: the agent
  module), `org` (target: the org), `colony` (target: the colony id; `was` is the model the role
  resolved to) or `provider` (target: the provider; key `fallback_model`, from `remember`). The
  mothership logs the same list. There is no undo; the cockpit shows each as "was X → now Y".
- `wait` parks every live colony (`status` `parked`, the worktree kept — suspended, not failed) and
  re-stamps an already-parked one: `attention` gains `provider`, `action: "wait"`, `reset_at` and
  `resume_unix` (the provider's `reset_unix`). The queue's 5 s resume pass requeues it once
  `resume_unix` has passed or the provider recovered, whichever is first; a reset-less record resumes
  it when the record's TTL lapses. A queued colony is reported under `failed`: the queue already holds it.
- `stop` stops every colony as `POST /api/sessions/{id}/stop` does; a pre-#213 `stopped` park loses its
  quota flag, so it is not resumed later.

A blocked colony that is live — `starting` included, when its agent never got a turn out — is
flagged by the watchdog's minute tick with `attention.reason` `provider_quota_exhausted`,
`attention.provider` and the reset, and a log line saying it needs the maintainer; the watchdog does
not nudge it, since another request cannot be answered until the plan resets ([#760]).

**Health.** `GET /api/providers/{id}/health` probes `GET {base_url}/v1/models` with a 5 s timeout:

```json
{"reachable": true, "status": 200, "latency_ms": 42, "models": ["deepseek-v4-flash"], "error": null, "note": null, "checked_at": "…"}
```

The probe is informational, not a routing gate. An Anthropic-wire endpoint need not serve `/v1/models`,
so a 404 from one comes back as `reachable: true` with the real `status`, empty `models` and
`"note": "no model list"`; `note` is `null` in every other case. A provider with a `quota` probe
configured (see **Provider fields**) gets `quota: {remaining, error}` on the same answer — the probe
URL is fetched with the provider's credential alongside the models check, and whatever goes wrong with
it (`remaining: null`, the reason in `error`) never changes `reachable`: reading a plan balance is not
a health check. No probe configured, no `quota` field.

**Discovered models.** Every fresh probe also records what `GET {base_url}/v1/models` answered with, as a
sidecar entry for the provider in `<config_dir>/provider-models.json`
(`{base_url, models, new_models, discovered_at}`) — a sidecar, not a `providers.json` field, because
discovery is an observation and must never rewrite the operator's configuration. The triggers are the
save (`PUT /api/providers/{id}` re-probes in the background), the Check button, and a background sweep
that re-probes each provider whose entry is missing or older than 24 h; a provider that will not answer is
retried at most once a day. Only a real answer records — a parsed `data` list, or the Anthropic-wire 404
no-model-list case as an empty list — while a refused (401), throttled (429), failed (5xx) or list-less
answer, like an unreachable one, records nothing, so a blip never wipes the stored list. The list is
deduped, sorted and capped at 500. `GET /api/providers` then carries `discovered_models`, `new_models`
(what the previous discovery at the same base URL did not have, so a model is flagged new once) and
`discovered_at`, and carries none of them while the entry names another base URL — a repointed provider is
not offered its old endpoint's models. The operator's own `models` list is never changed by any of this;
deleting a provider drops its sidecar entry.

**Test request.** `POST /api/providers/{id}/test` sends a one-token request (`max_tokens: 1`, the
provider's first listed model) through the route a colony's turn takes — `/v1/messages` on the
anthropic wire, the translated `/v1/chat/completions` on the openai wire — joined to `base_url` as the
gateway joins it, with the provider's credential and connection policy, and a 30 s timeout:

```json
{"ok": false, "url": "https://ark.example.com/api/coding/v1/chat/completions", "status": 404, "model": "ark-code", "latency_ms": 41, "error": "HTTP 404 (upstream answered 404 at …; check the provider's base URL)"}
```

`url` is the URL the request hit with userinfo and query removed. Settings → Providers runs it when a
provider is added, or its base URL, wire or key changes, and shows the URL and status, so a base path
that misses the provider's API root shows at setup ([#1018]). A provider with no listed model answers
`ok: false` with nothing sent. On the gateway's own routes, an upstream `404` or `405` is logged with
the same redacted URL, and on a translated route reaches the colony as `not_found_error` naming it.

At colony start the mothership probes every used provider: each unreachable one logs a warning, or
refuses the launch when the route has no fallback model.

Which agent backend a connection can serve, `model_map`, and the two levels of `disabled_tools`, are in
[docs/providers.md](../providers.md).
