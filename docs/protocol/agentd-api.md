# 3. colonizer-agentd API (VM, port 7070)

Part of the [Colonizer protocol](../protocol.md).

Every request requires `Authorization: Bearer <token>`; otherwise `401`. The token is the line the
mothership wrote to `<session>/vm/token`: agentd reads it once at boot and then seals the file
(`--seal-token`), so the guest holds no reader for it — the mothership keeps its own host-side copy.
Browsers never talk to agentd; only the harness does, over the mesh.

agentd assigns each runner event a monotonically increasing `seq` (starting at 1) and `ts` (RFC 3339
UTC), appends it to `/var/lib/colonizer/events.jsonl`, and broadcasts it. agentd's own diagnostics are
`log` events with the same numbering. If the runner exits, agentd emits
`{"type":"status","state":"exited","detail":"exit code N"}`.

## Origins

Every line the harness appends to a colony's `events.jsonl` or `harness.jsonl` — and every frame it
fans out to the browser for one of those lines — carries an envelope field `origin` naming the
subsystem that caused it. Like `seq`/`ts` it is stamped by the host at write time and is not part of
the runner contract body (§2). The vocabulary is closed:

- `user` — a person drove it: a message typed into the colony, or their answer to a question.
- `agent` — the colony's orchestrator agent: most runner lines, its questions included.
- `subagent` — a subagent inside the colony; the line also carries the `agent` ref (§2 rules).
- `watchdog` — the watchdog nudging a stalled colony (§6.3).
- `autonomy` — the autonomy judge answering a question in autonomous mode (§6.2b).
- `burn_down` — the burn-down scheduler, on a colony it launched (§6.2c).
- `redteam` — a red-team hunter colony (§6.7).
- `notify` — the notification dispatcher, about a dispatch it made or failed.
- `system` — the host itself: its validation chain, verification, lifecycle and bookkeeping.

Unlike the event types (unknown ones are ignored, as the top of this file says), this vocabulary is closed: a value outside it is a bug in the writer, not
a forward-compatibility case, and reading one logs it loudly. Lines written before the field existed
have no `origin`; readers treat absence as unknown/legacy. A subagent event keeps its `agent` ref and
adds `origin: "subagent"`. One body field shares the key: `memory_proposal`'s `origin` names the
proposer (§6.2) and predates the envelope, so those lines are not stamped and their `origin` stays
the proposer's — a value outside this vocabulary, read as the proposer and never as the stamp.

## `GET /v1/health`

```json
{"ok": true, "version": "0.1.0", "agent": {"state": "working", "running": true, "last_seq": 42}}
```

## `GET /v1/events?since=<seq>` (WebSocket)

- Server → client text frames: every stored event with `seq > since`, then live events.
- Client → server text frames: runner commands (§2). agentd forwards `user_message`, `answer`,
  `interrupt`, `set_model` to the runner's stdin unchanged. Invalid frames are ignored.
- Multiple concurrent clients are allowed.

## `GET /v1/pty?cols=<n>&rows=<n>` (WebSocket)

Starts `/bin/bash -l` (fallback `/bin/sh`) in `/workspace` with `TERM=xterm-256color` in a new PTY.

- Client → server **binary** frames: raw input bytes.
- Client → server **text** frames: `{"type":"resize","cols":120,"rows":40}`.
- Server → client **binary** frames: raw output bytes.
- On shell exit: text frame `{"type":"exit","code":0}`, then close. Closing the socket kills the shell.

## `POST /v1/shutdown`

Sends `shutdown` to the runner, waits up to 10 s, then kills it. Response `{"ok": true}`.

---
