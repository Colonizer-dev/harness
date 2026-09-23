// The inspector's BOOT section (issue #360): where a colony's launch time went, phase by phase. The
// mothership measures and names the phases; this only formats what it sent, in the order it sent
// them, so a phase added on the Rust side shows up here without a frontend change.
import { formatAvgLatency } from "../providerHealth";
import type { Session } from "../types";
import { formatBootMs } from "./host";

export interface BootRow {
  name: string;
  /** Formatted: sub-second phases keep their ms, since `issue` alone is often ~240 ms. */
  duration: string;
  /** True on exactly one row — the longest phase, the first of them on a tie. */
  slowest: boolean;
}

export interface BootView {
  rows: BootRow[];
  /** While starting, how far the boot got; once finished, its end-to-end total; if it stopped, where. */
  summary: string;
}

/**
 * The BOOT section's rows and summary line; null when there is nothing to show — a colony booted
 * before boot timing existed. A starting colony always gets a view, even before its first phase
 * lands, so the section says the boot is under way instead of staying absent.
 */
export function bootView(timing: Session["boot_timing"], starting: boolean): BootView | null {
  if (!timing) return starting ? { rows: [], summary: NOTHING_YET } : null;
  const { phases } = timing;
  // Strict `>` keeps the first of equal phases, so a tie highlights one row, not several.
  let slowest = -1;
  phases.forEach((phase, i) => {
    if (slowest === -1 || phase.ms > phases[slowest].ms) slowest = i;
  });
  const rows = phases.map((phase, i) => ({ name: phase.name, duration: formatAvgLatency(phase.ms), slowest: i === slowest }));
  const last = phases.at(-1);
  if (starting) {
    return { rows, summary: last ? `starting · last done: ${last.name}` : NOTHING_YET };
  }
  // The mothership sends `total_ms` only for a boot that finished, so its absence on a colony no
  // longer starting means the boot stopped part way; summing the phases would pass that off as a total.
  if (timing.total_ms == null) {
    return { rows, summary: last ? `stopped after ${last.name}` : "stopped before the first phase" };
  }
  // Same rounding as the overview row's `boot 94s`, so the two never disagree.
  return { rows, summary: `total ${formatBootMs(timing.total_ms)}` };
}

export interface BootMedianView {
  /** How many finished boots the medians were taken across. */
  count: number;
  /** One row per phase name, formatted like a `bootView` row; exactly one is the slowest median. */
  rows: BootRow[];
  /** The median end-to-end boot, in the same `total 94s` shape as a colony's BOOT summary. */
  summary: string;
}

/** Upper-middle median, the same convention as the Rust telemetry aggregate `boot_ms()`. */
function upperMedian(samples: number[]): number {
  const sorted = [...samples].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
}

/** Cross-colony medians for the mothership pane: per-phase medians across the most recent
 * finished boots (numeric `total_ms` only, mirroring `boot_ms()`), for comparing boot speed
 * before/after warm-start changes. Null with no finished boot. */
export function bootMedians(sessions: Session[], limit = 20): BootMedianView | null {
  const finished = sessions.filter((session) => typeof session.boot_timing?.total_ms === "number");
  if (finished.length === 0 || limit <= 0) return null;
  // `created_at` is the launch, the closest the API gets to when a colony booted. The sidebar and
  // feed sort by `updated_at`, but that moves with chat long after the boot.
  const sampled = [...finished]
    .sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
    .slice(0, limit);
  const perPhase = new Map<string, number[]>();
  const totals: number[] = [];
  for (const session of sampled) {
    const total = session.boot_timing?.total_ms;
    if (typeof total === "number") totals.push(total);
    for (const phase of session.boot_timing?.phases ?? []) {
      const samples = perPhase.get(phase.name) ?? [];
      samples.push(phase.ms);
      perPhase.set(phase.name, samples);
    }
  }
  const order = [...perPhase.keys()];
  const medians = new Map(order.map((name) => [name, upperMedian(perPhase.get(name) ?? [])]));
  // Strict `>` keeps the first of equal medians, the same tie rule as `bootView`.
  let slowest: string | null = null;
  for (const name of order) {
    if (slowest == null || (medians.get(name) ?? 0) > (medians.get(slowest) ?? 0)) slowest = name;
  }
  return {
    count: sampled.length,
    rows: order.map((name) => ({ name, duration: formatAvgLatency(medians.get(name) ?? 0), slowest: name === slowest })),
    summary: `median total ${formatBootMs(upperMedian(totals))}`,
  };
}

const NOTHING_YET = "starting · no phase finished yet";
