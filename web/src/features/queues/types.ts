// The queues API's payload types (issue #1127): what GET /api/queues answers — every colony
// waiting to start, why it waits, what it would take to move it, and how full each host is.

import type { Attention, SessionStatus } from "../sessions/types";

/** One queued colony: `org/repo#issue`, why it waits, and which actions the server would take. */
export interface QueuesRow {
  /** The colony's id (a session id), so a row links to `/colonies/{id}` and names the bulk actions. */
  id: string;
  org: string;
  repo: string;
  /** The issue the colony works; null for one launched without an issue. */
  issue: number | null;
  /** The colony's own title, null until the issue was read. */
  title: string | null;
  /** The colony branch it will start on, null before the worktree exists. */
  branch: string | null;
  /** The host the colony waits on; null when the mothership cannot say (a fleet peer it lost). */
  host: string | null;
  /** The agent module the colony will launch; null on older motherships. */
  agent: string | null;
  status: SessionStatus;
  created_at: string;
  /** The colony's own queue priority, higher first; 0 follows its org again. */
  priority: number;
  /** Why it waits, as the mothership writes it (`repo_pr_rate_limit`, …); null for a plain queued row. */
  reason: string | null;
  /** One human line more (`detail`), null when the reason says enough. */
  detail: string | null;
  /** The watchdog's attention item, when the colony needs a person despite waiting. */
  attention: Attention | null;
  /** When an automatic wait ends (a parked colony's reset, a retry's back-off); null for the rest. */
  resumes_at: string | null;
  /** A merge superseded this colony: held out of the queue until it is kept (issue #673). */
  held: boolean;
  /** An org or install policy holds it: nothing starts it until the hold is lifted. */
  policy_hold: boolean;
  /** Which of the bulk actions the server would take on this row right now. */
  actions: { resume: boolean; stop: boolean; restart: boolean };
  /** Why not, one entry per false action, in the server's own words. */
  why_not: Partial<Record<"resume" | "stop" | "restart", string>>;
}

/** One host's queue and capacity: how full it runs and how much is waiting on it. */
export interface QueuesHost {
  name: string;
  reachable: boolean;
  slots_in_use: number;
  slots_ceiling: number;
  /** Live colonies, parked ones and waiting ones; null for a fleet peer this mothership cannot count. */
  running: number | null;
  parked: number | null;
  queued: number | null;
  /** Everything waiting on the host, whatever the counters above could say. */
  queue_depth: number;
  /** More colonies want slots than the ceiling allows: the host is the bottleneck. */
  over_ceiling: boolean;
}

/** GET /api/queues: the waiting rows, the hosts they wait on, and the two queue-wide switches. */
export interface QueuesPayload {
  rows: QueuesRow[];
  hosts: QueuesHost[];
  /** A drain holds the queue for an update or restart; new colonies stay queued until it finishes. */
  draining: boolean;
  /** COLONIZER_NO_EXTERNAL_EFFECTS: colonies queue, but every publish is refused until it is lifted. */
  external_writes_blocked: boolean;
  queue_depth: number;
}

/** How the view groups its rows. `none` is the flat list; the rest group by the row's host, its
 *  machine wait reason, or its repository — client-side only, never sent to the server. */
export type QueuesGroup = "none" | "host" | "reason" | "repo";

/** The view's filters, one per server query parameter plus the client-side grouping. */
export interface QueuesFilters {
  host?: string;
  reason?: string;
  repo?: string;
  agent?: string;
  q?: string;
  group?: QueuesGroup;
}
