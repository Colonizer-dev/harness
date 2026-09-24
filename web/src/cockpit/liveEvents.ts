// What changed between two session lists, as the cockpit's realtime chrome shows it (Cockpit
// Dashboards v3): a one-line ticker in the header, a brief flash on a row whose status moved, and a
// highlight on a cost that just rose. Everything is derived from the session list the stream (or the
// poll behind it) already delivers — nothing here fetches, and nothing is invented: an event is only
// emitted for a change actually observed between two lists.
import { useEffect, useRef, useState } from "react";

import { colonyLabel } from "../notifications";
import { formatCost, sessionCost } from "../spend";
import type { Session, SessionStatus } from "../types";

export type LiveEventKind = "started" | "asked" | "resumed" | "pr" | "merged" | "failed" | "stopped" | "ended" | "spent";

export interface LiveEvent {
  id: string;
  kind: LiveEventKind;
  text: string;
  at: number;
}

/** How long a moved row stays flashed, and a risen cost stays lit, in ms. */
export const FLASH_MS = 2000;
export const BUMP_MS = 1500;

const ENDED: readonly SessionStatus[] = ["no_changes", "closed"];

function statusEvent(prev: SessionStatus, next: SessionStatus): LiveEventKind | null {
  if (prev === next) return null;
  if (next === "waiting_for_answer") return "asked";
  if (prev === "waiting_for_answer" && (next === "running" || next === "starting")) return "resumed";
  if (next === "starting" || (next === "running" && prev === "queued")) return "started";
  if (next === "pr_opened") return "pr";
  if (next === "merged") return "merged";
  if (next === "failed") return "failed";
  if (next === "stopped") return "stopped";
  if (ENDED.includes(next)) return "ended";
  return null;
}

const VERB: Record<Exclude<LiveEventKind, "spent">, string> = {
  started: "started",
  asked: "asked a question",
  resumed: "resumed",
  pr: "opened a PR",
  merged: "merged",
  failed: "failed",
  stopped: "stopped",
  ended: "ended",
};

/**
 * The events between two lists. A colony missing from `prev` is new (started or queued); one whose
 * status moved gets a status event; one whose measured cost rose gets a `spent` event. A first list
 * (prev null or empty) yields nothing — a page load is not news.
 */
export function diffSessions(prev: readonly Session[] | null, next: readonly Session[], at: number): LiveEvent[] {
  if (!prev || prev.length === 0) return [];
  const before = new Map(prev.map((s) => [s.id, s]));
  const out: LiveEvent[] = [];
  for (const s of next) {
    const label = colonyLabel(s.repo, s.issue);
    const old = before.get(s.id);
    if (!old) {
      out.push({ id: s.id, kind: "started", text: s.status === "queued" ? `${label} queued` : `${label} started`, at });
      continue;
    }
    const kind = statusEvent(old.status, s.status);
    if (kind && kind !== "spent") out.push({ id: s.id, kind, text: `${label} ${VERB[kind]}`, at });
    const was = sessionCost(old);
    const now = sessionCost(s);
    if (now != null && now > (was ?? 0) + 1e-9) {
      out.push({ id: s.id, kind: "spent", text: `${label} +${formatCost(now - (was ?? 0))}`, at });
    }
  }
  return out;
}

/** Status events outrank cost ticks for the ticker: a question is news, a cent is not. */
function headline(events: readonly LiveEvent[]): LiveEvent | null {
  return events.find((e) => e.kind !== "spent") ?? events[0] ?? null;
}

export interface LiveEvents {
  /** The newest headline event, for the ticker; null until something has changed. */
  latest: LiveEvent | null;
  /** Session id → when its status last moved (for the row flash). */
  flashed: Readonly<Record<string, number>>;
  /** Session id → when its cost last rose (for the cost highlight). */
  bumped: Readonly<Record<string, number>>;
  /** The last few events, newest first. */
  recent: readonly LiveEvent[];
}

const EMPTY: LiveEvents = { latest: null, flashed: {}, bumped: {}, recent: [] };

/**
 * Watches the session list and remembers what moved. Re-renders once more after the flash window so
 * a flashed row settles back without waiting for the next push.
 */
export function useLiveEvents(sessions: readonly Session[], keep = 12): LiveEvents {
  const prev = useRef<readonly Session[] | null>(null);
  const [state, setState] = useState<LiveEvents>(EMPTY);
  const [, settle] = useState(0);

  useEffect(() => {
    const at = Date.now();
    const events = diffSessions(prev.current, sessions, at);
    // The app's first render has an empty list until the first fetch lands; that fetch is a page
    // load, not news, so the baseline is the first non-empty list.
    if (prev.current !== null || sessions.length > 0) prev.current = sessions;
    if (events.length === 0) return;
    setState((current) => {
      const flashed = { ...current.flashed };
      const bumped = { ...current.bumped };
      for (const e of events) {
        if (e.kind === "spent") bumped[e.id] = at;
        else flashed[e.id] = at;
      }
      return {
        latest: headline(events) ?? current.latest,
        flashed,
        bumped,
        recent: [...events.filter((e) => e.kind !== "spent"), ...current.recent].slice(0, keep),
      };
    });
    const timer = setTimeout(() => settle((n) => n + 1), FLASH_MS + 50);
    return () => clearTimeout(timer);
  }, [sessions, keep]);

  return state;
}

/** Whether `id` moved within the flash window. */
export function isFlashed(events: LiveEvents, id: string, now = Date.now()): boolean {
  const at = events.flashed[id];
  return at != null && now - at < FLASH_MS;
}

/** Whether `id`'s cost rose within the bump window. */
export function isBumped(events: LiveEvents, id: string, now = Date.now()): boolean {
  const at = events.bumped[id];
  return at != null && now - at < BUMP_MS;
}
