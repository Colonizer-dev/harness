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
│   └── execute_tool Task                 a Task (or Agent) call …
│       └── subagent Explore              … and the subagent it started
│           └── execute_tool Read         the subagent's own tool calls
└── turn 2
```

| Span | Starts | Ends | Attributes |
| :--- | :--- | :--- | :--- |
| `invoke_agent <repo>` | the colony's creation | its final outcome | `gen_ai.operation.name` = `invoke_agent`, `gen_ai.agent.id` and `colonizer.colony.id` (the colony id), `gen_ai.agent.name` (the agent module, `claude_code`), `colonizer.repo`, `colonizer.org`, `colonizer.origin` (`user`, `burn_down`, …), `colonizer.outcome`, `colonizer.pr.url`, `gen_ai.response.model`, total `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.usage.cache_read.input_tokens`, `gen_ai.usage.cache_creation.input_tokens` and `colonizer.cost_usd`; events `colonizer.suspended`, `colonizer.restored`, `colonizer.stopped` |
| `turn <n>` | the message that starts it (or its first event) | its `turn_end` | `colonizer.origin` (`user`, `watchdog`, `autonomy`, …), `gen_ai.response.model`, and this turn's own tokens and `colonizer.cost_usd` (the change since the previous turn, not the running total); `error.type` = `turn_error` when the turn failed |
| `execute_tool <tool>` | the `tool_call` | its `tool_result` | `gen_ai.operation.name` = `execute_tool`, `gen_ai.tool.name`, `gen_ai.tool.call.id`, `colonizer.tool.output_bytes` (a size, never the output), `colonizer.denial.class` (`egress`, `read_only`, …) when the sandbox refused it; `error.type` = `tool_error` when it failed |
| `subagent <type>` | its Task call | the Task's result, or its `subagent_end` when it ran in the background | `gen_ai.operation.name` = `invoke_agent`, `gen_ai.agent.id` (the Task call's id), `gen_ai.agent.name` (the subagent type); `error.type` = `subagent_failed` |

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
same spans. A refused credential (401, 403) holds the spans like any other record.

The `colonizer.agent_event` log records of the same lines are still sent while **Colony activity**
is on: they are the per-event view (with the record id a backend can deduplicate on), the trace is
the per-colony one, and either can be switched off alone.

## Sampling

**Trace sample ratio** (0 to 1, default 1) keeps or drops whole colonies, never parts of one: a
colony is traced when the first 8 bytes of its trace id, read as a number, fall below that share of
the range. Every machine and every replay decides the same way. Logs and metrics are never sampled.
