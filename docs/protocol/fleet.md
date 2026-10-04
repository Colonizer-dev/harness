# Fleet: hosts, pairing, history and member health

Part of the [Colonizer protocol](../protocol.md).

## `GET /api/hosts`

Fleet visibility (issue #231): this host's own numbers, `self` first, plus one row per configured
peer, each obtained by this host polling that peer's own `GET /api/status` (never the other way
round — no peer reaches in). `{"hosts": [HostSummary, ...]}`:

```json
{
  "hosts": [
    {
      "id": "5e1347a6-5f2e-4b8b-9c1a-0d4b7c8e9f10",
      "name": "picard",
      "platform": "linux-x86_64",
      "os": "Debian",
      "version": "0.1.5",
      "slots_in_use": 3,
      "slots_ceiling": 4,
      "queue_depth": 1,
      "disk_free_bytes": 124592496640,
      "last_heartbeat": "2026-09-21T04:00:00+00:00",
      "health": "online"
    },
    {
      "id": "http://100.127.251.53:7878",
      "name": "http://100.127.251.53:7878",
      "platform": "",
      "os": "",
      "version": null,
      "slots_in_use": 0,
      "slots_ceiling": 0,
      "queue_depth": 0,
      "disk_free_bytes": null,
      "last_heartbeat": null,
      "health": "unreachable"
    }
  ]
}
```

The local row's `id` is `host.id` (the stable, per-install UUID `GET /api/status` documents above) and
its `name` the hostname (the id when there is none). A peer row's `id` and `name` are always the
peer's configured base URL: peers are polled without a token, so they answer the reduced status,
which names no host id or hostname. `platform`, `os`, `version`, `slots_in_use`,
`slots_ceiling`, `queue_depth` and `disk_free_bytes` are read straight out of that peer's own
`/api/status` (`runtime.platform`, `runtime.os.name`, `version`, `host.microvms_live`,
`host.microvms_ceiling`, `queue_depth`, `host.disk_free_bytes`); a reachable peer that reports no
platform or OS reads `"unknown"`, and a peer never reached has zeros, nulls and empty strings instead. `last_heartbeat` is an RFC 3339 timestamp for when this host last confirmed the
peer was up — `null` only for a peer that has never once answered.

`health` is `"online"` (the poll just succeeded, or this is the local host) or `"unreachable"` (the
poll failed — refused, timed out after 3 s, or answered something that was not `/api/status`'s
shape). An unreachable peer that *has* answered before keeps showing its last-known
`slots_in_use`/`disk_free_bytes`/etc. instead of being nulled out, so a stalled host still reads as
"last seen doing X" rather than going blank. This is poll-on-request, not a background loop: nothing
is cached to disk, and a peer's row is only ever as fresh as the last time `GET /api/hosts` was
called.

Peers are configured with `COLONIZER_FLEET_PEERS`, a comma-separated list of base URLs (e.g.
`http://100.127.251.53:7878,http://10.0.0.5:7878`) — the same one-item-per-comma parsing
`COLONIZER_ALLOWED_HOSTS` uses. Fleet membership adds its rows the same way, no list to maintain:
a member also polls its fleet's owner, and an owner also polls the members that gave a URL
([fleet.md](../fleet.md)). This route is also the one a `fleet`-scoped token may call, so a member
reads the fleet view with the token its pairing minted. No port is opened by this change on any host: this mothership only
ever dials **out** to the URLs it is given, over whatever private network the operator already runs
(their own tailnet, mesh, or VPN — never the public internet). `COLONIZER_BIND` stays loopback-only
by default everywhere, exactly as before; an operator who wants a given host to answer these polls
sets *that host's own* `COLONIZER_BIND` to a private interface IP of their choosing — never
`0.0.0.0` — the same opt-in a Settings operator has always had to make to reach the API from another
machine at all. Peer polls carry no token, so a peer answers the reduced `GET /api/status`
(version, queue depth, microVM counts, numeric host capacity, platform/OS, storage verdict — no
hostnames, host ids, repos, or account identities); the row keys on the configured URL.

## Fleet pairing (issue #686)

One mothership joins another's fleet like phone pairing: a single-use invite, both screens showing
the same six-digit confirmation code, a human approving what they see. The full walkthrough, the
`fleet` trust scope, the mesh ACL and the security notes are in [fleet.md](../fleet.md); the state
lives in `<config_dir>/fleet.json` (0600). The cockpit-facing routes are owner-only (a scoped token
gets **403**); the `peer` routes are how the other machine drives its side of the pairing, and the
first two are unauthenticated — the invite code, then the joiner's nonce, are the whole credential.
The table above names each one; the shapes:

- `POST /api/fleet/peer/redeem` `{code, nonce, name, url?}` answers `{pairing_id, confirm_code}` —
  or the one **404** an invalid, expired and already-redeemed code all share, indistinguishably. The
  first redeem consumes the code.
- `POST /api/fleet/peer/pairings/{id}` `{nonce}` answers `pending` until the owner decides, then
  `{status: "approved", token, member_id}` exactly once (a second poll reads **404**), or the
  rejection. `token` is the member's `fleet`-scoped credential — the lowest scope, admitted only on
  `GET /api/hosts`, `POST /api/fleet/peer/leave` and the history push's two ingest routes (below),
  never minted by `POST /api/tokens`, and never the member's local cockpit token.
- `POST /api/fleet/peer/leave` ends the membership from the member's side (**204**): the token is
  revoked, the owner's mesh policy is updated, and the member keeps its local data. The mesh itself
  enrolls no member yet — that waits for outposts ([#298](https://github.com/Colonizer-dev/harness/issues/298)); see [fleet.md](../fleet.md).

Both screens get the same six digits because each side computes them independently: the first 4
bytes of SHA-256(`"colonizer-fleet-pair\0"` ‖ normalized invite code ‖ `"\0"` ‖ joiner nonce) taken
as u32, mod 1,000,000. The owner knows the invite code and learns the nonce at the redeem; the
joiner generated both. The digits are a compare for two people looking at two screens, not an
authentication — a stolen invite redeemed by someone else is consumed, so the real joiner sees an
error and the owner sees a request nobody vouches for.

## Fleet history push (issue #762)

A member drains its finished colonies' history to its fleet's owner on the `fleet` token its
pairing minted; the design is in [fleet.md](../fleet.md#history-push). Two owner routes take it, both
`fleet`-scoped and both answering **403** to a token that names no current member (the owner's own
cockpit token included). What arrives lands under `<data_dir>/fleet-ingest/<member_id>/` on the
owner.

- `PUT /api/fleet/peer/payloads/{sha256}` — the body is one log ledger's raw bytes (at most 32 MiB),
  keyed by the lowercase hex SHA-256 of those bytes. **204** once stored (or already held); **400**
  when the key is malformed or the body does not hash to it; **413** past the size limit. Stored
  as `payloads/<sha256>`, so a re-upload is a no-op.
- `POST /api/fleet/peer/rows` — `{"rows": [{id, record, payloads}]}`, at most 500 rows and 4 MiB.
  `record` is the same allowlist projection of a colony the export bundle's history carries
  ([§6.11](fleet-export.md#611-fleet-export-bundle-687)), and `id` is its `<origin_host>:<original_id>`.
  `payloads` lists the colony's logs — `{name, sha256, bytes}` with `name` one of `events.jsonl`,
  `harness.jsonl`, `gateway.jsonl`, or `{name, bytes, omitted: true}` for a log too large to send.
  The answer is `{"accepted": [id…], "rejected": [{id, error, missing_payloads?}]}`: accepted rows
  are upserted by id into `sessions.json` (a re-sent row replaces itself, never duplicates), and a
  row whose payloads are not all held is refused by name with the hashes it lacks. A body that
  cannot be read as rows is refused whole (**400**/**422**, or **413** past the limits) — the
  member splits such a batch to find the row that caused it. A refusal may name the row itself
  with a top-level `"row": id`.

The member reads the other answers as states: **401** stops the drain and asks for attention,
**403** stops syncing (removed from the fleet), and **429**/**503** wait out `Retry-After`
(seconds or an HTTP date).

A removed member's token keeps a tombstone on the owner (its SHA-256, kept in
`<config_dir>/fleet.json` after the token itself is revoked), so every API request presenting it
answers **403** `{"error": "removed from the fleet"}`; a token the owner never knew, or one that
left by itself, answers the usual **401**.

Nothing is pushed until the member's operator consents, per membership:
`GET /api/fleet/sync/preview` counts what would be sent (finished colonies, their logs, bytes in
all and not yet acknowledged) from the same collection the drain sends, and
`POST /api/fleet/sync/consent` `{"enabled": true|false}` records the answer on the membership in
`<config_dir>/fleet.json`. A new join starts with it off; until it is on, `GET /api/fleet/sync`
reads `status: "consent_required"` and `POST /api/fleet/sync` answers **409**.

## Fleet history on the owner (issue #762)

What members pushed, read back on the owner ([fleet.md](../fleet.md#reading-it-on-the-owner)). All
three routes are owner-only: a scoped token — a member's `fleet` token included — answers **403**.

- `GET /api/fleet/history?member=&repo=&status=&since=&until=&limit=&cursor=` — every member's
  synced colonies, newest `record.updated_at` first. `member` is a member id, `repo` the record's
  `owner/name`, `status` its status (`merged`, `pr_opened`, …); `since`/`until` bound the finish
  time, each RFC 3339 or `YYYY-MM-DD` (`until`'s day is inclusive). Pagination follows
  `GET /api/sessions`: `limit` 1–100 (default 20), `cursor` the last entry's `key`, `next_cursor`
  null at the end; a malformed filter or a cursor naming no entry is **400**. The answer:
  `{colonies: [{key, member_id, member_name, member_removed, id, received_at, record, payloads}],
  next_cursor, stats: {total, members: [{member_id, name, removed, …}], repos: [{repo, …}]},
  members: [{id, name, removed}], repos: [..], retention_days}`, where each total is
  `{colonies, merged, cost_usd}` over every filtered row (`cost_usd` null when no row carries a
  cost), and `key` is `<member_id>/<row id>`. `member_removed` marks a member the owner removed.
- `GET /api/fleet/history/{member}/{row_id}` — one entry as above plus
  `logs: [{name, sha256, bytes, omitted, stored}]`; **404** for an unknown member or row.
- `GET /api/fleet/history/{member}/{row_id}/logs/{name}` — one stored log, streamed as
  `text/plain` exactly as the member sent it (no owner-side redaction); **404** when the row names
  no such log, it was omitted, or the owner does not hold it.

Rows are pruned `COLONIZER_FLEET_INGEST_RETENTION_DAYS` (default 90, `0` = never) after
`received_at` by the reclaim tick, together with the payloads only they referenced.

## Member health (issue #764)

Each entry of `GET /api/fleet`'s `members` carries one verdict:

```json
{"id": "mem_…", "name": "worker", "url": "http://10.0.0.2:7878", "joined_at": "…",
 "health": {"state": "degraded", "code": "no_heartbeat", "reason": "No heartbeat for 12 min", "hint": "the machine may be asleep"}}
```

`state` is `ok`, `unknown`, `degraded` or `stopped`; `code`, `reason` and `hint` are `null`
exactly when it is `ok`. `note` is independent of the state: something worth knowing that is not a
fault — today only `"History sync off"`, when the member's operator has not consented to the
history push — else `null`. `unknown` means no poll has checked the member yet: it is never reported
as `ok` on no evidence. `code` is stable; `reason` and `hint` are for showing verbatim. Reading the view never dials a
member: it evaluates what the owner already holds (`crates/colonizer/src/fleet_health.rs`), and
every signal that fires is a finding. The worst state wins (`stopped` over `degraded` over `unknown` over `ok`); findings of the same state break ties in
the order below.

| Order | `code` | Fires when | State | Hint | Wired |
|---|---|---|---|---|---|
| 1 | `token_revoked` | the member's fleet token is gone from the registry | stopped | re-pair this machine | yes |
| 2 | `sync_rejected` | the member's last sync drew a 401 (`unauthorized`) or a 403 (`removed`); reason "Token revoked" | stopped | re-pair this machine | yes |
| 3 | `runner_down` | the member's queue loop last ticked ≥ 5 min ago; reason "Colony runner not ticking" | degraded | restart colonizer on this machine | yes |
| 4 | `disk_full` | disk ≥ 90% used (≥ 95% stopped); without a total, < 5 GB free (< 1 GB stopped) | degraded / stopped | clean target/ dirs | yes |
| 5 | `no_heartbeat` | the member last answered ≥ 5 min before the owner's latest poll (≥ 30 min stopped) | degraded / stopped | the machine may be asleep | yes |
| 6 | `unreachable` | the owner's latest poll went unanswered | degraded | check that it is awake and on the network | yes |
| 7 | `sync_backlog` | the member's drains have ended with rows unsent for ≥ 60 min; reason "Sync behind by N rows" | degraded | check its network, then restart colonizer there | yes |
| 8 | `sync_rate_limited` | the member's drain is backing off after a 429 or 503 | degraded | it backs off by itself; wait a few minutes | yes |
| 9 | `unwatched` | the member published no URL, so the owner cannot poll it | degraded | re-join with this machine's URL so the owner can poll it | yes |
| 10 | `not_checked` | the member has a URL but no poll has reached a verdict yet | unknown | open the cockpit or wait for the next poll | yes |

The poll signals come from the owner's `GET /api/hosts` fan-out (the cockpit polls it while open).
A heartbeat's age is measured at the latest poll, not at the read, so a member does not go stale
because nobody looked; before the first poll those signals are unmeasured, so the member reads `unknown` (any other
finding, such as a revoked token, still wins), and a member that never answered counts its age from when it joined. `GET /api/hosts` rows gained
`disk_total_bytes` (omitted when unknown) for the disk percentage.

The sync and runner signals ride the same poll. A member's reduced `GET /api/status` (the body a
caller without the API token gets) carries two more keys, both ages, counts and classes only:

```json
{"runner": {"last_tick_age_s": 3},
 "fleet_sync": {"state": "error", "backlog_rows": 5, "oldest_unsent_age_s": 5400,
                "last_error_class": "error", "consent": true}}
```

- `runner.last_tick_age_s` — seconds since the queue loop last came round (`null` before its first
  tick). The loop ticks every 5 s and stamps the time before it starts queued colonies, so a tick
  that wedges leaves the stamp to age.
- `fleet_sync` — present only on a fleet member, read from its drain state
  (`<data_dir>/fleet-sync.json`) without collecting or hashing anything. `state` is the drain
  status of [the history push](#fleet-history-push-issue-762) (`consent_required` while the
  operator has not said yes). `backlog_rows` is how many rows the last drain left unsent, and
  `oldest_unsent_age_s` how long every drain since has ended with a backlog — a lower bound on the
  oldest row's wait, `null` with no backlog. `last_error_class` is `unauthorized` (401),
  `forbidden` (403), `rate_limited` (429/503, backing off), `error` (anything else) or `null`.
  `consent` is the operator's answer; with it off, no backlog is claimed.

`GET /api/hosts` rows carry them on as `runner_tick_age_s` and `fleet_sync`, each omitted when the
peer did not report it. A peer on an older colonizer reports neither, so its sync and runner
signals stay unmeasured and never fire. An `error` class does not fire on its own; a backlog that
grows from it does.
