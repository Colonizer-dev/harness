// Living Brain's API methods (crates/colonizer/src/living_brain). The root `Api` interface composes `LivingBrainApi`.
import { del, enc, post, put, request } from "../../http";
import type { LivingBrainBackfill, LivingBrainColony, LivingBrainQueue, LivingBrainRule, LivingBrainState } from "./types";

export interface LivingBrainApi {
  /** GET /api/living-brain: the connection, the rules and the queue — never the token. */
  livingBrain(): Promise<LivingBrainState>;
  /** PUT /api/living-brain: the server and MCP addresses (409 while connected) and/or the rules, replaced whole. */
  saveLivingBrain(body: { server_url?: string; mcp_url?: string; rules?: LivingBrainRule[] }): Promise<LivingBrainState>;
  /** POST /api/living-brain/connect: starts the device sign-in; the answer's `device` has the code and the page to open. */
  connectLivingBrain(): Promise<LivingBrainState>;
  /** DELETE /api/living-brain/connect: forgets the sign-in in flight. */
  cancelLivingBrainConnect(): Promise<LivingBrainState>;
  /** PUT /api/living-brain/token: a personal access token from the brain's Settings → API tokens; 400 when the brain refuses it. */
  setLivingBrainToken(token: string): Promise<LivingBrainState>;
  /** DELETE /api/living-brain/connection: revokes the token at the brain and forgets it here. */
  disconnectLivingBrain(): Promise<LivingBrainState>;
  /** POST /api/living-brain/queue/retry: every failed colony back to pending. */
  retryLivingBrain(): Promise<{ retried: number; queue: LivingBrainQueue }>;
  /** POST /api/living-brain/backfill: an org's existing colonies; `dry_run` only counts. */
  backfillLivingBrain(org: string, dryRun: boolean): Promise<LivingBrainBackfill>;
  /** GET /api/living-brain/colonies/{id}: what became of one colony, and its page at the brain. */
  livingBrainColony(id: string): Promise<LivingBrainColony>;
}

export const livingBrainHttp: LivingBrainApi = {
  livingBrain: () => request("/api/living-brain"),
  saveLivingBrain: (body) => put("/api/living-brain", body),
  connectLivingBrain: () => post("/api/living-brain/connect"),
  cancelLivingBrainConnect: () => del("/api/living-brain/connect"),
  setLivingBrainToken: (token) => put("/api/living-brain/token", { token }),
  disconnectLivingBrain: () => del("/api/living-brain/connection"),
  retryLivingBrain: () => post("/api/living-brain/queue/retry"),
  backfillLivingBrain: (org, dryRun) => post("/api/living-brain/backfill", { org, dry_run: dryRun }),
  livingBrainColony: (id) => request(`/api/living-brain/colonies/${enc(id)}`),
};
