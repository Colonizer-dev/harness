# Sessions: events, transcript and terminal

Part of the [Colonizer protocol](../protocol.md).

## `GET /api/sessions/{id}/transcript?format=common&limit=&cursor=`

The colony's own agent session transcript — the conversation the module that ran it kept
natively — read back and normalized to one message shape, whatever ran the colony. The agent's
transcripts directory is host-mounted over the module's `session_resume.dir` at boot, so the
harness reads it with `txcript`, one reader per format, and folds it through its canonical
`Common` model:

| agent module | harness | where its sessions live under `<session dir>/transcripts/` |
| --- | --- | --- |
| `claude-code` | `claude_code` | `<slug>/<uuid>.jsonl` (the projects root is the mount) |
| `codex` | `codex` | `sessions/**/rollout-*.jsonl` (its home is the mount) |
| `grok-build` | `grok` | `sessions/<encoded-cwd>/<id>/` (a session directory) |
| `opencode` | `opencode` | `opencode.db` (one SQLite database at the mount root) |
| `hermes` | `hermes` | `state.db` (one SQLite database at the mount root) |

Only `claude-code` and `codex` declare `session_resume` today, so only they persist a transcript; a
`grok-build`, `opencode` or `hermes` colony has nothing recorded and answers **404**. An `acp`,
`pi` or unknown module has no reader and answers **422**. The mount is colony-writable, so the
harness first copies only the files a reader needs — symlinks skipped, never followed — into a
host-private directory and reads that, and a store past the caps (8 deep, 10 000 entries, 64 MiB a
file, 256 MiB in sum) answers **413**.

An unreadable native store reads the same as one never written (**404**), because discovery treats
a store it cannot open as empty; only a store that opens but fails to parse or fold answers
**500**, and its body does not echo the failure — that is logged, never returned, so no host path
leaks.

`format` is required and the only value served is `common`; anything else (or no `format`) is
**400**, leaving room for other formats later. The answer is:

```jsonc
{
  "agent": "claude-code",              // the module that ran the colony
  "harness": "claude_code",            // the reader its transcript was folded through
  "meta": { "id": "…", "timestamp": "…", "cwd": "…", "model": "…" }, // txcript Meta
  "messages": [ { "role": "user|assistant", "content": [ /* text, thinking, tool_use, tool_result, … */ ], "timestamp": "…" } ],
  "total": 128,                         // messages in the whole transcript
  "next_cursor": "99"                   // null when the last page was reached
}
```

Messages page the way the colony list does (§4 scoped API tokens): `limit` defaults to 100 and is
clamped to at most 500, and `cursor` is the index of the last message already delivered, so the
next page starts right after it and `next_cursor` is the page's last index while more follow, or
`null` at the end. A `cursor` that names no message — not an integer, or past the last one — is a
**400**. The route takes the same visibility guard as the diff and files routes, so a scoped token
outside its org/repo limits reads an unknown colony (**404**).

## `GET /api/sessions/{id}/events?since=<seq>&epoch=<epoch>` (WebSocket)

A scoped token needs `read` on the colony to watch; its commands need `operate` or `launch`, and
`set_model` is the owner's alone (a token's is refused with a `warn` line).

Server → client:

- On connect, first: `{"type":"run_epoch","epoch":N}` — the run epoch this connection is attached
  to, with no `seq` field (old clients ignore the unknown frame). Then
  `{"type":"session","session":Session}`, then the last ≤200 harness logs as
  `{"type":"harness_log","level":"info|warn|error","message":"…","ts":"…","origin":"…"}`, then agent events with
  `seq` above the effective cursor (same objects as §3, including `seq`/`ts`), then
  `{"type":"replay_done","seq":N}` (N = the highest replayed `seq`, or the cursor when nothing was
  replayed; like `run_epoch` it is a control frame, not an event), then live. A client may hold its
  render until `replay_done` so a long history appears at once, on its latest messages.
- Each resume rotates the event log aside (`events.jsonl` → `events-N.jsonl`) and bumps the epoch,
  and the new run's `seq` numbering starts from 1 again. The effective cursor is `0` when the
  client's `epoch` names a retired run — its `since` is a rank in that run's numbering, meaningless
  in the new run — and `since` when `epoch` is absent (legacy clients), `0` ("unknown"), or current,
  so a tab left open across a resume replays the new run from the start instead of dropping its
  first events.
- Whenever the session changes: `{"type":"session","session":Session}`.
- When the colony resumes, pre-existing sockets are closed so they reconnect into the new epoch. A
  socket that falls behind is closed too, so the client reconnects with its `since`. A line of
  `events.jsonl` that does not parse is skipped with a `warn`.

Client → server:

```jsonc
{"type":"user_message","text":"…"}                        // harness assigns the id
{"type":"answer","question_id":"…","answers":{…},"response":null}
{"type":"interrupt"}
{"type":"set_model","model":"claude-sonnet-5"}              // trimmed; 1–153 of A–Z a–z 0–9 . _ : - / [ ]
```

A `user_message` is trimmed and dropped when empty or over 100,000 bytes. A command to a colony that is
not live is dropped, and the socket gets a fresh `session` frame instead. The harness drops a
`set_model` whose trimmed model is empty, too long or has any other character.
It checks the shape only: whether the model exists is known only inside the colony, so a refused
switch surfaces as the runner's `warn` log and no `model_changed`.

## `GET /api/sessions/{id}/terminal?cols=<n>&rows=<n>` (WebSocket)

Byte-for-byte proxy of agentd `/v1/pty` (same binary/text frame rules). Owner only. `cols` defaults to
80 (10–500) and `rows` to 24 (5–300). A colony that is not live yet or any more, or whose agentd cannot
be reached, gets `{"type":"error","message":"…"}` and the socket closes; **404** for an unknown
colony.

---
