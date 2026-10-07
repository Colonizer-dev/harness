// Org workspaces & activity API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { enc, post, put, query, request } from "../../http";
import type { ActivityPage, ActivityQuery, MergeStewardInfo, OrgInfo, OrgSettings, SpendHistory } from "./types";

export interface OrgsApi {
  orgs(): Promise<OrgInfo[]>;
  /** GET /api/spend/history: per-org daily totals for the last `days` (default 30); the overview's sparklines (issue #209). */
  spendHistory(days?: number, tzOffsetMinutes?: number): Promise<SpendHistory>;
  /** GET /api/activity: the activity log newest first, one page at a time (docs/protocol.md §6.9). */
  activity(query?: ActivityQuery): Promise<ActivityPage>;
  /** Returns `{org, settings}`; colony and memory counts come from the next `orgs()`. */
  saveOrg(org: string, settings: OrgSettings): Promise<Pick<OrgInfo, "org" | "settings">>;
  /** GET /api/merge-steward: the pull requests colonies opened, per org, and what the merge steward is doing about each (issue #1172). */
  mergeSteward(): Promise<MergeStewardInfo>;
  /** POST /api/merge-steward/merge: merges one colony's pull request now; refused unless GitHub calls it mergeable. */
  mergeNow(url: string): Promise<{ merged: boolean; message: string }>;
}

export const orgsHttp: OrgsApi = {
  orgs: () => request("/api/orgs"),
  spendHistory: (days, tzOffsetMinutes) =>
    request(`/api/spend/history?days=${days ?? 30}${tzOffsetMinutes != null ? `&tz_offset_minutes=${tzOffsetMinutes}` : ""}`),
  activity: (q = {}) =>
    request(
      `/api/activity${query({ before: q.before?.toString(), limit: q.limit?.toString(), kind: q.kind, actor: q.actor, org: q.org, repo: q.repo, q: q.q })}`,
    ),
  saveOrg: (org, settings) => put(`/api/orgs/${enc(org)}`, { settings }),
  mergeSteward: () => request("/api/merge-steward"),
  mergeNow: (url) => post("/api/merge-steward/merge", { url }),
};
