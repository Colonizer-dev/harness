// The decisions inbox's data in the cockpit (issue #1036): one read of GET /api/decisions every 30
// seconds — the mothership answers it from its own cache, so this never reaches GitHub — and the
// count it adds to the inbox badge.
import { useCallback, useEffect, useState } from "react";

import type { Api } from "../api";
import { needsYou } from "../notifications";
import type { DecisionsView, Session } from "../types";

const REFRESH_MS = 30_000;

/** The inbox's decisions and pull-request cards, refreshed on a timer and after every action. */
export function useDecisions(api: Pick<Api, "decisions">): { view: DecisionsView | null; refresh: () => Promise<void> } {
  const [view, setView] = useState<DecisionsView | null>(null);
  const refresh = useCallback(async () => {
    try {
      setView(await api.decisions());
    } catch {
      // An older mothership has no route: the inbox shows the colonies alone, as before.
    }
  }, [api]);
  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [refresh]);
  return { view, refresh };
}

/**
 * What the decisions inbox adds to the "Needs you" count: every decision and pull-request card,
 * except a card about a colony that already counts as needing you (a policy hold on a flagged
 * colony), so one thing never counts twice.
 */
export function decisionsCount(view: DecisionsView | null, sessions: Session[]): number {
  if (!view) return 0;
  const counted = new Set(sessions.filter(needsYou).map((s) => s.id));
  return view.decisions.length + view.prs.filter((card) => !card.colony || !counted.has(card.colony)).length;
}

/** The colonies a pull-request card already covers, so the inbox does not list them twice. */
export function prCardColonyIds(view: DecisionsView | null): Set<string> {
  return new Set((view?.prs ?? []).flatMap((card) => (card.colony ? [card.colony] : [])));
}
