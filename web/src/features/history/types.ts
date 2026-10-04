// Cross-colony transcript search (issue #739) — GET /api/history/search.
import type { SessionStatus } from "../sessions/types";

/**
 * One matching turn in a colony's transcript (GET /api/history/search, issue #739). `turn` is the
 * protocol message id the turn renders under — the cockpit scrolls to `turn-<id>` — and null when
 * the mothership cannot name one, in which case opening the colony alone is enough.
 */
export interface HistoryHit {
  colony: string;
  repo: string;
  org: string;
  agent: string;
  status: SessionStatus | (string & {});
  created_at: string;
  seq: number;
  ts: string;
  turn: string | null;
  role: "user" | "assistant";
  /** The matched text, drawn as plain text — never HTML. */
  snippet: string;
}

/** The filters GET /api/history/search takes; `q` is required (the mothership answers 400 without it). */
export interface HistorySearchQuery {
  q: string;
  repo?: string;
  org?: string;
  agent?: string;
  status?: string;
  since?: string;
  until?: string;
  limit?: number;
}
