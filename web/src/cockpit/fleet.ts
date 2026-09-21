// The fleet panel reads GET /api/hosts (issue #231): self plus every peer configured via
// COLONIZER_FLEET_PEERS, polled live on each request. A peer never drops out of the list once
// configured — it either carries its last-known stats stamped `unreachable`, or (never reached at
// all) a bare placeholder with everything but id/name/health empty. `fleetHostFacts` reads either
// shape the same way `hostFacts` reads a `HostInfo`: an input the peer never measured drops its
// segment outright, so a never-reached peer's row never draws a zero it did not earn.
import { timeAgo } from "../components/ui";
import { formatBytes, type HostFact } from "./host";
import type { FleetHost } from "../types";

export type { HostFact };

/**
 * A fleet host's segments, in reading order: slots in use against the ceiling, queue depth (only
 * when work is actually queued), disk free, and the peer's build version. Every segment guards on
 * the field it reads being non-null, the same "never draw an unmeasured number" rule `hostFacts`
 * follows for the self host.
 */
export function fleetHostFacts(host: FleetHost): HostFact[] {
  const facts: HostFact[] = [
    { icon: "server", value: `${host.slots_in_use}/${host.slots_ceiling}`, title: "colonies live / the parallel limit" },
  ];
  if (host.queue_depth > 0) {
    facts.push({ value: `${host.queue_depth} queued`, title: "colonies waiting for a slot" });
  }
  if (host.disk_free_bytes != null) {
    facts.push({ value: `disk ${formatBytes(host.disk_free_bytes)} free`, title: "disk free" });
  }
  if (host.version) {
    facts.push({ value: host.version, title: "build version" });
  }
  return facts;
}

/** A fleet host's last heartbeat, relative to now — reuses the overview's own `timeAgo`, so a host
 * strip and a fleet row never phrase "how long ago" differently. Never heard from at all reads as
 * "never", not as a bogus "just now". */
export function timeSinceHeartbeat(last_heartbeat: string | null, now?: Date): string {
  if (!last_heartbeat) return "never";
  return timeAgo(last_heartbeat, now);
}
