// Queues API — split out of src/api.ts (issue #1127).
// The root `Api` interface composes this with the other features.
import { query, request } from "../../http";
import type { QueuesFilters, QueuesPayload } from "./types";

export interface QueuesApi {
  /**
   * GET /api/queues: every colony waiting to start, why each one waits, which actions the server
   * would take on it, and how full every host is. The filters narrow the rows server-side (`group`
   * is the view's own and never sent); empty ones are dropped.
   */
  queues(filters?: QueuesFilters): Promise<QueuesPayload>;
}

export const queuesHttp: QueuesApi = {
  queues: (filters) =>
    request(`/api/queues${query({ host: filters?.host, reason: filters?.reason, repo: filters?.repo, agent: filters?.agent, q: filters?.q })}`),
};
