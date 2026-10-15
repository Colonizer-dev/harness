# Living Brain

Part of the [Colonizer protocol](../protocol.md).

[Living Brain](https://livingbrain.wiki) is a team brain: a wiki and a memory. Connected, the
mothership files each colony's session there as an agent log with a searchable note, and can give
colonies the brain's MCP server so they search, ask and note it while they work. Everything is off
until the operator connects and adds a rule. The code is `crates/colonizer/src/living_brain/`.

## Connecting

Settings → Connections & secrets → **Living Brain** → **Connect Living Brain**. The mothership runs
the brain's OAuth device flow (RFC 8628) itself, as the `livingbrain-cli` client the brain's own CLI
uses: it asks `POST /v1/device-auth/code`, the cockpit shows the code and opens the verification
page, and the mothership polls `POST /v1/device-auth/token` (at the server's interval, five seconds
more on `slow_down`) until the person approves, denies, or the code expires. The device code never
leaves the mothership. A personal access token made at the brain's Settings → API tokens can be
pasted instead.

The token is a saved secret like a provider key — `<config>/living-brain/token`, in the system
keychain when it is usable, else a 0600 file (`.enc` under `COLONIZER_MASTER_KEY`) — listed on the
Secrets page as **Living Brain**, never returned by any route, and scrubbed from transcripts and logs
with the other saved secrets. Before saving, the mothership reads `GET /v1/tokens` with it (the one
identity read the brain allows a bearer token; `GET /v1/workspaces/me` is cookie-only) to refuse a
token the brain answers 401 for and to name it in the status line. **Disconnect** revokes it at the
brain (`DELETE /v1/tokens/{prefix}`), forgets it here and empties the send queue. The server
(default `https://app.livingbrain.wiki`) and MCP (default
`https://mcp.livingbrain.wiki/v1/pages/mcp`) addresses change only while disconnected: a token is
never sent to a host other than the one that issued it.

## Rules

One rule per org (`acme`) or repository (`acme/web`); a repository's rule wins over its org's, and no
rule is off. A rule has three settings:

| Setting | Meaning |
| :--- | :--- |
| `send` | File this target's colony sessions in the brain. |
| `detail` | The brain CLI's levels. `full` (default): every message, reasoning summary and tool call with its input (first and last 8 KiB) and output (first and last 2 KiB). `messages`: every message, tool names only. `summary`: messages cut to 2 000 characters, tool names only. |
| `mcp` | Give this target's colonies the brain's MCP server. |

Switching `send` on stamps the rule's `since`: only colonies active since then are sent on their
own. Older ones are sent by **Send existing colonies** for an org, which counts first (a dry run)
and queues only when confirmed.

## What is sent

A colony is sent once it has **settled**: its status is `idle`, `waiting_for_answer`, `parked`,
`pr_opened`, `merged`, `closed`, `no_changes`, `stopped` or `failed`, and nothing has been written
to its event logs for 30 minutes. A sweep every minute — and at once whenever a notification goes
out — queues every settled colony, active since the rule went on or sent before, whose fingerprint
(the record's schema and collector versions, status, pull request, cost and the size of its event
logs) differs from the one last sent. Because the versions lead the fingerprint, an upgrade that
changes the record (schema 3 nested the subagents) re-sends every colony already sent — backfilled
ones included — once, at the queue's usual pace (20 per pass, two seconds apart). Two requests per colony, both in the token owner's
personal scope:

- `POST /v1/pages/sources` `{kind: "agent_log", path: "colonizer session <id>", body, scope:
  "personal", project}`. `body` is the brain's normalised session record, schema version 3
  (Living Brain's `docs/logs.md`), compact JSON in this field order: `schema_version: 3`, `agent:
  "colonizer"`, `collector_version: 2`, `detail`, `session_id` (the colony id), `repo`
  (`owner/name`), `branch`, `models` (each `model_changed` model in order, then the sorted
  `model_usage` keys), `started_at` (the colony's `created_at`), `ended_at` (the last event the
  record used), `turns`, `totals` (from `model_usage`; `cache_creation_tokens` is
  `cache_write_tokens`; `cost_usd` is `cost_usd` plus `routed_cost_usd`, or null), `outcome` (the
  status), `truncated`, `omitted_turns`, `omitted_subagent_turns`, and `colony` (`org`, `base`, `issue`, `issue_title`,
  `pr_url`, `pr_opened_at`, `merged_at`, `ci_state`, `origin`; null when absent). `project` is the
  repository's name, so a repository's colonies meet the sessions its people sync with the CLI.
- `POST /v1/notes` `{slug: "agent-log-colonizer-<id>", title, body, project}`: the CLI's session note
  — header, outcome, detail, the source and a transcript link, then the conversation quoted to fit
  48 KiB; under a call that started a subagent, one line `> 🤖 **<name>** · <model> · <duration> ·
  <status> · <N> step(s) — <description>` and its final answer quoted (its own steps are in the
  transcript). The slug keeps one page per colony, updated in place.

The events become turns the way the brain's `colonizer` collector reads them. The logs are read
`events-1.jsonl` … then `events.jsonl`, and a line at or below the last `seq` seen in the same file
(agentd's re-send) is dropped. The first `initial` message is a user turn carrying the colony's
instructions (an event `Colony started on issue #N: <title>` or `Colony started` when there are
none); a later one is `Colony resumed` (`Red-team probe started` for a red-team colony). Harness
nudges — ids starting `watchdog-`, `verify-fix-`, `pr-rewrite-`, `playbook-`, `u-selfheal-`,
`u-retry-`, `u-quota-reset-`, or any origin but `user` — are one-line events; the person's own
messages are user turns. The agent's `assistant_text` makes assistant turns (one message id's blocks
joined by a newline), `thinking` a reasoning turn, and each `tool_call` a call `{name, format, path,
input, output, status, exit_code}` settled by its `tool_result` (a background launch ack is
skipped). A background subagent's last words, at its `subagent_end`, are its call's output
(`completed` → `ok`, `failed` → `error`, `stopped` → `interrupted`). A
`question` is an assistant turn listing the questions and options; `question_answered` is a user
turn when the person answered and the event `Answered automatically: …` otherwise. A `turn_end` in
error, a system `verification`, a `finding` and a `loop_stop` are one-line events. Deltas, log
lines, status changes, boundaries and memory proposals are dropped.

**Subagents.** A `Task`/`Agent` call starts a subagent, and every event the runner tagged with that
call as its `agent.id` (`origin: "subagent"`) is the subagent's own step: its text, thinking and
tool calls become turns under the call's `subagent`, by the same rules as the orchestrator's — and a
subagent's own `Task` call nests the subagent it starts the same way, to any depth. The block is
`{id, name, description, models, started_at, ended_at, status, totals, turns, truncated,
omitted_turns}`, every field present: `id` is the starting call, `name` the call's `subagent_type`
(else its description, else `subagent`), `description` the call's description, `models` the models
its messages ran on (`agent.model`, which the Claude Code runner sets from the SDK message; the
call's own `model` when no message said), `started_at` the launch and `ended_at` its last event,
result or `subagent_end`, `status` `completed` or `failed` (a foreground result, by `is_error`) or
`completed`/`failed`/`stopped` (a background `subagent_end`), null when the stream never said.
`totals` is zeros and a null cost: the events carry no per-subagent usage, and the colony's totals
already hold every model's spend. An assistant turn carries `model` where it changes from the
previous assistant turn in the same list (the orchestrator's from `model_changed`). A subagent whose
starting call the record does not hold is dropped.

Then, in the CLI's order: the detail level (what it leaves out is never touched again), home
elision, the 256 KiB per-message clip, redaction of every string value on its own (subagents'
descriptions and turns included; the levels apply inside subagents too, and a call keeps its
`subagent` at every level), the size cap (768 KiB raw, 992 KiB once JSON-escaped: first the
subagents, oldest first, each shedding its oldest turns down to its final answer — its last
assistant turn with text — behind an event saying how many, counted in its `omitted_turns` and the
record's `omitted_subagent_turns`; then the orchestrator's turns, where the opening turn stays, the
oldest after it go, and an event says how many), and one more redaction pass over the serialised body. Nothing in the record is
read from the clock, so one colony in one state is one byte string and the brain's
`(scope, sha256(body))` key dedupes a resend.

Redaction is the mothership's: the home directory becomes `~`, every saved secret's value becomes
`[REDACTED:secret]`, and the shared detectors (`colonizer-redact`) run over every string value. The brain redacts again on arrival. The mapping lives in one file,
`living_brain/record.rs`; the note in `living_brain/note.rs`.

## The queue

`<config>/living-brain-queue.json` (0600, written atomically) keeps one entry per colony — pending,
sent or failed, the attempts, the last error, the fingerprint and hashes last sent, the source id
and the note's URL — and never a record, a note or the token, so a restart loses nothing. A pass
sends at most 20 colonies, two seconds apart. A record and note whose hashes match the last ones
sent cost no request. A transport error, a 429 or a 5xx backs off from 30 seconds, doubling to an
hour, and fails after eight tries; a 400, 403, 404, 409, 413 or 422 fails at once. A 401 stops the
queue, marks the connection `unauthorized` and leaves everything pending for the reconnect. Sending
never blocks a colony: it runs in its own task from what the mothership already stored.

## The MCP server in colonies

When a colony's rule has `mcp` on and its agent module is `claude-code` or `codex`, boot hands msb
the token as a secret for the MCP host alone (`--secret COLONIZER_LIVINGBRAIN_TOKEN@mcp.livingbrain.wiki`,
the path every colony secret takes, see [Saved secrets](secrets.md)) and sets
`COLONIZER_LIVINGBRAIN_MCP_URL` in the runner's environment. The guest only ever holds msb's
placeholder: the runner registers the server — Claude Code as a remote `http` MCP server whose
`Authorization` header carries the placeholder, Codex with `mcp_servers.livingbrain.url` and
`bearer_token_env_var` — and msb swaps the value in on TLS to that host. Sent anywhere else, the
placeholder is blocked by msb's secret scanner. Claude Code's own environment, and so every Bash
call, never sees the variable. The name sits under the `COLONIZER_` prefix that colony secrets may
not use, so no operator secret can shadow it. Another agent module, a disconnected brain or a
refused token boots the colony without the server and says why in the colony log.

## Routes

All owner-only.

| Route | Does |
| :--- | :--- |
| `GET /api/living-brain` | `{state: "disconnected"\|"connected"\|"unauthorized", server_url, mcp_url, connected_at, token_prefix, token_name, auth_error, device, rules, mcp_agents, queue}`. `device` is the sign-in in flight: `{state: "pending"\|"connected"\|"denied"\|"expired"\|"failed", user_code, verification_uri, verification_uri_complete, expires_at, error?}`. `queue` is `{pending, sent, failed, last_sync, last_error, items}` with the latest 50 colonies. |
| `PUT /api/living-brain` | `{server_url?, mcp_url?, rules?}`. Addresses are `https://` (plain `http://` only to this machine); changing one while connected is a 409. `rules` replaces the list; a target that is not `org` or `owner/repo` is a 400. |
| `POST /api/living-brain/connect` | Starts the device flow; the answer's `device` carries the code. 502 when the brain cannot be reached. |
| `DELETE /api/living-brain/connect` | Forgets the sign-in in flight. |
| `PUT /api/living-brain/token` | `{token}`: a personal access token; 400 when the brain refuses it. |
| `DELETE /api/living-brain/connection` | Revokes and forgets the token, empties the queue; `revoke_error` says when the brain could not be told. |
| `POST /api/living-brain/queue/retry` | Every failed colony back to pending. |
| `POST /api/living-brain/backfill` | `{org, dry_run}` → `{eligible, already_sent, to_send, queued, not_opted_in, still_running}` (`still_running`: not settled yet); 409 while disconnected. |
| `GET /api/living-brain/colonies/{id}` | `{state: "none"\|"pending"\|"sent"\|"failed", note_url, sent_at, last_error, sessions_url}`; the colony page links `note_url`. |
