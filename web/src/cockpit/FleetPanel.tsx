// The fleet panel (issue #231): every host the mothership can see — itself, plus each peer
// configured via COLONIZER_FLEET_PEERS — as one row apiece, so a second machine is finally
// something the cockpit shows instead of something the API silently ignored. The core ask is a
// stalled peer must look obviously different from a healthy idle one: it reuses the exact tone
// idiom the session status dot already uses (`SESSION_STATUS`'s tone → a small colored dot plus a
// label) rather than inventing a new visual language for "this machine is not answering".
//
// A fleet of one (self only, no peers configured) renders nothing — the panel would otherwise be
// a redundant single row repeating the host strip already above it, which is the common case today
// and must not add noise to it.
import type { ReactElement } from "react";

import { IconServer } from "../components/icons";
import { type Tone } from "../components/ui";
import { fleetHostFacts, timeSinceHeartbeat } from "./fleet";
import type { FleetHost } from "../types";

const TONE_VAR: Record<Tone, string> = {
  neutral: "var(--faint)",
  info: "var(--info)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  err: "var(--err)",
  accent: "var(--accent)",
};

const HEALTH_TONE: Record<FleetHost["health"], Tone> = {
  online: "ok",
  unreachable: "err",
};

/** Full class names, spelled out (not built from a template), so Tailwind's scanner keeps them. */
const HEALTH_DOT: Record<FleetHost["health"], string> = {
  online: "bg-ok",
  unreachable: "bg-err",
};

const HEALTH_LABEL: Record<FleetHost["health"], string> = {
  online: "online",
  unreachable: "unreachable",
};

/** One host's row: name, platform/os, its facts, and the online/unreachable dot that is the whole
 * point of the panel. */
function FleetHostRow({ host }: { host: FleetHost }): ReactElement {
  const tone = HEALTH_TONE[host.health];
  const edge = TONE_VAR[tone];
  const facts = fleetHostFacts(host);
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-3.5 py-2 last:border-b-0">
      <span aria-hidden="true" className={`h-[7px] w-[7px] shrink-0 rounded-full ${HEALTH_DOT[host.health]}`} />
      <span className="min-w-0 truncate font-mono text-[11.5px] font-semibold" title={host.id}>
        {host.name}
      </span>
      {(host.platform || host.os) && (
        <span className="truncate font-mono text-[11px] text-faint">
          {[host.platform, host.os].filter(Boolean).join(" · ")}
        </span>
      )}
      <span className="whitespace-nowrap font-mono text-[11px] font-medium" style={{ color: edge }}>
        {HEALTH_LABEL[host.health]}
      </span>
      {facts.map((fact, i) => (
        <span key={i} title={fact.title} className="inline-flex items-center gap-1 whitespace-nowrap font-mono text-[11px] text-faint">
          {fact.icon === "server" && <IconServer size={11} className="shrink-0" />}
          {fact.value}
        </span>
      ))}
      <span title="when this host last answered a poll" className="ml-auto whitespace-nowrap font-mono text-[11px] text-faint">
        {timeSinceHeartbeat(host.last_heartbeat)}
      </span>
    </div>
  );
}

/**
 * The fleet section: self plus every peer, as a small stack of rows beneath the host strip. Only
 * renders once there is more than one host — a single-machine install (still the common case) sees
 * no new panel at all, since the host strip above already covers it in full.
 */
export function FleetPanel({ hosts }: { hosts: FleetHost[] }): ReactElement | null {
  if (hosts.length <= 1) return null;
  return (
    <div className="flex flex-col overflow-hidden rounded-xl border border-border bg-panel">
      <div className="border-b border-border px-3.5 py-2 font-mono text-[10.5px] tracking-[0.12em] text-faint">
        FLEET · {hosts.length} hosts
      </div>
      {hosts.map((host) => (
        <FleetHostRow key={host.id} host={host} />
      ))}
    </div>
  );
}
