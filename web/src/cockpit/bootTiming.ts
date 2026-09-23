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

const NOTHING_YET = "starting · no phase finished yet";
