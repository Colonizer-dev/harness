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
| `detail` | `messages` (default): the conversation and the names of the tools called — the brain CLI's record. `full`: also each tool call's input (first and last 8 KiB) and output (first and last 2 KiB). `summary`: who, where, when, models, tokens and cost, no conversation. |
| `mcp` | Give this target's colonies the brain's MCP server. |

Switching `send` on stamps the rule's `since`: only colonies active since then are sent on their
own. Older ones are sent by **Send existing colonies** for an org, which counts first (a dry run)
and queues only when confirmed.

## What is sent

A colony is sent when a turn has ended: `idle`, `pr_opened`, `merged`, `closed`, `no_changes`,
`stopped` or `failed`. A sweep every minute — and at once whenever a notification goes out — queues
every such colony whose fingerprint (status, pull request, cost and the size of its event logs)
differs from the one last sent. Two requests per colony, both in the token owner's personal scope:

- `POST /v1/pages/sources` `{kind: "agent_log", path: "colonizer session <id>", body, scope:
  "personal", project}`. `body` is the brain's normalised session record (Living Brain's
  `docs/logs.md`, schema version 1) with `agent: "colonizer"`, the colony id as `session_id`,
  `owner/name` as `repo`, the branch, the models, the first and last event time, the turns, the
  token totals and cost from the colony's `model_usage`, and the colony's status as `outcome`. The
  first user turn is the issue and the operator's instructions, not the harness prompt; watchdog
  nudges and subagents' own lines are not turns. A `full` record says `schema_version: 2`. The
  record is serialised in declaration order from stored data only, so one colony in one state is one
  byte string and the brain's `(scope, sha256(body))` key dedupes a resend. `project` is the
  repository's name, so a repository's colonies meet the sessions its people sync with the CLI.
- `POST /v1/notes` `{slug: "agent-log-colonizer-<id>", title, body, project}`: the CLI's note — agent,
  session, repo, branch, when, models, turns, tool counts, tokens, cost, the source id and the
  conversation excerpted — plus the issue, the pull request, the status and the colony's summary.
  The slug keeps one page per colony, updated in place.

Redaction happens on the mothership before either is built: the home directory becomes `~`, every
saved secret's value becomes `[REDACTED:secret]`, and the shared detectors (`colonizer-redact`) run
over every free-text field. The brain redacts again on arrival. The mapping lives in one file,
`living_brain/record.rs`.

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
| `POST /api/living-brain/backfill` | `{org, dry_run}` → `{eligible, already_sent, to_send, queued, not_opted_in, still_running}`; 409 while disconnected. |
| `GET /api/living-brain/colonies/{id}` | `{state: "none"\|"pending"\|"sent"\|"failed", note_url, sent_at, last_error, sessions_url}`; the colony page links `note_url`. |
