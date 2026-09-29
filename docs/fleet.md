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
Cross-machine enrollment is a follow-up tied to the outposts design gate,
[#298](https://github.com/Colonizer-dev/harness/issues/298) — see
[outposts.md](outposts.md) for the control/execution seam it would slot into.

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
lowest there is. It is admitted on exactly four routes:

| Route | Why |
| :--- | :--- |
| `GET /api/hosts` | the fleet view, so every member can see every other |
| `POST /api/fleet/peer/leave` | leaving without holding anything broader |
| `PUT /api/fleet/peer/payloads/{sha256}` · `POST /api/fleet/peer/rows` | the history push ([below](#history-push)), into the member's own directory on the owner |

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
that leaves by itself leaves no tombstone. Leaving is behind a confirmation, since it costs the fleet
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
| **401** | stops, and flags `unauthorized` for attention: the owner does not know the token. Pairing has no token refresh, so a person checks the owner or joins again |
| **403** | stops syncing: `removed` — the owner removed this machine (below). Nothing local is deleted |
| **429** / **503** | waits out `Retry-After` — inline up to a minute, otherwise `backoff` until then |
| anything else, or no answer | `error`; the next tick tries again |

Background drains stay stopped after a 401 or a 403; a manual `fleet sync` tries again. Leaving
and re-joining starts the drain state over, since a new membership is a new owner's view.

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
