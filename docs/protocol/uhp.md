# 7. Standard names (UHP)

Part of the [Colonizer protocol](../protocol.md).

Colonizer's wire names came before the open protocols. This section maps them
to the [Unified Harness Protocol](https://unifiedharnessprotocol.org/) (UHP,
version `2026-09-12`, draft), which extends the OpenAI Responses API, so that
Responses SDKs, SSE parsers and UI components can drive a colony unchanged.

**Status: partly implemented.** These tables are the contract to review before
any code; each one is implemented in its own change afterwards. #651 landed the
first pieces — the §7.1 surface rules (`/uhp` routes with `UHP-Version` and the
§7.7 error envelope), the session page and §7.5's artifact reads — and #650 the
read-side core (`crates/colonizer/src/uhp.rs`): `GET /uhp/v1/uhp` (discovery,
served without a credential), `GET /uhp/v1/harnesses` and
`/uhp/v1/harnesses/{id}`, `GET /uhp/v1/models` and `GET /uhp/v1/sessions/{id}`,
with version negotiation on every served `/uhp` route (any `UHP-Version` other
than `2026-09-12` is **400** `unsupported_protocol_version`) and the envelope on
every `/uhp` refusal, the credential ones included (**401**
`authentication_error`, **403** `permission_error`). Scoped API tokens are held
to the same scope and org/repo limits there as on `/api`. #650 then served the
task-bearing core (`crates/colonizer/src/uhp_responses.rs`): `POST
/uhp/v1/responses` (create and continue, §7.3), SSE streaming (§7.4),
`GET /uhp/v1/responses/{id}`, and cancellation of a response or a session
(§7.6); discovery reports `streaming` and `cancellation` true. Creating a
response needs a `launch` token; cancelling one, or a session, needs `operate`;
a `read` token gets **403** `permission_error`. Still proposed: input files
(§7.5), and the `/api/sessions` aliases these tables name (`previous_response_id`
and `metadata.harness_id` on `POST /api/sessions`, `Session.last_response_id`,
SSE on `GET /api/sessions/{id}/events`): the `/uhp` routes carry those today.
Until a table lands, nothing on the wire changes for it. The *Colonizer today* column is what works; how the served
surface measures against the UHP conformance suite is in
[docs/conformance.md](../conformance.md). The runner
contract (§2), the event definitions in `docs/agent-events.schema.json`, the
sandbox, the mesh and the publish path stay as they are: the standard names live
on the mothership's API, not inside the microVM. The one exception is §7.5's
input files, which a colony reads next to `session.json`.

## 7.1 Rules for every rename

- **Both names for one release.** A renamed field or route is accepted under the
  old and the new name for one release after the alias ships: alias in
  `v0.1.N`, old name removed in `v0.1.N+1`. A request that sends both with
  different values gets **400** `invalid_input`.
- **Replies use the new name.** Replies and stream frames carry the new name. In
  the deprecation release they carry the old one too, with the same value, so
  the web UI and scripts keep working; it goes when the alias goes.
- **Deprecation notice.** A request that uses an old request name makes the
  mothership append a `harness_log` entry at `warn` to that colony's
  `harness.jsonl`, once per name per colony.
  `scripts/colony-report.mjs --transcript <id>` prints `warn` entries, so the
  notice shows there. The request names that trigger it are `instructions`
  (§7.3). A name that only appears in replies, such as `agent` (§7.2), cannot be
  detected on the wire; its removal is announced in the CHANGELOG instead.

  ```json
  {"type": "harness_log", "level": "warn", "ts": "…",
   "message": "deprecated: \"instructions\" is now \"input\"; the old name goes in v0.1.N+1"}
  ```

- **Unknown input is still ignored**, as the rule at the top of this file says.
  UHP routes also list the ignored request fields in the reply's
  `metadata.ignored_fields`, comma-separated, because UHP metadata values are
  strings.
- **Colonizer extras keep their names.** Error codes UHP lacks take its vendor
  form, `colonizer_<code>`. Extra stream events are `colonizer.<type>` with the
  §2 `type`. UHP lets servers add event types; clients must skip unknown ones
  and read an unknown error `code` as its `type`.
- **Two surfaces, one handler.** UHP paths are served as route aliases under
  `/uhp` (`/uhp/v1/responses`, …), a prefix of their own beside `/api/…` and
  the web UI on the API listener; the model gateway (§6.5) is a separate
  listener that serves only `/providers/{id}/…`. An SDK takes
  `<mothership>/uhp/v1` as its base URL. `/uhp/v1/…` routes always answer in
  UHP shapes and send `UHP-Version: 2026-09-12`. `/api/…` routes keep
  Colonizer's reply shapes and use the UHP error envelope (§7.7) only when the
  request carries a `UHP-Version` header.

## 7.2 Harness selection

| Colonizer today | Standard (UHP) | Wire location | Compat notes |
| --- | --- | --- | --- |
| One agent module for the install: `agent.provider` in `modules.json` (`"claude-code"`), set with `PUT /api/modules/agent` | `metadata.harness_id` on the create request | new optional `metadata.harness_id` in the `POST /api/sessions` body | Absent: the install's active agent module, as today. The value is a module id (`modules/agents/<id>/module.json`); UHP treats it as opaque. Unknown or disabled: **404** `harness_not_found`. Per-org `agent` overrides still set model, Claude account and skillsets; the per-org `agent.module` additionally picks which installed agent module the org's colonies launch on, fixed at create (§6.3). Further runners plug in here. |
| `Session.agent` (`"claude-code"`), fixed at create | `harness_id` on a session; `metadata.harness_id` on a response | `Session` in REST replies and in the `session` WS frame | Sent beside `agent` in the deprecation release. UHP requires a server to report the harness it defaulted to but names no key; Colonizer uses `metadata.harness_id` on every response. |
| — | `harness_mismatch` | continuation (§7.3) | A continuation that names another `harness_id` than the colony's: **409** `harness_mismatch`. A colony keeps one runner for life. |
| `agent.module` in `/colonizer/session.json` | unchanged | inside the microVM | Not a wire name; the runner contract does not change. |

## 7.3 Continuation

A turn becomes a UHP response. Its id is
`resp_<session id>.<run epoch>.<turn>`, where `<turn>` counts the turns of that
run epoch from 1, so the id is known when the turn starts. Clients treat it as
opaque. It is derived from the event log rather than stored, so §2 and §3 do not
change. It resolves through `sessions.json` and the colony's event logs; both
survive a mothership restart, and colonies run detached and are reattached after
one, so the id keeps working across restarts.

| Colonizer today | Standard (UHP) | Wire location | Compat notes |
| --- | --- | --- | --- |
| No turn id: a turn is the stored events up to its `turn_end` | response `id` | new `Session.last_response_id`; the `id` of every SSE response (§7.4) | Additive. |
| Session id, the `{id}` in `/api/sessions/{id}/…` | `metadata.session_id` on every response; `GET /v1/sessions/{session_id}` | unchanged | UHP requires `metadata.session_id`. |
| `POST /api/sessions/{id}/resume`, no body: same worktree, fresh microVM, new run epoch | `previous_response_id` on create | `POST /api/sessions` with `previous_response_id` continues that colony instead of creating one | Resolved as below. The resume route stays and runs underneath. |
| WS `{"type": "user_message", "text": …}`, live colonies only | `input` on a continuation | the same `POST /api/sessions` | The WS command does not change. |
| `instructions` in the create body: the task text | `input` | `POST /api/sessions` | Not an in-place alias: UHP's `instructions` means system-level instructions. `input` becomes the task text; `instructions` keeps today's meaning through the deprecation release, and reusing the name is a later decision. |
| duplicate-issue check, `allow_duplicate` | `Idempotency-Key` header | `POST /api/sessions` | The header is honoured there now (issue #901, [harness-api.md](harness-api.md)): a repeat with the same key answers the colony that key created and starts nothing — the key is kept with the colony, so it survives a mothership restart where a 24-hour in-memory map would not — and a key already naming a colony on another repository is a **409**. The `/uhp/v1/responses` create above is the separate surface, still planned. The duplicate-issue check stays. |

How `POST /uhp/v1/responses` reads a request (#650):

| Field | Meaning |
| --- | --- |
| `input` | The task text: a string, or `message` items and `input_text` parts, joined. Required, at most 100,000 characters. An `input_file` or `input_image` part is **400** `invalid_input` while discovery reports `files_input` false. |
| `metadata.repo` | `owner/name`: the repository a new colony works on. Required to create, unread on a continuation. |
| `metadata.issue`, `metadata.title` | The issue number (string) and title of a new colony, as on `POST /api/sessions`. |
| `metadata.harness_id` | `chrn_<module>` or the module id. Unknown or switched off: **404** `harness_not_found`; installed but not the module the repository's colonies launch on, or not the colony's on a continuation: **409** `harness_mismatch`. |
| `stream` | `true` answers with the response's SSE stream (§7.4) instead of the object. |
| `previous_response_id` | Continue that colony, as below. |
| `Idempotency-Key` header | Per caller (owner or token), 24 hours, in memory: a repeat answers the first response and starts nothing; a mothership restart forgets the keys. |

Every other field, `model` and `instructions` included, is ignored and listed in
`metadata.ignored_fields` (§7.1). Validation refusals are **400** `invalid_input`
with `param` naming the field. A create runs `POST /api/sessions` underneath, so
its refusals keep their status in the envelope (a duplicate-issue **409** reads
`colonizer_conflict`, a scoped token's cap **429** `rate_limited`). An idle
colony's continuation is delivered as a follow-up message; a stopped or failed
one is resumed with `input` as the resume brief's note.

`previous_response_id` resolves by the colony's state:

| Colony | Result |
| --- | --- |
| `idle` | Delivered as a follow-up; the reply is the new response, `in_progress`. |
| `queued`, `starting`, `running`, `waiting_for_answer`, `publishing` | **409** `session_busy`: retry once that work ends. UHP has no input path into a running task; the WS command keeps working as today. |
| `stopped` or `failed`, worktree kept | Resumed as by `POST …/resume`, with `input` added to the resume brief; the reply is that brief's response, turn 1 of the new run epoch, `in_progress`. |
| `pr_opened`, `merged`, `closed`, `no_changes`, or the worktree is gone | **404** `session_expired`. |
| names an older turn than the colony's latest | **409** `colonizer_not_latest`, with `detail.latest_response_id`: a colony cannot fork. |
| unknown or malformed | **404** `response_not_found`. |

## 7.4 Streaming

The browser keeps its WebSocket, and `web/src/sessionStream.ts` does not change.
The SSE stream is a projection of the same stored log, one response (turn) per
stream, so the §2 events keep their names inside the microVM and on the WS.

| Colonizer today | Standard (UHP) | Wire location | Compat notes |
| --- | --- | --- | --- |
| `GET /api/sessions/{id}/events?since=<seq>&epoch=<epoch>`, WebSocket, the whole colony | SSE (`text/event-stream`) of one response: `POST /v1/responses` with `stream: true` | the same `GET` without an upgrade and with `Accept: text/event-stream`: the current turn, or the one named by `response_id=` | The WS frame types and the §2 events do not change; the `Session` in the `session` frame follows §7.1. |
| `seq`, from 1 in each run epoch; the `run_epoch`, `session`, `harness_log` and `memory_proposed` frames have none | `sequence_number`, from 0 and +1 per event within a stream | every SSE event | Counted per stream, so it never equals `seq`. The SSE `id:` line carries `<epoch>.<seq>` of the last stored event sent; `Last-Event-ID` with that value resumes like `since` and `epoch` (UHP leaves the format open). |
| `turn_end`: `result`, `is_error`, `cost_usd`, `duration_ms`, `model_usage` | one terminal event carrying the whole response: `response.completed`, `response.failed` or `response.incomplete` | the last SSE event | `usage` (from `model_usage`) and `cost_usd` are this turn's: the `turn_end` values minus the previous turn's, since §2 makes them cumulative. `cost_usd` and `duration_ms` go in `metadata` as strings, since UHP metadata values are strings. |

| §2 event | SSE event(s) |
| --- | --- |
| the first event of a turn | `response.created`, then `response.in_progress` |
| `user_message` | none: it is the response's input |
| `assistant_text_delta` | `response.output_text.delta` |
| `assistant_text` | `response.output_text.done`, then `response.output_item.done` for the `message` item |
| `thinking` | `response.reasoning_summary_part.added`, one `response.reasoning_summary_text.delta` with the whole block, `response.reasoning_summary_part.done` |
| `tool_call` | `response.output_item.added` and `.done`, item `type: "function_call"` with `call_id`, `name`, and `arguments` as a JSON string. UHP uses `function_call` for tools the harness runs itself; a client never answers it. |
| `tool_result` | `response.output_item.done`, item `type: "function_call_output"` with the same `call_id`; UHP puts it in `output` |
| `question`, `question_answered` | `colonizer.question`, `colonizer.question_answered`, with the §2 body. UHP has no way to ask the user mid-task; answers still go through the WS `answer` command. |
| `memory_proposal`, `finding`, `log` at `warn` or `error` | `colonizer.memory_proposal`, `colonizer.finding`, `colonizer.log` |
| `status` | nothing, except `exited` with no `turn_end`, which ends the response by how the colony stopped (§7.7): `response.failed` with `response.status: "cancelled"` after Stop, `response.incomplete` after a budget stop, otherwise an `error` event (`harness_error`) and then `response.failed` |
| `turn_end` | `is_error: false`: `response.completed`. `is_error: true`: `response.failed` with `error.code` from §7.7. After an interrupt: `response.failed` with `response.status: "cancelled"`, which UHP makes authoritative. |

Served (#650) as `POST /uhp/v1/responses` with `stream: true` and as
`GET /uhp/v1/responses/{id}?stream=true`, which resumes after `Last-Event-ID`.
A text block opens its `message` item with `response.output_item.added` and
`response.content_part.added` before its first delta, and closes it with
`response.content_part.done`, as Responses SDKs expect; a `thinking` block is a
`reasoning` item. A stream whose colony stops without `exited` ends from the
colony's record within a few seconds. An interrupt is remembered in memory, so
after a mothership restart an interrupted turn that already finished reads
`failed` rather than `cancelled`.

A subagent event keeps its `agent` ref as an extra field on its item or event.
The projection gets its own schema when it is built;
`docs/agent-events.schema.json` describes §2 and does not change.

## 7.5 Files

| Colonizer today | Standard (UHP) | Wire location | Compat notes |
| --- | --- | --- | --- |
| None: the task is text (issue plus `instructions`) and follow-ups are plain text | `input_file` content part, with `file_data` (a data URL) and `filename`, or with a `file_id` from `POST /v1/files` | `input` items on `POST /api/sessions`, creating or continuing; uploads on `POST /api/files` | The mothership keeps the files under `<session dir>/in/` and copies them into `vm/` at each boot, so the agent reads them at `/colonizer/in/<filename>`, next to `session.json` (§1), in the mount the host already makes read-only: no new mount and no change to isolation. §1 gains that row when this is built. The prompt lists them. Over the documented size limit: **413** `file_too_large` with `detail.max_bytes`. |
| `/harness/out/pr.md`: a regular file (no symlink) of at most 256,000 bytes, read by the mothership after the microVM is gone | artifacts: `GET /v1/sessions/{session_id}/files` → `{"files": [File]}`, `GET /v1/containers/{container_id}/files/{file_id}/content`, `GET /v1/sessions/{session_id}/files/archive` | `GET /api/sessions/{id}/files`, `…/files/{name}/content` and `…/files/archive`, over `<session dir>/out/` | Read-only, regular files only, with the `pr.md` checks. `container_id` is `cntr_<session id>`. `pr.md` is listed like any other file. The worktree is not an artifact: it still reaches the repository through the publish path, which does not change. |

## 7.6 Cancellation

| Colonizer today | Standard (UHP) | Wire location | Compat notes |
| --- | --- | --- | --- |
| `POST /api/sessions/{id}/stop` → `{"result": "stopped"}` or `"already_stopped"`, plus the session | `POST /v1/sessions/{session_id}/cancel` → `{id, status}` | `POST /uhp/v1/sessions/{id}/cancel`, on the stop handler | `POST /api/sessions/{id}/stop` keeps its name and reply. The UHP reply takes its `status` from §7.7. |
| WS `{"type": "interrupt"}`: ends the current turn, no reply frame | `POST /v1/responses/{response_id}/cancel` → the response | `POST /uhp/v1/responses/{response_id}/cancel` | Interrupts that turn if it is still running. The colony and the output so far are kept, as UHP requires. |

Served (#650). `POST /uhp/v1/sessions/{id}/cancel` answers
`{"id", "object": "session", "status", "metadata": {"colonizer_result"}}`, with
`status` `cancelled` for a colony this stop ended or one stopped by hand,
`completed`, `failed` or `incomplete` for one that ended otherwise, and
`colonizer_result` the stop's `stopped` or `already_stopped`.
`POST /uhp/v1/responses/{id}/cancel` answers the response, `status: "cancelled"`
for a turn it ended.

Every cancel is safe to retry: a repeat changes nothing and is not an error
(Stop answers `already_stopped`, a response cancel returns the finished
response). That holds after a mothership restart too, because the state lives in
`sessions.json` and a colony whose microVM is gone after the restart is already
`stopped`.

| State when the cancel arrives | Stop or session cancel | Response cancel |
| --- | --- | --- |
| `queued` | Leaves the queue: `stopped`, `result: "stopped"`. No microVM or parallel slot was claimed. | A continuation waiting here ends `cancelled`, and the colony leaves the queue as with Stop. |
| `starting`, `running`, `waiting_for_answer`, `idle` | microVM stopped, worktree kept: `stopped`, `result: "stopped"` | A turn that has not finished ends `cancelled`: interrupted if it is running, or, while the microVM is still booting, the colony is stopped as with Stop. A finished one: **200**, unchanged. |
| `publishing` | Today **409** `"session is not running"`; becomes **409** `session_busy`. A retry after publishing gets `already_stopped`. | **200**, unchanged |
| `pr_opened`, `merged`, `closed`, `no_changes`, `stopped`, `failed` | **200** `result: "already_stopped"`; nothing changes | **200**, unchanged |
| unknown id | Today **404** `"no such session"`; becomes **404** `session_not_found`. | **404** `response_not_found` |

## 7.7 Errors

| Colonizer today | Standard (UHP) | Wire location | Compat notes |
| --- | --- | --- | --- |
| `{"error": "<message>"}` with the HTTP status, no code | `{"error": {"type", "code", "message", "param", "detail"}}` | UHP routes always; `/api/…` when the request sends `UHP-Version` | Otherwise `/api/…` keeps the string and adds a sibling `"code"`. The string form goes one release after the web UI reads the envelope. |
| `Session.error` (free text) and `Session.attention.reason` | response `status`, `error.code`, `incomplete_details.reason` | new `Session.error_code`; every UHP response | `error` and `attention` stay. The watchdog, the autonomy judge and notifications read `error_code` and `attention.reason` instead of matching message text. |

A response is `in_progress` from its first event until its turn ends. UHP has no
`queued`, so a continuation waiting in the queue is `in_progress` too. How the
turn ended fixes its final status; later changes to the colony, such as Stop or
publishing, leave a finished response as it is. While a response runs,
`metadata.colonizer_attention` carries the colony's `attention.reason`, if it
has one.

| The turn ended by | UHP `status` |
| --- | --- |
| `turn_end` with `is_error: false` | `completed` |
| `turn_end` with `is_error: true` | `failed`, with a code from the table below |
| an interrupt, or Stop while it ran | `cancelled` |
| Stop while `attention.reason` was `nudges_exhausted` | `failed` with `colonizer_agent_stalled`: the stop is the stall's outcome |
| the spend budget, the host-disk quota or the max session length | `incomplete` |
| the plan's quota running out | `failed` with `quota_exhausted` |
| the runner or its microVM going away | `failed` with `harness_error` |

In the class table, *retryable* means the same request may succeed later, and
*idempotent* means sending it again changes nothing beyond the first attempt:
the same answer, no second microVM, no new spend.

| Class | UHP `type` / `code` (HTTP) | Colonizer today | Retryable | Idempotent |
| --- | --- | --- | --- | --- |
| Invalid input | `invalid_request_error` / `invalid_input` (400), `harness_mismatch` (409), `colonizer_not_latest` (409), `file_too_large` (413) | **400** `{"error": …}` from create's checks | No: fix the request | Yes |
| Not found | `invalid_request_error` / `session_not_found`, `response_not_found`, `harness_not_found`, `file_not_found` (404) | **404** `"no such session"` | No | Yes |
| Expired | `invalid_request_error` / `session_expired` (404) | **409** "this colony can't be resumed…" on a colony that cannot resume | No | Yes |
| Busy | `invalid_request_error` / `session_busy` (409) | **409** `"session is not running"` while publishing; "this colony can't be resumed…" on a live colony (a suspended one waiting for an answer does resume) | Yes, once the running work ends | Yes |
| Harness unavailable | `server_error` / `harness_unavailable` (503) before a run; `harness_error` / `harness_error` in a failed response | attention `agent_failed`; `Session.error` when agentd never became ready or the microVM was gone after a restart; `status` `exited` with no `turn_end` | Yes: Resume boots a fresh microVM on the same worktree | No: each try boots a microVM |
| Provider failure | `harness_error` / `provider_error` in a failed response; `model_unavailable` (422) if refused up front | attention `model_error` (an upstream 4xx or 5xx ended the last turn); gateway `api_error`, `overloaded_error`, `authentication_error` | 5xx and overload: yes, with backoff. Auth: no, until the credentials are fixed | No: a retry is a new turn and spends tokens |
| Agent stall | `harness_error` / `colonizer_agent_stalled` | watchdog `stalled` while nudging, then `nudges_exhausted`; both show in `metadata.colonizer_attention` while the response is `in_progress`, and the code applies once the colony is stopped after `nudges_exhausted` | Yes: a follow-up or Resume | No |
| Rate limited | `rate_limit_error` / `rate_limited` (429) | an upstream 429 through the gateway | Yes, after `Retry-After` | Yes |
| Quota exhausted | `rate_limit_error` / `quota_exhausted` (429) | attention `provider_quota_exhausted`; the colony is parked ([#213]) | No, until the plan resets; Colonizer resumes parked colonies itself | Yes |
| Budget stop | no error: `status: "incomplete"`, `incomplete_details.reason` `colonizer_spend_budget`, `colonizer_host_disk_quota` or `colonizer_max_session_length` | `stopped` with `error` "passed its spend budget …"; gateway **403** `permission_error` once over; `stopped` past the host-disk quota; the microVM stopped at the max session length | No, until the budget is raised; then Resume | Yes |
| Cancelled | no error: `status: "cancelled"` | Stop, `interrupt` | Nothing to retry: continue or Resume | Yes |
| Internal | `server_error` (500); `colonizer_publish_interrupted` in a failed response | **500** `{"error": …}`; a restart mid-publish | Yes, with backoff | Yes, except create without `Idempotency-Key` |

Provider failure and agent stall stay apart: one is the model refusing, the
other the agent not making progress, and they need different fixes.
`waiting_for_answer` and `autopilot_held` are not classes: the first is a
question waiting for a person, the second autopilot declining to publish after
the turn's own error. UHP uses `incomplete` for budgets and does not reconcile
that with its `timeout` code, so Colonizer does not emit `timeout`.
