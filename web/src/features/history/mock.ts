// The `history` feature's mock methods (issue #739).
// Shared state lives in src/mockState.ts.
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { HistoryApi } from "./api";
import type { HistoryHit, HistorySearchQuery } from "./types";

export function historyMock(ms: MockState): HistoryApi {
  return {
    // GET /api/history/search (issue #739): the mock's transcript is each colony's brief, so one
    // hit per matching colony points at that colony's `initial` turn.
    historySearch: (q: HistorySearchQuery) =>
      ms.later((): { hits: HistoryHit[] } => {
        const needle = q.q.trim().toLowerCase();
        if (!needle) throw new ApiError("q is required", 400);
        const hits: HistoryHit[] = [];
        for (const { session } of ms.sessions.values()) {
          if (q.repo && session.repo !== q.repo) continue;
          if (q.org && session.repo.split("/")[0].toLowerCase() !== q.org.toLowerCase()) continue;
          if (q.agent && session.agent !== q.agent) continue;
          if (q.status && session.status !== q.status) continue;
          if (q.since && session.created_at.slice(0, 10) < q.since) continue;
          if (q.until && session.created_at.slice(0, 10) > q.until) continue;
          const text = session.issue_title ?? "";
          if (!text.toLowerCase().includes(needle)) continue;
          hits.push({
            colony: session.id,
            repo: session.repo,
            org: session.repo.split("/")[0],
            agent: session.agent,
            status: session.status,
            created_at: session.created_at,
            seq: hits.length + 1,
            ts: session.updated_at,
            turn: "initial",
            role: "user",
            snippet: text,
          });
        }
        return { hits: hits.slice(0, 50) };
      }),
  };
}
