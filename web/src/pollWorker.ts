// The tick owner for the dashboard's polls (issue #159). This module runs as a module worker —
// the main thread loads it only through `new Worker(new URL(...))`, never by import — so its
// timers are the worker's own and keep their cadence while the tab is hidden, where Chrome
// throttles main-thread timers to one wake-up per minute. No visibility logic lives here: the
// schedule alone avoids the intensive throttling, and the main thread re-polls on
// hidden->visible anyway (App.tsx), so a returning tab refreshes instantly.
//
// Each cadence is its own setTimeout chain rather than one shared interval: a slow tick can
// never delay the others, and stopping is just clearing the chains.

import { POLL_CADENCES, POLL_TICK_NAMES, type PollControlMessage, type PollTickMessage, type PollTickName } from "./pollSchedule";

// A minimal view of the worker global. The DOM lib types `self` as a Window whose postMessage
// needs a targetOrigin the worker scope does not take, so this file talks through globalThis
// instead — which the worker, unlike the window, shares with these bare functions.
const scope = globalThis as unknown as {
  postMessage(message: PollTickMessage): void;
  onmessage: ((event: MessageEvent<PollControlMessage>) => void) | null;
};

const timerIds = new Map<PollTickName, number>();

function schedule(name: PollTickName): void {
  timerIds.set(
    name,
    globalThis.setTimeout(() => {
      scope.postMessage({ type: "tick", name });
      schedule(name);
    }, POLL_CADENCES[name]),
  );
}

/** Idempotent: a second start (worker load racing a main-thread "start") schedules nothing twice. */
function start(): void {
  if (timerIds.size > 0) return;
  for (const name of POLL_TICK_NAMES) schedule(name);
}

function stop(): void {
  for (const id of timerIds.values()) globalThis.clearTimeout(id);
  timerIds.clear();
}

scope.onmessage = (event: MessageEvent<PollControlMessage>) => {
  if (event.data.type === "stop") stop();
  else if (event.data.type === "start") start();
};

start();
