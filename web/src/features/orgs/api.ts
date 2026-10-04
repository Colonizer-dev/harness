// Org workspaces & activity API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { enc, put, query, request } from "../../http";
import type { ActivityPage, ActivityQuery, OrgInfo, OrgSettings, SpendHistory } from "./types";

export interface OrgsApi {
  orgs(): Promise<OrgInfo[]>;
  /** GET /api/spend/history: per-org daily totals for the last `days` (default 30); the overview's sparklines (issue #209). */
  spendHistory(days?: number): Promise<SpendHistory>;
  /** GET /api/activity: the activity log newest first, one page at a time (docs/protocol.md §6.9). */
  activity(query?: ActivityQuery): Promise<ActivityPage>;
  /** Returns `{org, settings}`; colony and memory counts come from the next `orgs()`. */
  saveOrg(org: string, settings: OrgSettings): Promise<Pick<OrgInfo, "org" | "settings">>;
}

export const orgsHttp: OrgsApi = {
  orgs: () => request("/api/orgs"),
  spendHistory: (days) => request(`/api/spend/history?days=${days ?? 30}`),
  activity: (q = {}) =>
    request(
      `/api/activity${query({ before: q.before?.toString(), limit: q.limit?.toString(), kind: q.kind, actor: q.actor, org: q.org, repo: q.repo, q: q.q })}`,
    ),
  saveOrg: (org, settings) => put(`/api/orgs/${enc(org)}`, { settings }),
};
