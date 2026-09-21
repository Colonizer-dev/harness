// The dashboard's poll schedule, shared by the worker that owns the ticks (pollWorker.ts) and
// the hook that subscribes to them (usePollTick.ts). It lives on its own so the tests can pin
// the cadences in the plain node environment — neither a Worker nor the DOM is needed to
// import this module.
//
// Why a worker owns the schedule (issue #159): Chrome throttles timers in a hidden tab to one
// wake-up per minute, so the 4 s session poll that feeds the sidebar — and through it the
// notifications for colonies the open chat is not showing — could lag a full minute behind.
// Worker timers are not subject to that intensive throttling, so the ticks keep their cadence
// while the tab is hidden.

/** One tick per poll loop App.tsx used to own as a setInterval; the ms match those intervals exactly. */
export const POLL_CADENCES = {
  sessions: 4_000,
  redRuns: 5_000,
  pendingMemory: 10_000,
  orgs: 15_000,
  status: 30_000,
  fleet: 30_000,
  update: 900_000,
} as const;

export type PollTickName = keyof typeof POLL_CADENCES;

/** Every tick name, for the worker (one chain each) and the fallback (one interval each). */
export const POLL_TICK_NAMES = Object.keys(POLL_CADENCES) as PollTickName[];

/** What the worker posts on every tick; the hook fans it out to that name's callback. */
export interface PollTickMessage {
  type: "tick";
  name: PollTickName;
}

/** What the main thread may send; the worker starts itself on load and stops on unmount. */
export type PollControlMessage = { type: "start" } | { type: "stop" };
