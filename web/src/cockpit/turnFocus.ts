// Turn focus (issue #739): History's transcript search opens a colony at the exact turn a hit came
// from. The colony pane is a slot App builds — Cockpit never renders SessionView itself — so the
// request travels as module state rather than props. Each `focusTurn` bumps a counter, so picking
// the same hit twice still re-scrolls; the panel keys its "already handled" flag on that counter.
import { useEffect, useState } from "react";

export interface TurnFocus {
  id: string | null;
  /** Bumped on every request, so a repeat of the same id is still a fresh request. */
  n: number;
}

let request: TurnFocus = { id: null, n: 0 };
const listeners = new Set<() => void>();

/** Ask the chat panel to scroll to this message id and flash it, as soon as it renders. */
export function focusTurn(id: string | null): void {
  request = { id, n: request.n + 1 };
  for (const notify of listeners) notify();
}

/** The current request; a new `n` means an unhandled one. */
export function pendingTurn(): TurnFocus {
  return request;
}

/** The pending request, re-read whenever a new focus is asked for — the panel's effect waits on it. */
export function usePendingTurn(): TurnFocus {
  const [value, setValue] = useState(request);
  useEffect(() => {
    const notify = () => setValue(request);
    listeners.add(notify);
    notify();
    return () => {
      listeners.delete(notify);
    };
  }, []);
  return value;
}
