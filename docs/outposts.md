# Outposts: contributed compute

First slice of [#141](https://github.com/Colonizer-dev/harness/issues/141) (closed with
this slice). It draws the control/execution seam and ships the local side of it. Remote
outposts — other machines joining the mesh and hosting colonies — are PLANNED, not
present. The design gate for them, and for a hosted Colonizer, was
[#298](https://github.com/Colonizer-dev/harness/issues/298), closed with the hosted contract
([hosted.md](hosted.md#the-outpost-seam)); the outpost protocol and enrollment it names are a
follow-up with no tracking issue yet
([#929](https://github.com/Colonizer-dev/harness/issues/929)).

Terms follow [vision.md](vision.md): the **Mothership** is the Colonizer app on your
machine, a **Colony** is one session (a microVM plus worktree plus agent), and the
**Mesh** is the private network linking every colony to the mothership.

## Control plane vs execution plane

The control plane decides; the execution plane runs. Only the execution plane ever
touches a hypervisor.

| Control plane (stays on the mothership) | Execution plane (runs on a node) |
| --- | --- |
| Queue: which tasks launch, in what order | `boot` / `remove`: start and stop colony microVMs |
| Credentials: GitHub token, provider keys, gateway tokens | `running`: which colonies are up |
| Git: worktrees, branches, the publish step | `pull`: fetch colony images into the node cache |
| Publish: untrusted colony output becomes a pull request | agentd: events, PTY, and shutdown inside each colony |
| Mesh control: headscale, which nodes may join | The node's own tailscaled mesh endpoint |

The seam between them is the `ExecutionBackend` trait
(`crates/colonizer/src/execution.rs`). Its methods mirror the sandbox module's
`boot`, `remove`, `running`, and `pull` signatures with `msb` folded into the backend
(plus node_id/capabilities added for placement), so the local backend
delegates with zero adaptation. The colony launch path goes through it: boot
(`crates/colonizer/src/boot.rs`) pulls the image and boots the microVM, teardown
(`crates/colonizer/src/lifecycle.rs`) removes it, and the liveness checks ask it
what is running — all against the local backend, with no behavior change. A
remote outpost is a second backend behind the same trait.

## Trust

The trust boundaries in [architecture.md](architecture.md#trust-boundaries) do not
move: secrets stay on the mothership, colony output stays untrusted until publish
sanitizes it and a human reviews the pull request. An outpost extends the
mothership's reach, so it inherits the mothership's obligations:

- **Public and open-source repositories only, at first.** Private repositories stay
  on machines their owners run. The mothership never ships a private worktree or a
  credential to a node it does not fully trust, and in this slice it ships nothing
  anywhere: the only backend is local.
- **The deal, stated plainly:** a node operator can read the worktree of any colony
  running on their machine — the code, the diff, the agent's output. Contributing
  compute means accepting that the operator sees the work. There is no design where
  a colony is secret from its own host.
- **No secrets cross the seam, ever.** When remote outposts arrive, the node gets
  the worktree and a per-colony mesh credential, exactly what a local colony gets
  today. GitHub tokens, provider keys, and gateway signing keys never leave the
  mothership.

## Node identity and auth (PLANNED)

Each outpost joins the mesh under its own node name and presents a bearer token on
every control call, the same shape as agentd's per-session token inside the mesh
today: random, stored 0600 on both ends, compared in constant time, revocable by
deleting it on the mothership. Enrollment is an explicit operator act — run a
command on the outpost, approve it on the mothership — never automatic discovery.
The exact handshake is unwritten; this paragraph is the whole of the sketch.

## Placement (stub)

Local-first: the mothership runs its own colonies before asking anyone else. When
there is anywhere else to ask, nodes advertise `Capabilities` (`kvm`, plus `labels`
such as `gpu`), and the scheduler matches colonies to nodes that qualify. The
labels exist in the trait today, but nothing reads them: fleet placement
(`crates/colonizer/src/placement.rs`, below) picks a member from its platform, KVM
verdict and free slots, and a colony still runs on the member that launched it.

## Status

- **Local backend: SHIPPING.** The only `ExecutionBackend`, delegating to the
  sandbox module. No behavior change.
- **Remote outpost: PLANNED.** No protocol, no enrollment, and placement only
  decides; nothing runs a colony elsewhere. The trait
  is the seam it will plug into.

## Related pieces that do exist

Four things built since this slice point the same way. None of them runs a colony on
another machine.

- **The fleet view** ([#231](https://github.com/Colonizer-dev/harness/issues/231)).
  A mothership can list other motherships beside itself: set `COLONIZER_FLEET_PEERS`
  to their base URLs, comma separated, and `GET /api/hosts` polls each one's
  `/api/status` on request (3-second timeout) and returns one row per host — slots in
  use against the parallel limit, queue depth, free disk, version, and whether it
  answered. The Overview shows these rows in a Fleet panel once there is more than one
  host. It is read-only: each host still runs only its own colonies. This host only
  dials out; for a peer to answer, its operator sets that peer's `COLONIZER_BIND` to a
  private interface, never `0.0.0.0` ([protocol.md](protocol.md#get-apihosts)).
- **Fleets** ([#686](https://github.com/Colonizer-dev/harness/issues/686)).
  Motherships pair with a single-use invite and a confirmation code shown on both
  screens, so the fleet view fills in without a hand-kept peer list, and a member can
  sync its finished colonies' history to the owner once its operator opts in. Pairing
  does not enroll a member into the owner's mesh; that waits on this design gate
  ([fleet.md](fleet.md)).
- **Fleet placement** ([#688](https://github.com/Colonizer-dev/harness/issues/688)).
  Each launch decides which fleet member could take the colony — online, able to boot the
  microVM image, a free slot — and records why on the colony as `placement`. A pin to
  another member is refused with a 409 until cross-member launch exists, so the decision is
  shown, not acted on ([fleet.md](fleet.md#placement)).
- **The session store** ([#325](https://github.com/Colonizer-dev/harness/issues/325)).
  Colony records sit behind a `SessionStore` interface so a different backend can
  hold them later. A mothership uses the local files today; an in-memory object store
  exists only to prove the interface works off the local disk
  ([session-store.md](session-store.md)).

## Non-goals for this slice

- **No rating, payout, or benchmark.** Contributed compute raises the question of
  who gets paid and how well a node performed, and this slice answers none of it.
  Any future rating is gated on human merge and bound to evidence, per
  [#98](https://github.com/Colonizer-dev/harness/issues/98) — a node earns trust
  when its colonies' work actually merges, not when it says the work went well.
- **No remote execution of any kind.** No node enrollment, no worktree shipping, no
  cross-machine mesh join.
- **No caller migration.** The launch path still called the sandbox module directly
  when this slice shipped; it moved onto the trait later
  ([#625](https://github.com/Colonizer-dev/harness/issues/625)).
- **No capability enforcement.** Labels are advertised, not verified. A node that
  claims `gpu` is believed until its colonies say otherwise.
