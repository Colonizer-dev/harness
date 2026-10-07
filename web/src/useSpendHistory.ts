import { useEffect, useState } from "react";
import { useApi } from "./context";
import type { SpendHistory } from "./types";

/**
 * The overview's daily spend history for the last `days` days (GET /api/spend/history,
 * issue #209), loaded once on mount. The browser's UTC offset rides along so the days are the
 * reader's local ones (issue #613), matching how the overview buckets and labels them. Null before
 * the first response lands and on any failure: an older mothership (or a flaky one) simply gets org
 * cards without sparklines — never a broken overview.
 */
export function useSpendHistory(days = 30): SpendHistory | null {
  const api = useApi();
  const [history, setHistory] = useState<SpendHistory | null>(null);
  useEffect(() => {
    let cancelled = false;
    // `-getTimezoneOffset()` is the minutes east of UTC, what the mothership's `tz_offset_minutes` wants.
    const tzOffsetMinutes = -new Date().getTimezoneOffset();
    api
      .spendHistory(days, tzOffsetMinutes)
      .then((result) => {
        if (!cancelled) setHistory(result);
      })
      .catch(() => {
        /* sparklines are decoration; the cards stand without them */
      });
    return () => {
      cancelled = true;
    };
  }, [api, days]);
  return history;
}