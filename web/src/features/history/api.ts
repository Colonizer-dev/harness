// Cross-colony transcript search API (issue #739).
// The root `Api` interface composes this with the other features.
import { query, request } from "../../http";
import type { HistoryHit, HistorySearchQuery } from "./types";

export interface HistoryApi {
  /** GET /api/history/search (issue #739): the turns across colony transcripts matching `q`, at most 50; 400 on an empty query. */
  historySearch(query: HistorySearchQuery): Promise<{ hits: HistoryHit[] }>;
}

export const historyHttp: HistoryApi = {
  historySearch: (q) =>
    request(
      `/api/history/search${query({ q: q.q, repo: q.repo, org: q.org, agent: q.agent, status: q.status, since: q.since, until: q.until, limit: q.limit?.toString() })}`,
    ),
};
