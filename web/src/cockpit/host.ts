// What the machine under every colony is doing, read from GET /api/status `host` (issue #205).
// The mothership re-probes the host on each status poll and the overview strip renders the facts
// below as given; everything here is a pure reading of that payload, so host.test.ts owns the
// shapes and the strip stays a dumb renderer.
//
// Bytes share the compact 16G / 512M shape the rest of the app writes sizes in (SessionView's
// diskSize: the 512M a sandbox setting calls "512M" must look like the 512M the strip reads), so a
// memory reading and a colony's host footprint never disagree about a unit.
import { diskSize } from "../components/SessionView";
import type { HostInfo, Session } from "../types";

/** Bytes in the app's compact G/M/K shape, the same formatter the colony host footprint uses. */
export const formatBytes = diskSize;

/** Whole seconds as `3d 4h`, `5h 12m`, `42s`. */
export function formatUptime(secs: number): string {
  const total = Math.max(0, Math.floor(secs));
  const days = Math.floor(total / 86_400);
  const hours = Math.floor((total % 86_400) / 3_600);
  const minutes = Math.floor((total % 3_600) / 60);
  const seconds = total % 60;
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${seconds}s`;
  return `${seconds}s`;
}

/** The 1-minute load average, two decimals. */
export function formatLoad(load: [number, number, number]): string {
  return load[0].toFixed(2);
}

/** A boot's wall clock, milliseconds read as whole seconds: 94_000 → `94s`. */
export function formatBootMs(totalMs: number): string {
  return `${Math.round(totalMs / 1000)}s`;
}

export interface HostFact {
  /** Which icon leads the segment; a segment without one is self-labeled text. */
  icon?: "server" | "cpu" | "memory";
  /** The segment's text, already formatted and unit-labeled. */
  value: string;
  /** Hover detail, for when the value alone undersells the number. */
  title?: string;
}

/**
 * The host strip's segments, in reading order. Every input the host did not measure drops its
 * segment outright, so the UI can never draw a zero or an "undefined" for a number that was not
 * measured. `microvms_live`/`microvms_ceiling` are always present, so the leading count always is.
 */
export function hostFacts(host: HostInfo): HostFact[] {
  const facts: HostFact[] = [
    { icon: "server", value: `${host.microvms_live}/${host.microvms_ceiling}`, title: "microVMs booted / the parallel limit" },
  ];
  if (host.cpu_cores != null || host.load != null) {
    const parts: string[] = [];
    if (host.cpu_cores != null) parts.push(`${host.cpu_cores}c`);
    if (host.load != null) parts.push(formatLoad(host.load));
    facts.push({ icon: "cpu", value: parts.join(" · "), title: "vCPUs · load (1 min)" });
  }
  if (host.memory_used_bytes != null && host.memory_total_bytes != null) {
    facts.push({ icon: "memory", value: `${formatBytes(host.memory_used_bytes)}/${formatBytes(host.memory_total_bytes)}`, title: "memory used / total" });
  }
  if (host.disk_free_bytes != null && host.disk_total_bytes != null) {
    facts.push({ value: `disk ${formatBytes(host.disk_free_bytes)}/${formatBytes(host.disk_total_bytes)}`, title: "disk free / total" });
  }
  if (host.uptime_secs != null) {
    facts.push({ value: `up ${formatUptime(host.uptime_secs)}` });
  }
  return facts;
}

export interface ColonyFact {
  icon?: "cpu";
  value: string;
  title?: string;
}

/**
 * The second meta line of an overview colony row — the microVM it booted, the boot itself, its mesh
 * address and its agent. Only measured facts get a segment; a colony booted before this change has
 * none of the first three, and its row reads exactly as it did before.
 */
export function colonyFacts(session: Session): ColonyFact[] {
  const facts: ColonyFact[] = [];
  if (session.boot_cpus != null || session.boot_memory != null) {
    const parts: string[] = [];
    if (session.boot_cpus != null) parts.push(`${session.boot_cpus}c`);
    if (session.boot_memory != null) parts.push(session.boot_memory);
    facts.push({ icon: "cpu", value: parts.join(" · "), title: "the microVM this colony boots" });
  }
  if (session.boot_timing?.total_ms != null) {
    facts.push({ value: `boot ${formatBootMs(session.boot_timing.total_ms)}`, title: "last boot, end to end" });
  }
  if (session.mesh?.ip) {
    facts.push({ value: `mesh ${session.mesh.ip}`, title: "this colony's address on the private mesh" });
  }
  if (session.agent) {
    facts.push({ value: session.agent });
  }
  return facts;
}