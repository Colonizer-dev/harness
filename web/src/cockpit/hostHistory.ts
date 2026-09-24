// The host's recent readings, kept in the page while the cockpit is open. The mothership reports the
// host as a snapshot on every status poll and keeps no history, so the Host view's trend lines are
// honestly "since this page opened": each new probe (a new `checked_at`) is one sample, capped.
import { useSyncExternalStore } from "react";

import type { HostInfo } from "../types";

export interface HostSample {
  at: number;
  /** 1-minute load ÷ cores, 0–1+ (above 1 is oversubscribed); null when unmeasured. */
  cpu: number | null;
  /** Used ÷ total memory, 0–1; null when unmeasured. */
  memory: number | null;
  /** Used ÷ total disk, 0–1; null when unmeasured. */
  disk: number | null;
  microvms: number;
}

/** About half an hour at the status poll's pace. */
export const HISTORY_CAP = 360;

/** One reading as the trend lines want it. */
export function sampleOf(host: HostInfo): HostSample {
  const ratio = (used?: number, total?: number) => (used != null && total != null && total > 0 ? used / total : null);
  return {
    at: Date.parse(host.checked_at) || Date.now(),
    cpu: host.load != null && host.cpu_cores ? host.load[0] / host.cpu_cores : null,
    memory: ratio(host.memory_used_bytes, host.memory_total_bytes),
    disk: ratio(host.disk_used_bytes, host.disk_total_bytes),
    microvms: host.microvms_live,
  };
}

/** Appends a probe unless it is the one already last (the same `checked_at`), keeping the newest `cap`. */
export function appendSample(history: readonly HostSample[], sample: HostSample, cap = HISTORY_CAP): HostSample[] {
  if (history.length > 0 && history[history.length - 1].at === sample.at) return history as HostSample[];
  const next = [...history, sample];
  return next.length > cap ? next.slice(next.length - cap) : next;
}

let samples: HostSample[] = [];
const listeners = new Set<() => void>();

/** Records a host reading; the cockpit calls it on every status, whichever view is showing. */
export function recordHost(host: HostInfo | null | undefined): void {
  if (!host) return;
  const next = appendSample(samples, sampleOf(host));
  if (next === samples) return;
  samples = next;
  listeners.forEach((l) => l());
}

export function useHostHistory(): readonly HostSample[] {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => samples,
    () => samples,
  );
}

/** `8G`, `512M`, `2048` (MiB) as bytes — the sandbox's memory setting shape; null when unreadable. */
export function parseSize(value: string | null | undefined): number | null {
  if (!value) return null;
  const m = /^\s*(\d+(?:\.\d+)?)\s*([KMGT]?)i?B?\s*$/i.exec(value);
  if (!m) return null;
  const n = Number(m[1]);
  const unit = m[2].toUpperCase();
  const mult = unit === "K" ? 1024 : unit === "M" || unit === "" ? 1024 ** 2 : unit === "G" ? 1024 ** 3 : 1024 ** 4;
  return Math.round(n * mult);
}
