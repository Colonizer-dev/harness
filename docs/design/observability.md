# Observability export: architecture

Status: accepted · Decided 2026-10-03 · [#839](https://github.com/Colonizer-dev/harness/issues/839)

This is the design the observability add-on (#840–#865) is built from. Every numbered rule below is an
accepted decision, not reopened here; what this document adds is what the issue left to
implementation: exact id formulas, the span tree, byte budgets, the privacy rules as testable
statements, and the packaging split between the mothership and a separate add-on binary.

Related: [Settings and privacy](../observability/settings.md) (the `observability` module's fields —
not restated here), [the live map](../telemetry.md), [usage data](../usage-data.md), [the agent event
schema](../agent-events.schema.json), [decisions.md](../decisions.md) (four entries at the bottom link
back here).

Where this document and the GitHub issue disagree on a name, the repo wins: the issue's
`max_read_bytes_per_sec` is `max_read_mib_per_sec` in `settings.rs`; the issue's generic "backoff like
agent_link" is `events.rs`'s actual 1 s→2 s→4 s→8 s→10 s (`agent_link`, `events.rs:90-153`), which the
add-on's own supervisor does not copy verbatim (see [Supervision](#supervision)).

## Scope

Three things export data off the mothership; none share a switch, an id, or a destination:

| Thing | Doc | Switch | Destination |
| :--- | :--- | :--- | :--- |
| Live map | [telemetry.md](../telemetry.md) | one checkbox, off by default | `telemetry.colonizer.dev` |
| Usage data | [usage-data.md](../usage-data.md) | `colonizer telemetry off`, on by default | wherever `COLONIZER_TELEMETRY_ENDPOINT` names |
| Observability (this doc) | here | the `observability` module, off until configured | the operator's own backend, never colonizer.dev |

Also distinct: fleet history push (`fleet_sync.rs`, a member draining its own finished colonies'
history to its fleet's owner) is a separate, existing feature; [Fleet](#fleet-resource-attributes-and-relay)
only adds a resource attribute and an optional relay of what a member already sends.

## Source inventory

Every file the exporter reads, and nothing else. `transcripts/` and `chats/` are listed because the
tailer must know to skip them, not because it reads them.

| Source | Where | Has `seq`? | OTLP signal(s) | Tier | Attribute allowlist (structure tier) |
| :--- | :--- | :--- | :--- | :--- | :--- |
| `events` | `sessions/<id>/events.jsonl` | yes | logs, traces | structure + content | `type`, `state`, `risk`, `kind`, `blocking`, `tool_call_id`, `name` (tool), `is_error`, `denial.class`, `question_id`, `model`, `model_usage.*` (tokens), `cost_usd`, `duration_ms`, `access`, `policy`, `path` (hashed with `repo_names=hashed`), `agent_ref.id/name` |
| `harness` | `sessions/<id>/harness.jsonl` | no | logs | structure | `origin`, `level` |
| `gateway` | `sessions/<id>/gateway.jsonl` | no | logs, traces | structure | `provider`, `wire`, `model`, `wire_model`, `status`, `failure`, `fallback`, `queue_ms`, `duration_ms`, `request_bytes`, `response_bytes`, `input_tokens`, `output_tokens` |
| `findings` | `sessions/<id>/findings.jsonl` | no | logs | structure + content (`title`/`reason` are content) | `state`, `issue`, `duplicate_of`, `severity`, `verdict`, `fix_session`, `review_session`, `pr` |
| `transcripts/` | `sessions/<id>/transcripts/` | — | never sent | — | — |
| `activity` | `<data>/activity.jsonl(+.1)` | yes | logs | structure + content (`detail` of every kind but the reviewed few) | `kind`, `actor`, `via`, `colony`, `repo` (hashable), `target`, `summary` (the `detail` of the reviewed kinds `decision.shadow`, `decision.act`, `decision.fallback`, `outcome.suspended`) |
| `spend` | `<data>/spend.jsonl` | no | logs, metrics | structure | `kind`, `org`, `session`, `agent`, `model`, token fields, `cost_usd`, `scoring_ms` |
| `decisions` | `<data>/decisions.jsonl` | no | logs | structure | `kind`, `point`, `mode`, `options` (count only), `pick`, `confidence`, `latency_ms`, `miss`, `did` (closed: `jev`/`rule`/`cap`, else `other`), `outcome.*` (the grade's scalars) |
| `routing` | `<data>/routing.jsonl` | no | logs | structure + content (`decision.reason`) | `kind` (`decision`/`actual`), `actual_cost_usd`, and the routing record's `decision.point`, `decision.jev_mode`, `decision.jev_agrees`, `decision.floor`, `decision.tier`, `decision.rule`, `decision.source`, `decision.score`, `decision.model`, `decision.agent`, `decision.misroute`, `decision.sensitivity` |
| `jev_ladder` | `<data>/jev_ladder.jsonl` | no | logs, metrics | structure | `kind` (`decision`/`reread`), `tool`, `tool_call_id`, `action`, `keep_call`, `keep_result`, `matched_tool_call_id` |
| `jev_focus` | `<data>/jev_focus.jsonl` | no | logs, metrics | structure | `kind`, `session`, `mode`, `candidates` (count only), `chosen`, `would_catch`, `verdict`, `actual_first_failure_ms`, `focused_first_failure_ms`, `total_ms`, `checks_run` |
| `chats/` | `<data>/chats/` | — | never sent | — | — |
| `mothership` | `<data>/logs/mothership.jsonl` (#856) | no | logs | structure | `level`, `target`, `fields` (allowlisted keys only; none yet) |
| `export_gap` | the exporter's own record (#843), stream `meta` | no | logs | structure | `reason`, `file`, `bytes`, `lines`, `archived` |

`decisions`, `routing`, `jev_ladder` and `jev_focus` carry no colony id on most rows (`routing`'s
`session` field is colony-scoped, the Jev ledgers' `session` likewise) — the record id's
`colony_or_dash` (below) is the row's own `session`/`colony` field when present, `-` otherwise.
`activity`, `spend` and `mothership` are install-wide and use `-`.

### Log record names

Settled with #845. Every log record's event name is `colonizer.<what>` in snake case, one per
source: `colonizer.harness_log`, `colonizer.agent_event`, `colonizer.gateway_request`,
`colonizer.activity`, `colonizer.spend`, `colonizer.finding`, `colonizer.decision`,
`colonizer.routing`, `colonizer.jev_ladder`, `colonizer.jev_focus`, `colonizer.mothership_log`,
`colonizer.export_gap`. A line's own fields keep their ledger names as attribute keys (`provider`,
`model`, `status`, …), exactly the allowlists above, rather than OpenTelemetry's `gen_ai.*` and
`http.*` names: the allowlist is then the ledger's schema, checkable field by field, and a reader
can join a record to the line it came from. The `gen_ai.*` names belong to spans (#846, #847),
where the semantic conventions define them. The severity comes from `level` where a line has one,
`WARN` for a failed gateway request, an `outcome.failed` activity line and every `export_gap`, and
`INFO` otherwise.

## Identity

`source` below is the first column of the source inventory table above (`events`, `harness`,
`gateway`, `findings`, `activity`, `spend`, `decisions`, `routing`, `jev_ladder`, `jev_focus`,
`mothership`).

### Record id

```
colonizer.record.id = hex(sha256("colonizer.rec.v1|" + host_id + "|" + source + "|" + colony_or_dash + "|" + key)[..16])
```

- `host_id`: `runtime::host_id` (`crates/colonizer/src/runtime.rs:489`), the per-install UUID at
  `<config_dir>/host_id`.
- `source`: one of the identifiers above.
- `colony_or_dash`: the session id the row belongs to, or `-` for an install-wide row with none.
- `key`: the line's `seq` as a decimal string where the source has one (**only** `events` and
  `activity`); otherwise the lowercase hex SHA-256 of the raw line bytes, newline excluded.
- Result: the first 16 bytes of the SHA-256 digest, rendered as 32 lowercase hex characters.
- Replaying the same bytes through the same host always recomputes the same id — this is what makes
  the exporter's at-least-once delivery safe to replay into a backend that dedupes on this field
  (carried as the OTLP log record's `colonizer.record.id` attribute, and promoted as the idempotency
  key wherever a backend has one, e.g. Loki's `__loki_log_id__`-style ingestion metadata).

### Trace id

```
colonizer.trace.id = sha256("colonizer.trace.v1|" + host_id + "|" + colony_id)[..16]
```

One trace per colony (its whole lifetime, not per turn), 16 raw bytes — the OTLP trace id width.

### Span id

```
colonizer.span.id = sha256(trace_id_bytes ‖ kind_utf8 ‖ key_utf8)[..8]
```

`‖` is raw byte concatenation: the trace id's 16 raw bytes (not hex), then UTF-8 `kind`, then UTF-8
`key`, fed to SHA-256 as one buffer; the first 8 bytes are the span id. For this to be unambiguous —
`kind="turn", key="1"` must not collide with some other `kind'+key'` concatenating to the same bytes —
the set of `kind` values is **prefix-free**: no kind string is a prefix of another. The closed set:

| `kind` | Meaning |
| :--- | :--- |
| `invoke_agent` | The root span (one per colony) |
| `turn` | One agent turn |
| `subagent` | One subagent's lifetime |
| `execute_tool` | One tool call |
| `chat` | One routed model call (gateway.jsonl) |
| `question` | One question/answer round-trip |
| `host_step` | One host-chain step (watchdog nudge, autonomy judge note, etc.) |

(Verified pairwise: none is a prefix of another.) `key` is defined per kind in the span table below.
Replaying the same files recomputes the same span ids, so re-running the exporter (backfill, cursor
reset) never creates duplicate spans in a backend that keys on span id.

## Spans: reconstructing a trace from files

Spans are **derived**, not emitted live: a pure function reads a window of already-written `events`
(and `gateway`) lines for one colony and returns the spans implied so far. No `tracing-opentelemetry`,
no span context propagated through the running mothership — the files already have everything
(causality is `agent_ref` for subagents, `tool_call_id` for tool calls, ordering for turns).

### Span tree

| Kind | `key` | Parent | Starts at | Ends at | Name | Attributes (allowlist, beyond `gen_ai.*`) |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| `invoke_agent` | colony id | — (root) | colony launch | final `turn_end` outcome, or `root_after_idle` (24h) for a stopped colony with no outcome yet | `invoke_agent {repo}` | `colonizer.colony.id`, `colonizer.repo` (hashable), `colonizer.outcome`, `colonizer.trace.dropped_spans` |
| `turn` | 1-based turn counter | `invoke_agent` | first event after the previous `turn_end` (or colony start) | this turn's `turn_end` | `turn {n}` | `gen_ai.usage.input_tokens`/`output_tokens` (from `model_usage` delta), `cost_usd` delta, `is_error` |
| `subagent` | `agent_ref.id` (the Task `tool_call_id` that started it) | the `execute_tool` span of that same Task call | the Task `tool_call` | the Task's `tool_result`, or the subagent's own terminal `status` | `subagent {agent_ref.name}` | `gen_ai.agent.name`, `colonizer.subagent.description` (content tier) |
| `execute_tool` | `tool_call_id` | enclosing `turn` (or `subagent` if `agent_ref` is set) | `tool_call` | matching `tool_result` | `execute_tool {name}` | `tool.name`, `is_error`, `denial.class`, output size (bytes, not content) |
| `chat` | `gateway` record id | enclosing `turn` by time overlap | request accepted (`gateway.ts`) | response (`ts + duration_ms`) | `chat {wire_model}` | `gen_ai.request.model`, `gen_ai.response.model`, `input_tokens`, `output_tokens`, `status`, `failure`, `fallback` |
| `question` | `question_id` | enclosing `turn` | `question` event | `question_answered` event, or never (see below) | `question` | `risk`, `kind`, `blocking` |
| `host_step` | source + its own key (e.g. `activity` record id) | `invoke_agent` | the host-side event's own timestamp, zero duration | same | `host_step {activity.kind}` | `kind`, `actor` |

Claude Code calls that go straight through the TLS edge (not the gateway) leave no `gateway` row, so
they get no `chat` span; their tokens and cost land only as the `turn` span's `model_usage` delta —
stated in the issue's accepted decisions and repeated here because it is the one case where "no
record, no span" is correct, not a gap.

### Turn boundaries, sampling, and spans that never close

1. A turn starts at the first event after the previous turn's `turn_end` (or at colony start, for
   the first turn) and ends at its own `turn_end`. `turn_end` is cumulative (`model_usage`,
   `cost_usd`), so the `turn` span's own attributes are deltas against the running total the exporter
   keeps in its cursor state — not the cumulative numbers the event carries.
2. The root `invoke_agent` span is emitted once, at the colony's final outcome (`outcome.*` in
   `activity`, or the session's terminal status). A colony still running, or parked, has no root yet;
   Tempo shows "root span not yet received" for its children, which is accepted (issue's rule 5).
   `root_after_idle` emits the root anyway after 24 hours with no new `events` line, tagged
   `colonizer.outcome = "idle"`, so a colony nobody ever closes does not orphan its spans forever.
3. `trace_sample_ratio` decides, once per colony, whether its trace is exported at all:
   `sampled = (first 8 bytes of trace_id interpreted as u64) / u64::MAX < trace_sample_ratio`
   — deterministic by trace id, so a partial backfill or a retried export samples the same way twice,
   and two machines in a fleet agree on the same colony without talking to each other.
4. A span whose end event never arrives (a `tool_call` with no `tool_result`, a `question` with no
   `question_answered`) is closed at the cursor's current read position with status `unset` and an
   attribute `colonizer.span.incomplete = true`, when the colony itself reaches a terminal state or
   `root_after_idle` fires — never left open indefinitely, since an open span pins memory and most
   backends time out an unclosed span anyway.

## No OpenTelemetry SDK in the export pipeline

OTLP is a wire format; the mothership and the add-on do not run an OTel SDK pipeline (no `Tracer`, no
`SpanProcessor`, no in-process exporter registry). The add-on encodes `ExportLogsServiceRequest` /
`ExportTraceServiceRequest` / `ExportMetricsServiceRequest` messages itself from the derived records
above and POSTs them.

| Dependency | Version | Notes |
| :--- | :--- | :--- |
| `opentelemetry-proto` | 0.33 | `default-features = false`, features `gen-tonic-messages, logs, trace, metrics, with-serde`; MSRV 1.75 |
| `prost` | 0.14 | pulled in by `opentelemetry-proto`'s `gen-tonic-messages`; used for protobuf encode only |
| `opentelemetry`, `opentelemetry_sdk` | pinned transitively, default features off | compiled, **never instantiated** — no `TracerProvider`, no global, no `#[instrument]` anywhere in this tree |
| `tonic` | pinned, feature-gated | only behind the add-on's own `otlp-grpc` feature (#851), not built by default |
| `reqwest` | 0.13 (already in `Cargo.lock`, shared) | OTLP/HTTP transport |
| `flate2` | new | gzip compression (OTLP/HTTP `Content-Encoding: gzip`) |

None of `opentelemetry*`, `prost`, `tonic`, `flate2` exist in today's `Cargo.lock`; `reqwest` 0.13.5 and
`zstd` 0.14.0 are already present and nothing above adds a second major version of either — the
duplicate-dependency check #844 asks for is `cargo tree -p colonizer-observability -d` showing no
duplicate major version of `reqwest`, `prost`, or `zstd-safe`.

Transports, in the order a release ships them:

| Transport | Status |
| :--- | :--- |
| OTLP/HTTP protobuf | default |
| OTLP/HTTP JSON | supported, same encode path minus the protobuf step |
| OTLP/JSON file sink | the `file` provider (#850) |
| OTLP/gRPC | cargo feature `otlp-grpc` (#851), **not** in release binaries |

OTLP 1.x as `opentelemetry-proto` 0.33 ships it (proto schema v1.7+). OTLP/JSON follows the spec's
JSON mapping (hex-encoded ids, lowerCamelCase fields, 64-bit integers as JSON strings, enums as their
integer values) — verified by golden fixture tests in the add-on crate and by a real OTel Collector in
CI (#864), not inferred from `with-serde` compiling.

## Backend limits and size budgets

| Backend default | Value | Drives |
| :--- | :--- | :--- |
| Tempo `max_bytes_per_trace` | 5 MB | `max_trace_bytes` stays under it with headroom |
| Tempo distributor attribute truncation | 2 KiB | `max_attribute_bytes` default 1 KiB, well under |
| Loki `max_line_size` | 256 KB | `max_content_bytes` hard max 192 KiB (196608 bytes), under it |
| Loki structured metadata | 64 KB / 128 entries per line | attributes carried as metadata stay far below both |
| Typical OTLP receiver request cap | 4 MiB | requests split below 1 MiB by default, 4 MiB hard max |

Rules:

1. `max_attribute_bytes` (default 1024, range 128–8192): any single attribute value longer than this
   is truncated with a trailing `…(N more bytes)`; never applies to log/span *bodies*, only attributes.
2. `max_content_bytes` (default 32768, hard max 196608): a content-tier string (prompt, completion,
   tool output, question/answer text) longer than this is truncated the same way, and always travels
   as a log record's body, never as a span attribute — this is why content never blows the trace
   budget.
3. `max_trace_bytes` (default 4194304): one trace's encoded-span budget, tracked as a running "bytes
   used" counter per trace in `state.json`, advanced only as spans are encoded and committed — an
   exported span is never retracted, so the counter never re-walks spans already sent. A **64 KiB
   reserve** out of that budget covers the `invoke_agent` root span and every `turn` span (the trace's
   spine); those are never dropped. Once the rest is spent, new `execute_tool`, `subagent`, `chat`,
   `question` and `host_step` spans not yet encoded are dropped instead of sent, oldest-pending-first —
   never evicting an already-exported span — incrementing a per-trace `dropped_spans` counter in
   `state.json` (the root span's own `colonizer.trace.dropped_spans` attribute once it is finally built
   and sent, at outcome or `root_after_idle`) and `colonizer_observability_spans_dropped_total` at
   once, labeled by `kind`. Both counters commit atomically with the cursor in the same `state.json`
   write, so a replay from an earlier cursor reproduces the same counts and drop decisions.
4. Export requests are split below 1 MiB by default (a conservative default under most receivers'
   caps, keeping retries cheap) and never built above 4 MiB (`timeout_secs` and backend headroom
   both argue against a request near the common ingest ceiling).
5. These caps are privacy rules too (P9 below): the same truncation that keeps a trace inside Tempo's
   limits is also what keeps one long pasted secret from inflating a whole trace past an operator's
   backend quota.

## Privacy

Numbered so each is independently testable.

- **P1.** Four independent switches gate what is read at all: `stream_operational`,
  `stream_activity`, `stream_traces`, `stream_metrics` (settings.rs, already landed). A stream that
  is off is never read, not just never sent.
- **P2.** Every record defaults to the **structure tier**: ids, types, timings, tool *names* (not
  arguments), sizes, token counts, cost, status/error codes, and the allowlisted keys the source
  inventory table names. Nothing outside a record's own allowlist is ever read off that record.
- **P3.** The **content tier** — prompts, completions, tool arguments and results, question/answer
  text, file paths, command lines, tool error message text, `activity.detail` for `chat`/`question`/
  `answer` kinds, PR and issue titles — is exported only when **all** of: (1) the install switch
  `conversation_content` is on; (2) the colony's org has explicitly opted in,
  `settings.observability.content = true` in `orgs.json`, absent meaning off — deliberately **not**
  `effective_deja_enabled`'s inherit-from-install pattern (`orgs.rs:625`), an org must say yes itself;
  (3) the colony's sensitivity (`sensitivity.rs`) is known and not `Restricted` — missing or
  unrecognised sensitivity fails closed, same direction as `Restricted` itself.
- **P4.** `conversation_thinking` is a fourth, independent switch on top of P3: thinking content
  needs P3's three conditions **and** `conversation_thinking` on.
- **P5.** Content never becomes a span attribute. It travels only as a log record's body, correlated
  to the relevant trace/span id by attribute (`colonizer.trace.id`, `colonizer.span.id`) — so turning
  content off removes log records, never collapses a span's own structure.
- **P6.** All four gates (P1–P4) are enforced **in the add-on, at export time** — in the live
  exporter, in preview, in backfill, and in offline export. The cockpit's toggles explain the rule;
  they are not where it is enforced, since a stale cockpit build must not become a privacy hole.
- **P7.** Every exported string is passed through `redact_text`/`redact_value` **again** at export
  time, even though the source files were already redacted when written (`redact.rs`). Double
  redaction: a detector added after a line was written still catches it on export.
- **P8.** Never sent, regardless of any switch: secret-store values; header values, including the
  `observability-headers` secret's own value; gateway request/response bodies; images or base64
  blobs (replaced with a `{type, size}` placeholder); `transcripts/`; `chats/`; credential files.
- **P9.** Repository and organisation names are sent in plain by default. `repo_names = hashed`
  replaces `owner/repo` with `hmac-sha256(key, name)[..12]` (hex, 12 bytes → 24 hex characters), keyed
  by a per-install key at `<data>/observability/hash.key` (mode 0600; a fleet copies this key between
  members so their hashes of the same repo name agree — a joinable hash, not a per-machine salt).
  `hashed` also drops branch names, PR URLs, and issue/PR titles outright (titles are content tier
  and already gated by P3; `hashed` removes them even when P3 is satisfied, since a title plus a
  hashed repo is often enough to re-identify the repo).
- **P10.** The attribute/content/trace byte caps (max_attribute_bytes, max_content_bytes,
  max_trace_bytes) are privacy rules, not just backend-fit rules: truncation bounds how much of a
  long paste (which may itself carry something sensitive past the redactor) ever leaves the machine.

## Conversation content is per-org opt-in

(P3 above states the rule; this section is the rule's own heading so
[decisions.md](../decisions.md) can link straight to it.) The three conditions are conjunctive and
every one fails closed: no org setting, no sensitivity, or `Restricted` sensitivity all mean "no
content from this colony", independent of the install switch. This is the one place in the privacy
design that does not mirror an existing inherit-from-install pattern in the repo — `effective_deja_
enabled` was the closest existing precedent and was deliberately not followed, because recall
(deja) only ever affects the colony itself, while content export sends words outside the machine the
org agreed to run on.

## Config: settings, environment, secrets

The `observability` module kind (settings, defaults, validation) already landed — see
[Settings and privacy](../observability/settings.md) for the full field table; this section covers
only what #840 did not: environment overrides and the master switch.

| Env var | Effect |
| :--- | :--- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | overrides `endpoint` |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | overrides `protocol` |
| `OTEL_EXPORTER_OTLP_HEADERS` | overrides the `observability-headers` secret's parsed value for this run only — never written back to the secret |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | overrides `timeout_secs` |
| `OTEL_EXPORTER_OTLP_COMPRESSION` | overrides `compression` |
| `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_{ENDPOINT,PROTOCOL,HEADERS,TIMEOUT,COMPRESSION}` | per-signal variants, take precedence over the unsuffixed form for that signal only (traces can go to a different collector than logs) |
| `OTEL_RESOURCE_ATTRIBUTES` | merged into the resource attributes below, `k=v,k2=v2`, additive |
| `OTEL_SERVICE_NAME` | overrides `service.name` (default `colonizer`) |
| `OTEL_SDK_DISABLED` | `true` forces export off for this process, overriding a saved+enabled module — the one override that can only turn export *off* |
| `COLONIZER_OBSERVABILITY=on` | master switch for an **env-only** deployment: with no module saved (or saved disabled), this plus `OTEL_EXPORTER_OTLP_ENDPOINT` is enough to export, every other field at its settings.rs default. With a module saved and enabled, this does nothing extra |

Rules:

1. Effective config = saved module settings, overlaid field-by-field by any `OTEL_*` variable set;
   `OTEL_SDK_DISABLED=true` wins over everything. No `OTEL_*` variable enables export from an absent
   or disabled module by itself — only `COLONIZER_OBSERVABILITY=on` does, so a container can be fully
   configured by environment without calling the settings API.
2. Must not collide with `COLONIZER_TELEMETRY_ENDPOINT` (the live map's own variable): different name,
   different subsystem — never `COLONIZER_OBSERVABILITY_ENDPOINT` or similar.
3. Effective-config provenance (#841): the status API reports, per field, whether its value came from
   the module, an env var (naming which), or a default.
4. Auth headers live only in the `observability-headers` secret (or its env override above); the
   endpoint continues to refuse userinfo and query strings (settings.rs, already enforced).
5. `GET /metrics` (Prometheus) is a separate switch (`prometheus`) from OTLP export, and requires the
   install-wide **read** token, not the OTLP headers secret, which a scraper should never see.

## Performance

1. One background task per mothership (the add-on process), not one per colony.
2. CPU-bound work (encoding, gzip, id/`repo_names=hashed` hashing) runs in `spawn_blocking`, keeping
   the async reactor free for I/O.
3. Reads are budgeted in bytes and lines per tick, round-robin across sources, bounded overall by
   `max_read_mib_per_sec` (default 8) — throttles the tailer's read rate, never the colony's writes.
4. Memory is bounded by one batch at a time, plus the open-span state (capped — see the drop rule in
   [Backend limits](#backend-limits-and-size-budgets)).
5. `state.json` (cursors) is written at most once per second, after a backend ack, not per line.
6. Target: ≤5% of one core at 20 busy colonies; no measurable change to `handle_agent_event` latency
   (`events.rs:249`) — the add-on only reads files the mothership already writes.

## Fleet: resource attributes and relay

Every mothership exports with its own credentials; secrets never cross machines (issue #691 holds).
Resource attributes:

| Attribute | Value |
| :--- | :--- |
| `service.instance.id` | this mothership's `host_id` |
| `colonizer.fleet.id` | see below |

**Choice made here, not specified by the issue:** `colonizer.fleet.id` is the fleet **owner's**
`host_id`. A lone mothership is a fleet of one, so its fleet id equals its own `service.instance.id`
until the day it joins or starts a fleet. Rejected: `member_id` (`fleet_members.rs:78`) is
per-membership, not per-fleet; `owner_url` is a network address that can be re-pointed without the
fleet's identity changing.

Needs one small addition `fleet_members.rs` lacks today: `Membership` (`fleet_members.rs:~103`)
stores `owner_url` and `member_id` but not the owner's `host_id`. The join-confirm response needs to
also return it, stored alongside `Membership` — new mothership work, flagged against #841 (the issue
already computing `OTEL_RESOURCE_ATTRIBUTES`), not an add-on concern.

Relay: the owner may optionally relay what members already independently push to their own backends
(off by default, structure tier only even when on) — this does not replace a member's own export, and
never carries a member's secrets; it is a second, redundant copy at the owner's discretion, built on
`fleet_sync.rs`'s existing push of member history to the owner (#861).

## Mothership process logs via tracing

The mothership's own process logs (today, `eprintln!` scattered through the crate) move to the
`tracing` crate with a JSON file layer writing `<data>/logs/mothership.jsonl`
(`{ts, level, target, message, fields}`), rename-rotated the way `activity.jsonl` rotates to
`activity.jsonl.1` (#856). `stderr` is unchanged — the JSON file is additive, one more tailed source.
#857 migrates remaining `eprintln!` call sites and lints against new ones outside this layer.

## The exporter is a downloadable add-on, not part of the mothership

Adopted 2026-10-03, on the owner's request. The exporter — tailer, id/span mapping, OTLP transports,
metrics rendering, backfill, preview/offline-export — ships as crate `crates/colonizer-observability`,
binary `colonizer-observability`, built and checksummed with every release, in its **own** per-platform
tarball (`colonizer-observability-<target>.tar.gz`) listed in that release's `SHA256SUMS` and attested
the same way the main artifacts are (`.github/workflows/release.yml:298-305`). It is not in the default
bundle `scripts/install.sh --bundle` produces, and it is not linked into the mothership binary.

**Why:** install size and attack surface (no OTel/protobuf/gRPC in every install that never turns this
on), crash isolation (a malformed OTLP response can't take the mothership down with it), and a version
handshake keeps the two in lockstep. **Cost:** a second artifact per release platform, a version
handshake at spawn, and preview/test needing the add-on installed first.

**Alternatives considered and rejected:**

| Alternative | Rejected because |
| :--- | :--- |
| Cargo feature compiled into the mothership | release binaries are one-size; the owner wants it downloadable, not a separate build matrix |
| Dynamic library plugin | no stable Rust ABI to load across a mothership upgrade |
| WASM module | no filesystem or network access without a host-provided shim, which is most of the work anyway |

### Consequences

1. **CI gate:** `cargo tree -p colonizer-harness -e normal` must contain none of `opentelemetry`,
   `opentelemetry_sdk`, `opentelemetry-proto`, `prost`, `tonic`, `flate2` — true today (none are
   present) and enforced going forward so the mothership never grows this dependency tree by accident.
2. **Shared code:** redaction detectors (`redact.rs`), and possibly `sensitivity.rs` and the id
   formulas above, move into a new dependency-free crate, proposed name `crates/colonizer-redact`,
   used by both the mothership and the add-on. Extracted by the first add-on issue that needs it
   (#844); the mothership keeps calling the same functions through the new crate, not duplicating
   them.
3. **grpc stays refused:** `settings.rs`'s refusal of `protocol: "grpc"` "in this build" remains true
   for release builds even after #851 lands `otlp-grpc`, since that feature is not compiled into
   release binaries by default (table in [No OpenTelemetry SDK](#no-opentelemetry-sdk-in-the-export-pipeline)).

### Install and removal

`colonizer add observability` (CLI) and a one-click install in the cockpit's observability pane:

1. Download `colonizer-observability-<target>.tar.gz` of **exactly the mothership's own version**
   from that version's GitHub release.
2. Verify against that release's `SHA256SUMS`, then its build-provenance attestation — the same two
   steps `scripts/install.sh` performs for the main bundle (`sha256sum` compare, then
   `gh attestation verify` when `gh` is available; skip-with-a-note otherwise, fatal under
   `COLONIZER_REQUIRE_ATTESTATION=1`). Unlike Headroom's build-time pin (`headroom.lock` compiled in via
   `include_str!`), this is a runtime fetch against the running mothership's own version tag.
3. Unpack to `<data>/addons/observability/<version>/` with a small manifest (version, installed-at,
   verified-by: checksum or checksum+attestation).
4. `--from <path>` installs a locally built binary instead (source builds, air-gapped hosts); the
   version handshake below still applies.
5. A mothership update that finds the add-on installed fetches the matching add-on version too, same
   two-step verification.
6. `colonizer remove observability` stops the child and deletes the binary and its run state
   (`state.json`, `status.json`, the metrics file, the contract config). `hash.key` and the `file`
   provider's own output are kept unless `--purge` is also given — removing them loses a fleet's
   joinable hashing key and an operator's locally exported history, both of which outlive one add-on
   install.

### Supervision

1. The mothership spawns the add-on as a child process when the `observability` module is
   saved+enabled and the add-on is installed; stops it (SIGTERM, a grace period, then SIGKILL if it
   has not exited) when the module is disabled or removed.
2. Restarts with backoff: 1 s, doubling, capped at 60 s; resets after 10 minutes of continuous healthy
   running. **Not** a copy of `agent_link`'s backoff (`events.rs:90-153`: 1 s to a 10 s cap, resets on
   the next successful connection) — respawning a process costs more than reopening a WebSocket, so
   this is a deliberate superset (higher cap, a cooldown before reset), not a reuse of its numbers.
3. The add-on's `refused` exit code (78, see [Contract](#contract) below) is never retried — a version
   mismatch does not get better by restarting, it needs an update.
4. The child's environment is cleared to an allowlist (`PATH`, `HOME`, `TZ`, `LANG`) so it never reads
   the *service's* ambient `OTEL_*` variables itself; the mothership resolves every override (the
   env table above) and hands over the effective config through the contract instead.
5. Lower CPU priority on Unix (`nice 10`), so a busy exporter never competes with a colony's own CPU.
6. The child's `stderr` is captured into the mothership's own ops log (`<data>/logs/mothership.jsonl`,
   #856) rather than inherited raw, so one `docker logs`/`journalctl` view covers both processes.
7. The child exits when its `stdin` closes — the mothership's own death (crash or otherwise) is
   detected by the add-on without a heartbeat protocol.

### Contract

Versioned: `contract: 1`, bumped on any breaking change to the shapes below.

| Part | Direction | Shape |
| :--- | :--- | :--- |
| (a) the jsonl files | mothership → add-on, read-only | everything in the source inventory table |
| (b) `<data>/observability/exporter.json` | mothership → add-on, atomic write, mode 0600 | `{contract, mothership_version, host_id, fleet_id, data_dir, settings: {...effective ExporterConfig...}, policy: {<colony_id>: {org, repo, sensitivity, content: bool, thinking: bool}}}` |
| (c) secrets | mothership → add-on, one JSON line on the child's `stdin` at spawn | `{"headers": "k=v,k2=v2"}` — never argv, env, or disk; a secret change tears down and respawns the child |
| (d) `state.json`, `status.json`, `metrics.prom` | add-on → mothership, atomic writes | cursors (add-on's own, versioned); `{heartbeat_ts, lag_by_source, last_error, counters, version}` at most once/s; Prometheus text |
| (e) one-shot subcommands | mothership invokes, JSON on stdout | `preview`, `send-test-event`, `backfill`, `offline-export` |

Rules:

1. A colony **absent** from `policy` in `exporter.json` is treated as structure-only (fail closed) —
   the mothership writes an entry for every colony it knows about, including `content: false,
   thinking: false` ones, so "absent" only ever means "the mothership has not resolved this colony
   yet", not "this colony gets content by accident".
2. Version handshake: `colonizer-observability --version --json` lets the mothership check
   compatibility before spawning. If the installed add-on's `contract` or version does not match what
   the mothership expects, the add-on (once spawned) writes `status: "refused"` naming both versions,
   prints one line to stderr, and exits **78**; the cockpit offers "update add-on" instead of showing
   a generic crash.

## Delivery semantics

1. **Retry/backoff:** a failed export request is retried with the same 1 s-doubling backoff shape used
   elsewhere in the harness (capped; exact cap and jitter are #849's to tune), never blocking the next
   read tick.
2. **Bisect:** a `400`/`413` (request too large or malformed) splits the batch in half and retries each
   half, down to one record — isolating the bad record instead of wedging the whole batch.
3. **Partial success:** an OTLP response's `partial_success` (rejected count, error message) is
   logged and counted; rejected records still advance the cursor (an ack is an ack) and are not
   retried forever against a backend that keeps rejecting them for a reason that will not change.
4. **What is dropped and counted:** backlog older than `max_backlog_days` (default 7; `0` disables),
   size-budget drops (`max_trace_bytes`, see above), and backend-rejected records all increment named
   counters in `status.json`/`metrics.prom`, never silently.
5. **Commit:** a cursor only advances after a backend ack (or an accepted drop, per the rules above);
   cursor, open-span state and each trace's budget counters are written together in one atomic
   `state.json` write — never two writes a crash could tear apart — giving at-least-once delivery with
   no second spool.

## Cardinality and Loki labels

Only these become Loki labels (low-cardinality, safe to index): `service.name`, `colonizer.stream`
(`operational`/`activity`/`traces`/`metrics`), `severity`, and, only if an operator opts in explicitly
in the collector config, `colonizer.org`. Everything else — `colonizer.colony.id`, `colonizer.repo`,
trace/span ids, tool names — stays structured metadata or a trace/log attribute, never a label; a
label per colony or per repo is the cardinality explosion Loki's own docs warn against.

## Metrics catalogue

Placeholder: the concrete counters and histograms are #852's to define. Fixed in advance here: the
exporter's self-metrics (`colonizer_observability_spans_dropped_total`, read/backlog lag, export
latency and failure counts) live in the same `metrics.prom` the Prometheus switch exposes — one
metrics surface to scrape, not two.

## Consequences: issues #840–#865

| Issue | Crate(s) | Mothership-side part |
| :--- | :--- | :--- |
| #840 | `colonizer` | `observability` module kind, settings, validation — landed |
| #841 | `colonizer` | `OTEL_*`/`COLONIZER_OBSERVABILITY` resolution, resource attributes (incl. fleet id sync above), effective-config provenance |
| #842 | `colonizer-observability` | rotation/truncation-safe jsonl tailer, durable cursors — add-on only |
| #843 | `colonizer-observability` | source registry, fair read budgets, backlog guard, colony attribute join |
| #844 | `colonizer-observability`, `colonizer-redact` | OTLP proto types, encoding, batching, export policy enforcement, secret-canary test kit; extracts the shared crate |
| #845 | `colonizer-observability` | maps events/harness/activity/gateway/spend/decision (`routing.jsonl` shares its row shape)/finding/jev_ladder/jev_focus/mothership records to OTLP logs |
| #846 | `colonizer-observability` | root/turn/tool/subagent spans, deterministic ids, sampling |
| #847 | `colonizer-observability` | question, host-chain spans, per-trace size budget |
| #848 | `colonizer`, `colonizer-observability` | mothership: install switch + org opt-in storage, per-colony policy resolution into `exporter.json`; add-on: content log records, span-linked |
| #849 | `colonizer-observability` | exporter task, OTLP/HTTP transport, retry/backoff/bisect, atomic state commit, health |
| #850 | `colonizer`, `colonizer-observability` | mothership: status API, supervisor+contract wiring; add-on: OTLP/JSON file sink, clean shutdown, state reset |
| #851 | `colonizer-observability` | OTLP/gRPC behind `otlp-grpc`, off in release |
| #852 | `colonizer`, `colonizer-observability` | mothership: `/metrics` route, install-wide read token check; add-on: metrics catalogue, Prometheus rendering |
| #853 | `colonizer-observability` | OTLP metrics push, exporter self-metrics |
| #854 | `colonizer`, `colonizer-observability` | mothership: preview/send-test-event APIs, `colonizer observability` CLI shelling to the add-on; add-on: the one-shot subcommands |
| #855 | `web/` | cockpit pane: health card, test button, preview, content confirmation, org opt-in toggle |
| #856 | `colonizer` | process logs through `tracing`, JSON file layer |
| #857 | `colonizer` | migrate remaining `eprintln!`, forbid new ones (lint/CI) |
| #858 | `colonizer-observability` | backfill on-disk history |
| #859 | `colonizer`, `colonizer-observability` | mothership: read archived colonies for backfill; add-on: offline OTLP/JSON export |
| #860 | `colonizer`, docs | fleet setup docs, exporter health folded into member health |
| #861 | `colonizer` | fleet owner relay (off by default) |
| #862 | `examples/` | local Grafana stack + recipes |
| #863 | `examples/` | provisioned Grafana dashboards + lint |
| #864 | CI | conformance + secret-leak check against a real OTel Collector |
| #865 | — (deferred) | native Loki push — see [No native Loki connector](#no-native-loki-connector) |

## No native Loki connector

Not built now: a Loki push-API connector. The `file` provider plus the already-supported path of
running Alloy, Vector or Fluent Bit over those files (or over the OTLP/JSON file sink) covers Loki
today without the harness carrying a second wire format; a native push client is deferred to #865.

**Triggers that would reopen this:** a measured case where the collector hop meaningfully hurts
(added latency or memory on a resource-constrained host); enough operators asking for one-binary Loki
without a collector; or Loki adopting OTLP ingestion natively widely enough that "native connector"
stops meaning "a second wire format" at all.

Also not built now, neither deferred to a numbered issue: vendoring `.proto` files with `prost-build`
instead of depending on `opentelemetry-proto` (rejected unless `cargo audit` or a binary-size delta
someday argues otherwise); shipping colony data from inside the microVM (the exporter runs only on
the mothership, reading files it already received from the guest — a colony never dials a backend
itself).

## Alternatives considered

| Alternative | Rejected because |
| :--- | :--- |
| Vendor-specific connectors (Datadog agent protocol, etc.) per backend | OTLP already speaks to all of them; one wire format, one encoder |
| Collection only through the fleet owner | a member's own backend credentials would have to leave the member, breaking #691; offered instead as an optional, off-by-default relay of what a member already sends itself |
