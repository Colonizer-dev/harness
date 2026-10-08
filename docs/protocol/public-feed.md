# The public activity feed

Part of the [Colonizer protocol](../protocol.md).

A sanitized, read-only view of what this mothership's colonies are doing, for a site that is not
the cockpit. It exists so a colony can be drawn as an animated entity somewhere else, and it is
built so that no field of it can carry a prompt, a chat, a cost, an error message, a file path or a
secret. That is a constraint on the *shape* of the response, not a promise about its content: see
[What is published](#what-is-published) for the closed field list and
[What is never published](#what-is-never-published) for the inputs it refuses to read.

## Switch it on

Off unless `colonizer.toml` says otherwise. Off, both routes answer **404** — a feature that does
not exist should not be discoverable by its status codes.

```toml
[public_feed]
enabled = true
repos = ["acme/web", "acme/api"]   # the only repositories published
# hosts = []                       # host ids this install publishes under; empty means this one
# history_limit = 200              # events one snapshot answers; clamped to 1000
```

`repos` is the opt-in, and it is the whole opt-in: an event whose repository is not on this list
is **dropped**, not published with the repository removed. "Something happened, somewhere" is still
a leak of this host's activity, and there is no useful anonymous version of it. An empty `repos`
therefore publishes nothing at all — a feed switched on without an allowlist is a working endpoint
with an empty body, not a public one.

## Make a key

The feed's keys are its own (`crates/colonizer/src/public_feed/keys.rs`), not scoped API tokens. A
scoped token is a least-privilege credential inside the cockpit's permission model; a feed key is
the opposite — a read-only credential for a third-party site that holds no cockpit token at all. The
prefix is `cfd_`, so it can never be mistaken at a glance for the install's token, a scoped `col_`
token or a phone's `cph_` cookie. These commands are local: they read and write the config dir,
with no mothership and no token.

```
colonizer feed-key create --name "the website" --ip 203.0.113.0/24 --rate 30
colonizer feed-key list
colonizer feed-key revoke cfk_1a2b3c4d
```

The plaintext is printed once, at creation, and exists nowhere else: `<config_dir>/feed-keys.json`
holds a SHA-256 of each key. `--ip` repeats and takes a bare address or a CIDR block, IPv4 or IPv6;
an entry that does not parse is refused at creation rather than silently ignored, because an
allowlist that drops an entry you believed in is a list you cannot reason about. An empty allowlist
means any address — the key is then the whole control, so creation says so. `--rate` is requests a
minute (default 60), between 1 and 600; a rate above the ceiling is refused, because the limit's
window is held in memory and an unbounded one is not a control. Revoking keeps the record, stamped
`revoked_at`, so an audit of who held a key outlives the credential.

## Ask for the feed

```
GET /api/public/feed
GET /api/public/feed/stream
Authorization: Bearer cfd_…
```

Both authenticate with the feed key and nothing else. The install's API token does not open them:
the routes are owner-only to scoped tokens, and a key is not a token. In `host_guard` these two
paths are an open door and the handler does the check itself — 401, then 403, then 429, in the order
a caller can act on.

- **401** for a missing, empty, unknown or revoked key. One answer for all four, so a caller cannot
  tell which it was.
- **403** when the request arrived from an address the key's `ip_allowlist` does not name. The
  address is read from the socket. `X-Forwarded-For` is deliberately **not** consulted: a forwarded
  header is a claim the client makes about itself, and honouring it would let any caller name an
  address inside any allowlist by adding a header. A feed behind a reverse proxy must therefore be
  reached on an address its key's allowlist names — a deployment decision to make knowingly. A
  server that was never given the peer's address (which the harness always is, via
  `into_make_service_with_connect_info`) cannot judge the list, so there the key is the whole
  control.
- **403** when the request arrived **through the remote tunnel**, on either route. The tunnel is
  not a fixed source address, so there is nothing for an allowlist to mean: a tunnelled request
  carries no peer address at all and the check above would be skipped rather than enforced. The
  feed refuses it outright with `the public feed is not available over the remote tunnel`; reach
  it on the host's own address.
- **429** once a key is over its `rate_limit_per_minute`. The window is counted in memory, so a
  restart hands every key a fresh one.

### `GET /api/public/feed`

```json
{
  "events": [ { "id": "…:41", "kind": "pr_opened", "…": "…" } ],
  "active": [ { "colony": "034941017c35", "repo": "acme/web", "issue": 895,
                "kind": "pr_opened", "status": "pr_opened", "since": "2026-10-08T12:00:00Z" } ]
}
```

`events` is the last `history_limit` events, oldest first. `active` is the colonies whose newest
event in that window says they are still going — a colony that merged, failed or stopped drops out,
which is what an external view wants. It is derived from the same `events`, so it can never
disagree with them.

### `GET /api/public/feed/stream`

The same events as Server-Sent Events, with `Last-Event-ID` resume.

```
event: pr_opened
data: {"id":"…:41","kind":"pr_opened","colony":"034941017c35","repo":"acme/web","issue":895, …}
id: …:41
```

Each event's `id` is `"{host}:{seq}"`, where `seq` is the activity log's own sequence, so a
reconnecting browser resumes exactly where it stopped with no gap and no replay. With no
`Last-Event-ID` a stream starts at the oldest retained line, so a client that connects gets the
recent past and then the live tail rather than an empty screen until the next colony happens to
move. A `Last-Event-ID` that does not parse is treated as absent — a full replay, not silence.

The stream polls the activity log once a second rather than subscribing to a broadcast channel: the
log is already the durable, already-redacted record, and a client that reconnects after an hour gets
the same answer a client that never disconnected would. A resume point the log has rotated past
cannot be honoured, so it is announced before the retained lines are replayed:

```
event: gap
data: {"requested_after":1,"oldest_retained":9}
```

That frame has no `id`. A site drawing a colony trail learns that part of it is missing instead of
drawing a broken one.

## What is published

One event carries a closed allowlist of ten fields, and a test asserts the serialized key set is
exactly those ten — adding a field later breaks the build rather than quietly widening what an
external site can read.

| field | |
|---|---|
| `id` | `"{host}:{seq}"`, stable across restarts, and what `Last-Event-ID` resumes from |
| `kind` | `started`, `tick`, `asking`, `pr_opened`, `merged`, `failed`, `stopped` |
| `colony` | a 12-hex-character pseudonym of the colony id, never the id |
| `repo` | the repository, present because it is on the allowlist |
| `issue` | the issue number, if the colony was launched on one |
| `pr_url` | the pull request, if there is one and it is an http(s) URL |
| `status` | a word: `running`, `waiting_for_answer`, `pr_opened`, `merged`, `failed`, `stopped` |
| `ts` | when the line was written |
| `host` | this install's stable host id, so a site aggregating a fleet can tell them apart |
| `intensity` | reserved; unset today (see below) |

`pr_url`, `status` and `intensity` are omitted when unset, so a reader cannot tell "absent" from
"empty". The kinds are a closed vocabulary of the feed's own: an activity kind the feed does not
know becomes a `tick`, so a new kind shows a colony working rather than vanishing, but it cannot
invent a kind of its own.

A `tick` is a heartbeat, not news, and is thinned to at most one per colony per minute. The other
kinds are never thinned. The thinning is computed by the reader rather than stored — a snapshot
reads the whole log in one call and is its own window; a stream carries one map of "when did this
colony last tick" across its polls, in memory, for the life of the connection.

`colony` is the first 6 bytes of the SHA-256 of the colony id, hex. A colony id is already 8 random
hex characters, so this is a pseudonym rather than a security boundary: it stops a casual reader
joining a feed event back to a colony record, and nothing more. It is deliberately *not* the keyed
HMAC of `observability::hashing`, which lives in the separate `colonizer-observability` add-on
binary that the harness does not depend on.

`intensity` is reserved and unset. Nothing in the activity log yields an honest tokens-per-minute
figure, and a number this public that nobody can defend is worse than no number.

## What is never published

The activity log carries more than the feed will ever say. The projector reads a line's `seq`, `ts`,
`kind`, `colony`, `repo`, `issue` and `pr_url`, and **nothing else**. Specifically never read:

- **`title`** — a colony's task, so a line still reads after the colony is deleted. This is the
  important one: `activity::Entry::colony` fills `title` from the colony's issue title *or, when
  there is none, from its summary* — the text an operator typed.
- **`detail`** — one line of context: a failure's reason, a loop a colony came from. A path, an
  error, a command.
- **`target`** and **`section`** — what the line is about when it is not a colony: a provider, a
  module, a secret's name, a loop's name.

### There is no title, under any name

The feed publishes an issue *number* and never an issue *title*, and there is deliberately no
`issue_title` field to fill in later. Both places a title could come from are operator-typed free
text:

- `activity::Entry::title`, which falls back to the colony's summary — the task.
- `Session::issue_title`, which looks like GitHub text but is not: `POST /api/sessions` copies
  whatever `title` the caller sent, a handoff falls back to a chat transcript's title, and a
  validation run builds `"Fix: {finding title}"`.

Nothing on a colony record says which of those produced the string, so a feed field called
`issue_title` would publish an operator's task under a reassuring name. `redact` strips secrets,
not tasks. The number stays because a number on an allowlisted repository is a link a site can
already build; the words do not.

`pr_url` is the only free text left in the feed, and it is validated rather than trusted: it must
parse as an `http` or `https` URL with a host, carry no whitespace or control characters (a
newline would split one event across two SSE frames), and be at most 2048 characters. Anything else
is published as absent. A site that renders `pr_url` as a link would otherwise be one `javascript:`
URL away from executing whatever it was handed.

Lines that are about the installation rather than a colony are dropped for the same reason a
non-allowlisted repository is: `chat.*` (a conversation — the one place prompt text lives),
`remote.*`, `redteam.*`, `secret.*`, `settings.*`, and any line with no colony or no repository.

Private repositories are opt-in and only ever opt-in by naming them in `repos`. There is no
"publish everything except" form, and no way to publish a repository's events with its name
removed.

## See also

[Accounts, tokens and install settings](auth.md) for the credentials this deliberately is not,
[Activity](activity.md) for the log the feed is a projection of, and
[Mothership API](mothership-api.md) for the routes an install token reaches.