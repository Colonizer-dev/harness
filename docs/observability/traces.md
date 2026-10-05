# Traces

With **Traces** on, every colony becomes one OpenTelemetry trace, from its launch to its outcome,
exported over the same OTLP/HTTP connection as the logs (`/v1/traces`, or
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`). The spans are rebuilt from the ledgers the harness already
writes (a colony's `events.jsonl`, and the outcomes in `activity.jsonl`); nothing in a colony's path
runs a tracer. Spans carry structure only: names, ids, times, token counts, cost, models and error
classes. A prompt, a tool's input or output, a path, a command or an answer never becomes a span
attribute, whatever the content switches say ([export policy](export-policy.md)).

## The span tree

```text
invoke_agent acme/widgets                 the colony (root)
├── turn 1                                one turn: a message to its turn_end
│   ├── execute_tool Bash                 one tool call: tool_call to tool_result
│   ├── execute_tool Task                 a Task (or Agent) call …
│   │   └── subagent Explore              … and the subagent it started
│   │       └── execute_tool Read         the subagent's own tool calls
│   └── question                          a question, to its answer
├── turn 2
├── chat qwen/qwen3-coder                 one routed model call through the gateway
└── host_step verification                one host-chain verdict
```

| Span | Starts | Ends | Attributes |
| :--- | :--- | :--- | :--- |
| `invoke_agent <repo>` | the colony's creation | its final outcome | `gen_ai.operation.name` = `invoke_agent`, `gen_ai.agent.id` and `colonizer.colony.id` (the colony id), `gen_ai.agent.name` (the agent module, `claude_code`), `colonizer.repo`, `colonizer.org`, `colonizer.origin` (`user`, `burn_down`, …), `colonizer.outcome`, `colonizer.pr.url`, `gen_ai.response.model`, total `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.usage.cache_read.input_tokens`, `gen_ai.usage.cache_creation.input_tokens` and `colonizer.cost_usd`; events `colonizer.suspended`, `colonizer.restored`, `colonizer.stopped` |
| `turn <n>` | the message that starts it (or its first event) | its `turn_end` | `colonizer.origin` (`user`, `watchdog`, `autonomy`, …), `gen_ai.response.model`, and this turn's own tokens and `colonizer.cost_usd` (the change since the previous turn, not the running total); `error.type` = `turn_error` when the turn failed |
| `execute_tool <tool>` | the `tool_call` | its `tool_result` | `gen_ai.operation.name` = `execute_tool`, `gen_ai.tool.name`, `gen_ai.tool.call.id`, `colonizer.tool.output_bytes` (a size, never the output), `colonizer.denial.class` (`egress`, `read_only`, …) when the sandbox refused it; `error.type` = `tool_error` when it failed |
| `subagent <type>` | its Task call | the Task's result, or its `subagent_end` when it ran in the background | `gen_ai.operation.name` = `invoke_agent`, `gen_ai.agent.id` (the Task call's id), `gen_ai.agent.name` (the subagent type); `error.type` = `subagent_failed` |
| `question` | the `question` | its `question_answered` | `colonizer.question.risk`, `colonizer.question.kind` (`exec_policy` for an exec-policy ask), `colonizer.question.blocking`, `colonizer.question.options` (a count), `colonizer.answered_by` (`user` or `autonomy`, the judge); `colonizer.unanswered` when the root closes it. Never the question or the answer. |
| `chat <model>` (client) | the gateway accepted the request | its last byte (`ts` + `duration_ms`) | `gen_ai.operation.name` = `chat`, `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.response.model` (the model sent upstream), `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `colonizer.gateway.status`, `colonizer.gateway.wire`, `colonizer.gateway.queue_ms`, `colonizer.fallback`; `error.type` = the failure code (`upstream_error`, `queue_full`, …). One span per attempt, so a retry is a second span. |
| `host_step <step>` | the host's line (a verification: its run's start) | the same instant (a verification: its verdict) | `colonizer.step`, and per step: `verification` `colonizer.verdict`, `colonizer.verify.exit_code`, `colonizer.verify.commits`, `colonizer.verify.files_changed` (a count); `screening` its mode, outcome and finding count; `validated` `colonizer.finding.severity`; `review` `colonizer.review.verdict`; `fix_colony` `colonizer.fix.colony`; `watchdog_turn_end` `colonizer.watchdog.after_secs`; `boundary` its kind and control; `path_policy` the access, policy and tool, never the path; `jev_ladder` applied, tokens before and after, trigger, and the kept and dropped counts |

Questions, gateway requests and host steps sit where they belong: a question under the turn (or
subagent) that asked it, open across turns until it is answered; a gateway request and a host step
under the root, since the gateway is tailed apart from the events and the host chain runs between
turns. A question's span is keyed by its `question_id`, a gateway request's by its line's digest,
and a host step's by its line's `seq`.

The root's status is `OK` for `merged`, `closed` and `no_changes`, `ERROR` for `failed`, and unset
for `idle` and `deleted`. Streaming text deltas, thinking, logs and status changes make no span. A
subagent whose `name` is free text (its task description, when the type was not named) gets no
name, since that text is content.

## Ids

Ids are computed, not random, so exporting the same ledger lines again (after a restart, a reset or
a backfill) produces the same spans with the same ids, and a backend that keys on them stores each
span once.

```text
trace id = sha256("colonizer.trace.v1|" + host_id + "|" + colony_id)[..16]
span id  = sha256(trace_id ‖ kind ‖ key)[..8]
```

`host_id` is the install's id (`service.instance.id`), `‖` is byte concatenation of the raw trace id,
the kind and the key, and the keys are: the colony id for `invoke_agent`, the turn number for
`turn`, the `tool_call_id` for `execute_tool`, and the Task call's `tool_call_id` for `subagent`.

## When spans are sent

A span goes out when it ends, so a running colony's trace fills in as it works: each tool call when
its result arrives, each turn at its `turn_end`. The root goes out last, once:

- at the colony's final outcome (`merged`, `closed`, `no_changes`, `failed`), once its `events.jsonl`
  has been read to the end;
- when the colony is deleted (`colonizer.outcome = deleted`);
- or, for a stopped colony, after 24 hours with nothing new (`colonizer.outcome = idle`).

Until then a backend such as Tempo shows the trace with its root "not yet received". A suspension, a
restore and a stop become events on the root, not separate spans, and a later outcome never sends a
second root. Whatever is still open when the root goes out is closed with
`colonizer.span.incomplete = true`. A tool call still open at its turn's end closes with
`colonizer.unmatched = true` (a background subagent and its tool calls run on). At most 4 096 spans
per colony are held open; past that the oldest closes with `colonizer.evicted = true`.

The open spans, turn counters and running totals are kept in `<data>/observability/state.json`,
written in the same atomic write as the read offsets and only after the backend acknowledged the
spans, so a restart resumes mid-turn with the same ids, and a crash before that write replays the
same spans. A refused credential (401, 403) holds the spans like any other record. The budget's byte
count is committed the same way, so a replay leaves out the same spans.

The `colonizer.agent_event` log records of the same lines are still sent while **Colony activity**
is on: they are the per-event view (with the record id a backend can deduplicate on), the trace is
the per-colony one, and either can be switched off alone.

## Long colonies and Tempo limits

A colony that runs for days can make more spans than a tracing backend keeps for one trace: Tempo
refuses whatever arrives past `max_bytes_per_trace` (5 MB by default), and the root arrives last.
**Max trace size** (`max_trace_bytes`, 4 MiB by default) keeps every trace under it. The exporter
counts the encoded bytes of every span it sends per colony (in `state.json`, with the offsets).
Past 90 % of the budget, tool calls, subagents, questions, gateway requests and host steps are
counted instead of sent: on their turn as `colonizer.spans_suppressed.<kind>`, and on the root as
the same counts, `colonizer.trace.dropped_spans` and `colonizer.trace_budget_exhausted = true`.
The last 10 % is kept for the turns and the root, which are always sent. A span's attribute values
are cut at 2 KiB (Tempo's distributor limit) with the `…(N more bytes)` marker and
`colonizer.truncated`, whatever **Max attribute size** says for log records.

To keep more of a long colony, raise Tempo's `max_bytes_per_trace` (in its `overrides`) and
`max_trace_bytes` together, the second below the first. Raising only `max_trace_bytes` makes Tempo
refuse the end of the trace, the root included; raising only Tempo's limit changes nothing here.

## Sampling

**Trace sample ratio** (0 to 1, default 1) keeps or drops whole colonies, never parts of one: a
colony is traced when the first 8 bytes of its trace id, read as a number, fall below that share of
the range. Every machine and every replay decides the same way. Logs and metrics are never sampled.
