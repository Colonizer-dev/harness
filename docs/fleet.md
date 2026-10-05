# Fleets: motherships that join each other

A fleet is a small set of Colonizer motherships — one **owner** and one or more **members** — that
share one fleet view. Each member keeps its own colonies, settings and secrets; what it gains is a
row in every other member's and the owner's `GET /api/hosts`, and a place on the owner's member
list. Membership ends from either side at any time.

Joining works like phone pairing: the owner mints a **single-use invite code** that lives 15
minutes, the joining machine's operator types the owner's private URL and the code into their own
cockpit, both screens then show the same six-digit **confirmation code** (`123 456`), and the join
completes only when the owner approves what they see and the joiner confirms what they see. No
account, no relay, no shared secret beyond the invite itself.

This page is the overview. The routes are documented in
[protocol.md](protocol.md#fleet-pairing-issue-686); the pane is Settings → Fleet
([cockpit.md](cockpit.md#fleet)).

## What this is not (yet)

Joining a fleet today means pairing and the fleet view. It does **not** enroll the joining machine
into the owner's embedded headscale mesh, and it does not let colonies migrate between machines.
Placement ([below](#placement)) is decided and shown, but a colony still runs on the member that
launched it. Cross-machine enrollment is a follow-up tied to the outposts design gate,
[#298](https://github.com/Colonizer-dev/harness/issues/298) — see
[outposts.md](outposts.md) for the control/execution seam it would slot into.

## Placement

Where a colony would run is decided at launch, and why is recorded on the colony as `placement`
(shown under the status on its cockpit card). A member can take a colony when it is online, can boot
the colony microVM image — Linux with a working `/dev/kvm`, macOS on Apple Silicon — and has a free
slot. A Linux member publishes its `/dev/kvm` verdict as `host.kvm_ok` in the reduced `/api/status`
allowlist, and `GET /api/hosts` rows carry it as `kvm` (omitted where there is nothing to check).

**Unpinned**, the launching member wins when it is eligible — the worktree and the publish stay
where the colony runs — and otherwise placement names, in the reason, the reachable peer with the
most free slots. Nothing launches on another member yet, so the colony still runs or queues here,
and the reason says so.

**Pinned**, `POST /api/sessions` takes a `host` — a member's id or name — and never falls back.
Pinning to this member is today's behaviour; a pin to another member is a **409** that names why
that host cannot take the colony (it is unreachable, full, or cannot boot the image), or says
cross-member launch is not built yet ([#298](https://github.com/Colonizer-dev/harness/issues/298));
an unknown host is a **400**.

**Claims are fleet-wide.** The GitHub claim marker is the same on every member, so two members never
run the same issue: launching an issue another host holds is refused, naming the holder, and a member
never releases another member's claim. When that holder is a member the fleet currently reads as
unreachable, the **409** says so — its colony is not re-run elsewhere — and the way past is to
remove the member or pass `allow_duplicate`.

## Roles

| | Owner | Member |
| :--- | :--- | :--- |
| Creates invites and approves or rejects pending requests | yes | — |
| Removes a member | yes | — |
| Hosts colonies | yes | yes |
| Sees the fleet view (`GET /api/hosts`) | yes, plus members | yes, plus the owner |
| Leaves | — | yes |

An owner with no members can itself join another fleet, and a member that leaves can later become
an owner. One mothership is in at most one fleet.

## Pairing, step by step

**The owner (the machine being joined):**

1. Settings → Fleet → **Create invite**. The pane shows the code once — the mothership keeps only
   its SHA-256 — together with this cockpit's URL. Both travel to the joining machine's operator
   over whatever channel already carries your credentials.
2. When the joiner has redeemed the code, a **pending request** appears with its own six-digit
   code. Compare the two screens: **Approve only if the joining machine shows the same code**, and
   otherwise **Reject**.
3. The new member appears in the Members list. **Remove** ends that membership.

**The joiner (the machine joining):**

1. Settings → Fleet, enter the owner's URL and the invite code; optionally a name and this
   machine's URL, which is what the owner's member list and fleet polls will use.
2. Both screens now show a six-digit confirmation code. Read it to the owner.
3. When the owner says the codes match and approves, press **Codes match**. Until then it answers
   `pending` — wait, and try again. **Cancel** abandons the join at any point.

## The confirmation code

Both sides compute the code independently, from things each side already holds:

```
first 4 bytes of SHA-256("colonizer-fleet-pair\0" || normalized invite code || "\0" || joiner nonce) as u32, mod 1,000,000
```

The owner knows the invite code and learns the nonce at the redeem; the joiner generated both. The
six digits are a *secure out-of-band compare*, the same trick phone pairing uses: they are far too
short to authenticate anything by themselves, and are not meant to. Their job is to let two people
looking at two screens notice a machine in the middle. If a stolen invite was redeemed by someone
else before the real joiner used it, the theft consumes the code — the single-use rule — so the
real joiner's screen shows an error, and the owner sees a pending request with a code nobody
vouches for. Either way nothing pairs quietly.

## What a member may do: the `fleet` scope

Approving a request mints the member a **fleet-scoped API token** — the new `fleet` scope, the
lowest there is. It is admitted on exactly these routes:

| Route | Why |
| :--- | :--- |
| `GET /api/hosts` | the fleet view, so every member can see every other |
| `POST /api/fleet/peer/leave` | leaving without holding anything broader |
| `PUT /api/fleet/peer/payloads/{sha256}` · `POST /api/fleet/peer/rows` | the history push ([below](#history-push)), into the member's own directory on the owner |
| `GET /api/fleet/policy` | reading the owner's fleet network policy ([below](#network-policy)), to apply its floor |
| `GET` · `POST` · `PUT` · `PATCH` · `DELETE /api/previews/{id}/…` | the owner's dev-server previews ([below](#dev-server-previews)) of running colonies |

Everything else answers 403, the same as any other scoped token out of scope. The token is handed
over exactly once at approval, stored hashed on the member, and never shown again on either side —
and it is never the member's local cockpit token, which stays at home. Revoking it is what
remove/leave does; the fleet scope cannot be minted by hand, and Settings → API tokens does not
offer it ([cli.md](cli.md#scoped-api-tokens)).

## Leaving and removing

Either side ends the membership — the owner with **Remove**, the member with **Leave fleet**. The
member's fleet token is revoked, the mesh plumbing described below is updated, and the member keeps
every local colony, secret and setting. A removal also leaves a **tombstone** on the owner — the
removed member's id and its token's SHA-256, the same hash the token registry kept (the most
recent 256) — so the removed machine's next call answers **403** `removed from the fleet` instead
of the **401** an unknown or invalid token gets, and the member can tell it was removed. A member
that leaves by itself leaves no tombstone. Neither can refresh its token: the owner answers a
removed or departed member's refresh with **403**. Leaving is behind a confirmation, since it costs the fleet
view and takes a new invite to undo.

## History push

A member pushes its history to the owner on its own: every finished colony (pull request opened,
merged or closed, no changes, stopped, failed) travels as one **row** — the same allowlist
projection of the colony record the export bundle carries, keyed `<host id>:<session id>` — and
its log ledgers (`events.jsonl`, `harness.jsonl`, `gateway.jsonl`) travel as **payloads**, keyed
by their SHA-256. Running colonies wait until they finish. The owner keeps each member's history
under `<data_dir>/fleet-ingest/<member_id>/`: `sessions.json` (rows by id) and `payloads/`. The
routes are in [protocol.md](protocol.md#fleet-history-push-issue-762).

**Joining is not consent.** Every membership starts with history sync **off**, and nothing is
sent until the member's operator turns it on after seeing what would go: Settings → Fleet shows
the counts beside the switch, `colonizer fleet sync --preview` (`GET /api/fleet/sync/preview`)
prints them — finished colonies, log files, bytes in all and not yet sent, and what is never sent —
and `colonizer fleet sync --enable` (`POST /api/fleet/sync/consent {"enabled": true}`) prints the
same preview, then turns it on. `--disable` withdraws it. Until then the status reads
`consent_required`, the background task sends nothing, and the manual trigger refuses with a
**409** that says how to consent. Consent belongs to the membership: leaving and re-joining — even
the same owner — starts it off again.

**Where it runs.** With consent given, a background task on the member drains shortly after
startup, right after consent is given, and every five minutes after that; an owner or a machine
alone pushes nothing. `COLONIZER_FLEET_SYNC=off` stops the background drain. `colonizer fleet sync`
(or `POST /api/fleet/sync`) drains now, and `colonizer fleet sync --status` (`GET /api/fleet/sync`)
shows where it stands.

**How it drains.**

- **Payloads first.** Every payload a row references is uploaded and acknowledged before the row
  is sent; the owner also refuses, by name, a row whose payloads it does not hold. A log larger
  than 32 MiB is named on its row as omitted and not sent.
- **Acknowledged means sent.** The drain state lives in the member's `<data_dir>/fleet-sync.json`:
  each row's fingerprint (its record, and each log's size and modification time) as the owner
  acknowledged it, written after every batch. A row that changes — a pull request merged later —
  is sent again; an unchanged one never is.
- **Resumable at any point.** A drain killed mid-batch re-sends only rows the owner never
  acknowledged, and the owner upserts by row id, so a re-send replaces rather than duplicates.
- **Capped batches.** At most 100 rows and 1 MiB of body per batch.
- **A bad row is isolated, never blocking.** When the owner refuses a batch without naming a row,
  the member splits it in half and retries each half until the row that causes it stands alone.
  A refused row is retried on later drains; after three refusals it is **retired** — listed in
  the status with the owner's reason, and left out until the row itself changes.

**Failures are states.** The status (`consent_required`, `idle`, `synced`, `backoff`,
`unauthorized`, `removed`, `error`) is recorded with a reason, for the member-health view to show:

| The owner answers | The member |
| :--- | :--- |
| **401** | refreshes the fleet token once per drain with the refresh credential the pairing handed over, keeps the new token and retries. A second 401, or a refresh the owner refuses, stops and flags `unauthorized` for attention: a person checks the owner or joins again. A membership from before refresh has no credential and stops at the first 401 |
| **403** | stops syncing: `removed` — the owner removed this machine (below). It never refreshes, and the owner refuses a removed machine's refresh anyway. Nothing local is deleted |
| **429** / **503** | waits out `Retry-After` — inline up to a minute, otherwise `backoff` until then |
| anything else, or no answer | `error`; the next tick tries again |

Background drains stay stopped after a 401 or a 403; a manual `fleet sync` tries again. Leaving
and re-joining starts the drain state over, since a new membership is a new owner's view.

### Reading it on the owner

Settings → Fleet on the owner has a **Fleet history** section: every member's synced colonies,
newest finish first, each marked "finished on <member>". Filters narrow it by member, repository,
status and finish date, and the totals at the top — colonies, merged, and cost where the rows
carry one — are counted per member and per repository over whatever the filters leave. Picking a
colony opens its record (repository, issue, branch, pull request, cost, summary, error) and its
logs, each read from the payload the member sent. The cockpit's fleet panel lists hosts rather than
colonies, and the Overview's [fleet colony list](#the-fleet-colony-list) shows only the newest of
what is here, so the full history lives on this screen. The routes are `GET /api/fleet/history`,
`GET /api/fleet/history/{member}/{row_id}` and `…/logs/{name}`
([protocol.md](protocol.md#fleet-history-on-the-owner-issue-762)); they are the owner's alone — a
scoped token, a member's `fleet` token included, reads **403**.

- **Removed members.** Removing a member keeps what it synced. Its colonies stay listed, marked
  "(removed)", under the name it had: the owner writes it to `member.json` beside the rows. When
  the last member is removed the section still shows while any history remains.
- **Redaction.** Logs are served exactly as the member sent them. Redacting secrets before they
  leave is the member's job ([#761](https://github.com/Colonizer-dev/harness/issues/761)); the
  owner runs no redaction pass of its own on this history.
- **Retention.** The owner keeps a synced row for `COLONIZER_FLEET_INGEST_RETENTION_DAYS` days after
  it arrives (default `90`; `0` keeps everything). The reclaim tick, every five minutes, drops older
  rows and then every payload no remaining row references that is itself older than the window —
  so a log uploaded just before its row is never taken. A member directory left empty is removed.
  The member keeps its own copy either way, and an unchanged row is not sent again.

## The fleet colony list

The Overview's **Fleet colonies** panel — directly below the [fleet panel](cockpit.md#fleet) —
gathers every colony the fleet knows about into one table (issue #689): this host's own colonies,
live from the session stream, and the finished colonies each member pushed to the owner. Each row
names the member it ran on (falling back to the origin host id), the colony (`repo#issue` and title),
its status, what it is **waiting on**, its cost, and a link that opens the colony on its host.

"Waiting on" is one word: `answer` (a question is out to a person), `slot` (queued for a parallelism
slot, or behind another colony's issue), `quota` (parked for tokens — a provider's plan is out, or
the autopilot hold timed out), `ci` (its pull request's checks are pending or failing) or `review`
(its pull request is open). A working or done colony waits on nothing and shows an em dash; an
imported row carries no check state, so an imported `pr_opened` colony always reads `review`.

Filters narrow by host, org and repository; the totals above add cost up per repository, per host
(member) and per day. A member that has pushed nothing still appears in the per-host totals — zero
colonies and an em dash cost, never a made-up `$0.00`.

The imported half is the newest of the history Settings → Fleet history pages, at most five pages of
a hundred rows each; the rest lives on that screen. A member pushes only **finished** colonies
([History push](#history-push)), so an in-flight colony on another member is not listed — this host
live, every member only once done. The link is the cockpit's own `?colony=<id>` here and the member's
own cockpit URL for an imported row; fleet members are not on the owner's colony mesh
([What this is not (yet)](#what-this-is-not-yet)).

## Member health

Settings → Fleet shows each member with one badge: **OK**, a grey **Not checked yet** until the
owner has polled it, or a degraded (amber) or stopped (red)
reason such as "No heartbeat for 12 min", with the one thing to do underneath ("the machine may be
asleep"). The owner works it out from what it already sees — the member's answers to the fleet
poll, its disk, whether its token still exists — and the worst problem wins. The rule, the
thresholds and which signals are wired are in [protocol.md](protocol.md#member-health-issue-764).

Each member's answer to that poll also says how its [history push](#history-push) and its colony
runner are doing, so the badge covers them too:

- A member whose last sync drew a **401** or a **403** (removed) reads **stopped**, "Token revoked",
  with "re-pair this machine" underneath.
- A member whose drain has ended with rows still unsent for an hour or more reads **degraded**,
  "Sync behind by N rows". The drain runs every five minutes, so that is a dozen drains in a row.
- A member whose queue loop has not ticked for five minutes or more (it ticks every five seconds)
  reads **degraded**, "Colony runner not ticking", with "restart colonizer on this machine".
- A member whose operator has not turned history sync on is **not** degraded: that is a choice,
  not a fault. It shows a grey "History sync off" note under the badge instead.

A member running an older colonizer reports neither, and those signals stay unmeasured.

## Network policy

The owner sets one **fleet network policy** — a shared egress floor and who may reach whom — with
`PUT /api/fleet/policy` (owner-only; a member answers **409**, since the floor is the owner's word),
and it lives as `<config_dir>/fleet-policy.json`. `GET /api/fleet/policy` reads it back, for the
owner and for a fleet token. Its shape:

- `egress` — a fleet-wide floor, `{mode, allow, block}`, the same shape an org's egress overrides
  take ([sandbox-network.md](sandbox-network.md#egress-policy-303)).
- `orgs` and `repos` — the same overrides per GitHub org and per `owner/name` repository.
- `reach` — a map from a member id (`mem_…`, as `GET /api/fleet` lists members, or `"owner"`) to the
  ids it may reach. A key absent from the map may reach every member.

A member fetches the policy with its fleet token on the history-push cadence and caches it; a failed
fetch keeps the last cache, so staleness can only ever keep a fence up, never take one down, and
leaving the fleet lifts the cached policy. At boot the floor for the colony's org and repository —
the fleet, org and repo levels unioned, the mode from the most specific level that names one —
**clamps** the colony's own resolved egress policy: a member may *tighten* the floor (an allowlist
under an open floor, extra blocks, a narrower allowlist, an empty allowlist) but never *loosen* it.
An open local mode under an allowlist floor, allow entries the floor's allowlist does not cover, and
a local list that drops a floor block are refused; each refusal is logged and listed in the colony's
egress record (`GET /api/sessions/{id}/egress`, field `fleet_refused`). The owner's own colonies are
clamped too, and `ALWAYS_BLOCKED` and the harness's infrastructure allows are unchanged. A policy
naming only `orgs` and `repos` leaves other orgs unfenced; a fleet-wide `egress` covers everything.

## Dev-server previews

The owner points a browser at a port inside a running colony and reaches its dev server through the
mothership. `POST /api/sessions/{id}/preview` `{"port": 5173}` opens one — owner-only, the colony
must be running with a microVM, and the port must not be agentd's own `7070` — and
`DELETE /api/sessions/{id}/preview` closes it. The preview is then served at `/api/previews/{id}/…`,
on `GET`, `POST`, `PUT`, `PATCH` and `DELETE`: the mothership reverse-proxies plain HTTP to the guest
port over the mesh. A request needs the owner's credentials or a fleet token — **401** without one —
and a fleet caller must be allowed to reach `owner` by the policy's `reach` map, else **403**. Every
credential the caller presented (`Authorization`, `Cookie`, any `x-colonizer-*` header) is stripped
before the request is forwarded, so the token never reaches the colony. The preview is **404** the
moment the colony stops or reboots, because the port lives only as long as its microVM, and the
cockpit's colony row shows a **preview** link while the colony is running.

Previews are **mesh-only**: an install running its colonies with the mesh module off publishes only
agentd's own port, so the proxy answers **409** rather than pretending. Three limitations:

- **No WebSocket or HMR upgrade.** The proxy carries plain HTTP/1.1 request/response pairs and drops
  an `Upgrade` header; a response is buffered whole, under a size cap, so a preview is a page, not a
  download.
- **Base paths.** A dev server that serves absolute asset paths must be told where it now lives: set
  its base to `/api/previews/<id>/` (Vite: `--base /api/previews/<id>/`).
- **Owner-only in practice.** A fleet token authenticates only on the owner, so the previews a member
  reaches are those of colonies running *on the owner*; member-to-member previews wait for member
  mesh enrollment ([#298](https://github.com/Colonizer-dev/harness/issues/298)) — see
  [The mesh ACL](#the-mesh-acl-ready-but-nothing-can-use-it-yet) below.

## The mesh ACL: ready, but nothing can use it yet

The fleet rides the same embedded headscale the colonies use
([architecture.md](architecture.md#mesh-design)), and the owner keeps its ACL ready for the day
members can join it. The owner's headscale carries a `fleet` user, and the policy admits that user
to `harness@` only while the fleet has members: the rule appears with the first member, disappears
with the last, and is restored at startup, with headscale reloaded by `SIGHUP` when it changes.
Removing a member also deletes that member's mesh node — the one named `fleet-<member-id>` — best
effort.

None of this admits anything today, because pairing does not enroll a member's machine into the
owner's mesh ([What this is not (yet)](#what-this-is-not-yet)): no member node ever presents itself
as the `fleet` user. The plumbing takes effect when cross-machine enrollment lands — the follow-up
tied to the outposts design gate, [#298](https://github.com/Colonizer-dev/harness/issues/298).
Cross-member launch ([Placement](#placement)) waits on the same gate: a colony may name a peer the
fleet has room on, but it still runs on the member that launched it.

## The read-only fallback

`COLONIZER_FLEET_PEERS` — the comma-separated list of peer base URLs from
[#231](https://github.com/Colonizer-dev/harness/issues/231) — keeps working exactly as before, as a
poll-only, read-only mode for machines that never pair. Pairing extends `GET /api/hosts` rather
than replacing it: a member also polls its fleet's owner, and an owner also polls the members that
gave a URL, so the fleet view fills in without the operator maintaining a peer list by hand.

## Security notes

- **Single use.** An invite is consumed at the first redeem, whoever redeems it. A second redeem
  answers the same 404 an invalid or expired code gets, so a guessed-or-stolen code buys nothing
  and reveals nothing.
- **Fifteen minutes.** Invites expire quickly; a code pasted hours later fails.
- **Hashed at rest.** The mothership stores only the SHA-256 of an invite code, like API tokens;
  the plaintext exists on the owner's screen once.
- **One 404.** Invalid, expired and already-used codes are indistinguishable from outside, so the
  joiner's failed attempt says nothing about which of the three held.
- **Both screens, or nothing.** Approval is a human comparing two codes; the protocol has no
  auto-approve. A member is admitted only after the owner approved *and* the joiner confirmed.
- **Private transport.** The owner URL the joiner types is any URL already reachable on your
  private network — a tailnet, a LAN, a VPN — the same transport `COLONIZER_FLEET_PEERS` uses.
  Nothing listens publicly for this: pairing rides the cockpit's existing authenticated API, and
  the two unauthenticated peer routes (redeem, pairing poll) take an invite code or a one-time
  nonce as their only credential. `COLONIZER_BIND` stays loopback/private throughout.
