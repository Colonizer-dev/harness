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
lowest there is. It is admitted on exactly two routes:

| Route | Why |
| :--- | :--- |
| `GET /api/hosts` | the fleet view, so every member can see every other |
| `POST /api/fleet/peer/leave` | leaving without holding anything broader |

Everything else answers 403, the same as any other scoped token out of scope. The token is handed
over exactly once at approval, stored hashed on the member, and never shown again on either side —
and it is never the member's local cockpit token, which stays at home. Revoking it is what
remove/leave does; the fleet scope cannot be minted by hand, and Settings → API tokens does not
offer it ([cli.md](cli.md#scoped-api-tokens)).

## Leaving and removing

Either side ends the membership — the owner with **Remove**, the member with **Leave fleet**. The
member's fleet token is revoked, the mesh plumbing described below is updated, and the member keeps
every local colony, secret and setting. Leaving is behind a confirmation, since it costs the fleet
view and takes a new invite to undo.

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
