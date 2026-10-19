# Fleet relay: members' history on the owner's backend

A fleet owner may relay what its members push to it — the [history push](../fleet.md#history-push)
each member consented to — on to the owner's own observability backend: the colony records and the
`events.jsonl`, `harness.jsonl` and `gateway.jsonl` logs that arrived under
`<data>/fleet-ingest/<member_id>/`. It is **off by default**: turn it on with the
**Relay fleet members' history** setting (`relay_fleet_history`), shared by every provider of the
`observability` module ([Settings](settings.md)); the export itself must be on and enabled too, or
nothing is relayed. Running colonies are never relayed — a member only ever pushes finished ones.

What is relayed is structure only, always, whatever the owner's own content switches say: the same
allowlist projection and redaction the member's push already applied, and the relay fails closed —
no member's sensitivity or org opt-in is consulted on the owner, because the owner does not know
them. Members' data goes to the owner's backend, so consent stays on the member's side: the flag
rides the pairing answer and every rows answer, and the member's push preview
(`GET /api/fleet/sync/preview`, `colonizer fleet sync --preview`) says
"the owner relays this history to its own observability backend under this host's identity
(structure only, never conversation content)" whenever the freshest flag says the owner relays.

Each member's records are sent under that member's identity, not the owner's: the resource's
`service.instance.id` is the member's host id and `colonizer.relayed_by` is the owner's. The ids
are the same ones the member's own exporter would have produced, so a backend that receives both a
member's direct export and the owner's relay of it deduplicates them as one stream. The
**double-send guard** works on what a member reports about its own exporter — the
`{host_id, enabled, destination_hash}` entries of the contract's `relay_members` block, which
members start sending with issue #860: the add-on skips a member whose exporter is enabled with
the same destination hash as the owner's. Until members report that, the guard cannot fire — an
owner whose backend a member already exports to should leave the relay off, or move one of them.

The relay runs in the add-on, beside the live export, periodically over what has arrived, and
keeps its position in `<data>/observability/fleet-relay.json`. The design decisions — identity,
the fail-closed tier, the redundant second copy — are in [the design doc's fleet
section](../design/observability.md#fleet-resource-attributes-and-relay).
