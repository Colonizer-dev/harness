# Outposts: contributed compute

First slice of [#141](https://github.com/Colonizer-dev/harness/issues/141). It draws
the control/execution seam and ships the local side of it. Remote outposts — other
machines joining the mesh and hosting colonies — are PLANNED, not present.

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
delegates with zero adaptation. Nothing calls through the trait yet; wiring the
launch path over to it is a later slice, deliberately, so this one changes no
behavior.

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
labels exist in the trait today; the scheduler does not.

## Status

- **Local backend: SHIPPING.** The only `ExecutionBackend`, delegating to the
  sandbox module. No behavior change.
- **Remote outpost: PLANNED.** No protocol, no enrollment, no scheduler. The trait
  is the seam it will plug into.

## Non-goals for this slice

- **No rating, payout, or benchmark.** Contributed compute raises the question of
  who gets paid and how well a node performed, and this slice answers none of it.
  Any future rating is gated on human merge and bound to evidence, per
  [#98](https://github.com/Colonizer-dev/harness/issues/98) — a node earns trust
  when its colonies' work actually merges, not when it says the work went well.
- **No remote execution of any kind.** No node enrollment, no worktree shipping, no
  cross-machine mesh join.
- **No caller migration.** The launch path still calls the sandbox module directly;
  moving it onto the trait is a separate, reviewable step.
- **No capability enforcement.** Labels are advertised, not verified. A node that
  claims `gpu` is believed until its colonies say otherwise.
