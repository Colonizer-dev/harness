import { useEffect, useState } from "react";
import { useApi } from "./context";
import type { SpendHistory } from "./types";

/**
 * The overview's daily spend history for the last 30 days (GET /api/spend/history, issue #209),
 * loaded once on mount. Null before the first response lands and on any failure: an older mothership
 * (or a flaky one) simply gets org cards without sparklines — never a broken overview.
 */
export function useSpendHistory(): SpendHistory | null {
  const api = useApi();
  const [history, setHistory] = useState<SpendHistory | null>(null);
  useEffect(() => {
    let cancelled = false;
    api
      .spendHistory(30)
      .then((result) => {
        if (!cancelled) setHistory(result);
      })
      .catch(() => {
        /* sparklines are decoration; the cards stand without them */
      });
    return () => {
      cancelled = true;
    };
  }, [api]);
  return history;
}