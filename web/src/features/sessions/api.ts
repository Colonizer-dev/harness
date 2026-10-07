// Colonies (sessions) API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, enc, post, query, request, wsUrl } from "../../http";
import type { SocketLike } from "../../http";
import { outboxId } from "../../outbox";
import type { BurnDownStatus, CommitLink, FindingRecord, NewSessionRequest, Session, SessionDiff } from "./types";

/** GET /api/sessions/{id}/behind: how far the colony branch lags origin/{base} (issue #173). */
export interface BehindInfo {
  behind_by: number | null;
  base: string | null;
  branch: string;
}

/** POST /api/sessions/{id}/catch-up: merging origin/{base} into the colony branch (issue #173). */
export interface CatchUpResult {
  session: Session;
  merged: boolean;
  conflicts: string[];
  behind_by: number | null;
}

/**
 * POST /api/sessions/{id}/stop: the colony plus what the stop did. `already_stopped` is a 200 like
 * `stopped` — the colony was already over — so a retried or stale stop lands as a success.
 */
export type StopReply = Session & { result: "stopped" | "already_stopped" };

export interface SessionsApi {
  sessions(): Promise<Session[]>;
  session(id: string): Promise<Session>;
  /** The colony's finding ledger, in the order it was written (an append-only record per finding stage). */
  findings(id: string): Promise<FindingRecord[]>;
  /** GET /api/sessions/{id}/commits (issue #765): the colony's recorded commits, oldest first. */
  sessionCommits(id: string): Promise<{ commits: CommitLink[] }>;
  /** GET /api/sessions/{id}/diff (issue #611): the colony's changed files with per-file +/- counts, plus the unified diff. */
  sessionDiff(id: string): Promise<SessionDiff>;
  createSession(body: NewSessionRequest): Promise<Session>;
  publishSession(id: string): Promise<Session>;
  resumeSession(id: string): Promise<Session>;
  /**
   * POST /api/sessions/{id}/messages: send the colony's agent a message over HTTP, deduped on a fresh
   * client id. The cockpit uses it where no chat stream is open — Retry on a colony held on gateway
   * errors (issue #1093). 409 when the colony cannot take a message.
   */
  messageSession(id: string, text: string): Promise<{ id: string; duplicate: boolean }>;
  stopSession(id: string): Promise<StopReply>;
  /** POST /api/sessions/{id}/priority (issue #1156): a queued colony's own queue priority, higher first; null follows its org again. 409 when it is not queued. */
  setSessionPriority(id: string, priority: number | null): Promise<Session>;
  /** POST /api/sessions/{id}/move-to-front or move-to-back (issue #1156): just ahead of or behind every other queued colony. 409 when it is not queued. */
  moveSession(id: string, to: "front" | "back"): Promise<Session>;
  /** POST /api/sessions/{id}/prewarm (issue #701): boot a suspended colony's question ahead of its answer. Answers 202 when requested, 204 when it is a no-op. */
  prewarmSession(id: string): Promise<unknown>;
  /** POST /api/sessions/{id}/keep (issue #673): release a superseded colony to start again. 409 when it is not superseded. */
  keepSession(id: string): Promise<Session>;
  cleanupSession(id: string): Promise<Session>;
  /** POST /api/sessions/{id}/seen: the colony was looked at — clears `unseen_failure` and has the mothership push "resolved" to every device (issue #744). */
  seenSession(id: string): Promise<void>;
  /** POST /api/sessions/{id}/retain: keep (`{keep: true}`) or release this colony's worktree from automatic reclamation. */
  setKeep(id: string, keep: boolean): Promise<Session>;
  /**
   * Forgets a colony: worktree, local branch, chat and logs. Its pull request stays on GitHub.
   * The logs are archived into `<data_dir>/archive` first (issue #496); `purgeLogs` deletes that
   * archived bundle too, instead of keeping it.
   */
  deleteSession(id: string, opts?: { purgeLogs?: boolean }): Promise<unknown>;
  /** GET /api/sessions/{id}/behind: how far the colony branch lags origin/{base} (issue #173). */
  behindSession(id: string): Promise<BehindInfo>;
  /** POST /api/sessions/{id}/catch-up: merge origin/{base} into the colony branch (issue #173). */
  catchUpSession(id: string): Promise<CatchUpResult>;
  /** GET /api/burn-down: the burn-down scheduler's read on the weekly token plan (issue #210). */
  burnDown(): Promise<BurnDownStatus>;
  /** POST /api/burn-down/stop: switches the scheduler off and stops every colony it launched. */
  stopBurnDown(): Promise<void>;
  openTerminal(sessionId: string, cols: number, rows: number): SocketLike;
}

export const sessionsHttp: SessionsApi = {
  sessions: () => request("/api/sessions"),
  session: (id) => request(`/api/sessions/${enc(id)}`),
  findings: (id) => request(`/api/sessions/${enc(id)}/findings`),
  sessionCommits: (id) => request(`/api/sessions/${enc(id)}/commits`),
  sessionDiff: (id) => request(`/api/sessions/${enc(id)}/diff`),
  createSession: (body) => post("/api/sessions", body),
  publishSession: (id) => post(`/api/sessions/${enc(id)}/publish`),
  resumeSession: (id) => post(`/api/sessions/${enc(id)}/resume`),
  messageSession: (id, text) => post(`/api/sessions/${enc(id)}/messages`, { id: outboxId(), text }),
  stopSession: (id) => post(`/api/sessions/${enc(id)}/stop`),
  setSessionPriority: (id, priority) => post(`/api/sessions/${enc(id)}/priority`, { priority }),
  moveSession: (id, to) => post(`/api/sessions/${enc(id)}/move-to-${to}`),
  prewarmSession: (id) => post(`/api/sessions/${enc(id)}/prewarm`),
  keepSession: (id) => post(`/api/sessions/${enc(id)}/keep`),
  cleanupSession: (id) => post(`/api/sessions/${enc(id)}/cleanup`),
  seenSession: (id) => post(`/api/sessions/${enc(id)}/seen`),
  setKeep: (id, keep) => post(`/api/sessions/${enc(id)}/retain`, { keep }),
  deleteSession: (id, opts) => del(`/api/sessions/${enc(id)}${query({ purge_logs: opts?.purgeLogs ? "true" : undefined })}`),
  behindSession: (id) => request(`/api/sessions/${enc(id)}/behind`),
  catchUpSession: (id) => post(`/api/sessions/${enc(id)}/catch-up`),
  burnDown: () => request("/api/burn-down"),
  stopBurnDown: () => post("/api/burn-down/stop"),
  openTerminal: (id, cols, rows) =>
    new WebSocket(wsUrl(`/api/sessions/${enc(id)}/terminal?cols=${cols}&rows=${rows}`)),
};
