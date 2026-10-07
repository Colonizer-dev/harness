// Per-colony "why is this not progressing" (issue #230). The list endpoint and the WS
// `session` frame never carry `diagnosis`/`recent_events` — only GET /api/sessions/{id} does —
// so a non-terminal colony polls that route here. Terminal colonies never fetch.
import { useEffect, useState } from "react";
import { useApi } from "./context";
import { isTerminal } from "./notifications";
import type { Diagnosis, RecentEvent, Session } from "./types";

/** Re-poll while non-terminal: deliberately slower than the 4s overview rate. */
export const DIAGNOSIS_POLL_MS = 10_000;

export function useSessionDiagnosis(session: Session | null): {
  diagnosis: Diagnosis | null;
  recentEvents: RecentEvent[];
} {
  const api = useApi();
  const [fetched, setFetched] = useState<{ diagnosis: Diagnosis | null; recentEvents: RecentEvent[] } | null>(null);
  const id = session?.id ?? null;
  const terminal = session ? isTerminal(session.status) : true;
  useEffect(() => {
    // A new colony starts from its embedded fields: the previous fetch must not linger past the switch.
    setFetched(null);
    if (!id || terminal) {
      return;
    }
    let active = true;
    const load = () => {
      api.session(id).then(
        (s) => {
          if (active) setFetched({ diagnosis: s.diagnosis ?? null, recentEvents: s.recent_events ?? [] });
        },
        () => {
          /* a diagnosis is a courtesy: errors hide the rows, never toast */
        },
      );
    };
    load();
    const timer = setInterval(load, DIAGNOSIS_POLL_MS);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, [api, id, terminal]);
  if (!session || terminal) return { diagnosis: null, recentEvents: [] };
  // The embedded fields answer the first paint (and static renders); the poll refreshes them.
  return fetched ?? { diagnosis: session.diagnosis ?? null, recentEvents: session.recent_events ?? [] };
}
