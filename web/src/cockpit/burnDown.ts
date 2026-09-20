// Pure formatting/decision helpers for the burn-down Overview card (issue #210). No React, no API
// calls: every function takes what it needs and returns plain values, so the card stays thin and
// the tests stay trivial.
import type { Tone } from "../components/ui";
import type { BurnDownState, BurnDownStatus } from "../types";

export type { BurnDownState };

const DAY_MS = 86_400_000;
const HOUR_MS = 3_600_000;
const MINUTE_MS = 60_000;

/** Milliseconds from `now` until `iso`; null when the timestamp is absent or unparseable. */
export function msUntil(iso: string | null, now: Date): number | null {
  if (!iso) return null;
  const ms = new Date(iso).getTime();
  if (!Number.isFinite(ms)) return null;
  return Math.max(0, ms - now.getTime());
}

/** Milliseconds until the plan reset, per the status: null when the backend hasn't scheduled one. */
export function msToReset(status: BurnDownStatus, now: Date): number | null {
  return msUntil(status.next_reset, now);
}

/** "2d 5h", "3h 12m", "<1m" — and "—" when there is nothing to count down to. */
export function formatCountdown(ms: number | null): string {
  if (ms === null) return "—";
  if (ms < MINUTE_MS) return "<1m";
  const days = Math.floor(ms / DAY_MS);
  const hours = Math.floor((ms % DAY_MS) / HOUR_MS);
  const minutes = Math.floor((ms % HOUR_MS) / MINUTE_MS);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}

const STATE_LABELS: Record<BurnDownState, string> = {
  disabled: "Off",
  unconfigured: "Needs repos",
  unknown_allowance: "Allowance unknown",
  outside_window: "Waiting for window",
  burning: "Burning down",
  at_reserve: "At reserve",
};

export function stateLabel(state: BurnDownState): string {
  return STATE_LABELS[state] ?? state;
}

const STATE_TONES: Record<BurnDownState, Tone> = {
  disabled: "neutral",
  unconfigured: "warn",
  unknown_allowance: "warn",
  outside_window: "neutral",
  burning: "accent",
  at_reserve: "ok",
};

export function stateTone(state: BurnDownState): Tone {
  return STATE_TONES[state] ?? "neutral";
}

export function usd(n: number): string {
  return `$${n.toFixed(2)}`;
}

/**
 * Whether the Overview should render the card at all: while the scheduler is enabled, and — after a
 * stop — until the next reset, because a scheduler that has just been switched off but still has
 * colonies worth showing should not blink out of the page mid-session. The reset eventually clears
 * it: with nothing enabled and no colonies, there is nothing left to look at.
 */
export function shouldShow(status: BurnDownStatus): boolean {
  return status.enabled || status.colonies.total > 0;
}