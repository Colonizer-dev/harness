// The thin main-thread side of the worker poll schedule (issue #159): creates the worker, fans
// each tick out to that name's callback, and cleans up on unmount. Everything below touches
// `Worker` or `window` and is deliberately left to the untestable side of the suite — like
// notifications.ts, the decisions (names and cadences) live in pollSchedule.ts as pure data so
// the tests can pin them in plain node.
//
// The fallback matters for contexts without module workers (some embedded webviews, mounts
// outside a bundler): plain setIntervals keep the same cadences, just without the hidden-tab
// immunity the worker buys.

import { useEffect, useRef } from "react";
import { POLL_CADENCES, POLL_TICK_NAMES, type PollTickMessage, type PollTickName } from "./pollSchedule";

/** One callback per tick name; every name is required so a poll can never be dropped silently. */
export type PollCallbacks = Record<PollTickName, () => void>;

export function usePollTick(callbacks: PollCallbacks): void {
  // The callbacks close over fresh state every render but the subscription is created once, so
  // the ref always holds the latest without ever re-creating the worker or the intervals.
  const latest = useRef(callbacks);
  latest.current = callbacks;

  useEffect(() => {
    const fire = (name: PollTickName): void => {
      latest.current[name]();
    };
    let cleanup: () => void;
    try {
      const worker = new Worker(new URL("./pollWorker.ts", import.meta.url), { type: "module" });
      worker.onmessage = (event: MessageEvent<PollTickMessage>) => {
        if (event.data.type === "tick") fire(event.data.name);
      };
      worker.postMessage({ type: "start" });
      cleanup = () => {
        worker.postMessage({ type: "stop" });
        worker.terminate();
      };
    } catch {
      // No Worker construction here: the same cadences on the main thread instead.
      const ids = POLL_TICK_NAMES.map((name) => window.setInterval(() => fire(name), POLL_CADENCES[name]));
      cleanup = () => {
        ids.forEach((id) => window.clearInterval(id));
      };
    }
    return () => cleanup();
  }, []);
}
