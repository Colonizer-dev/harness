// Mothership host, settings & logins API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, post, put, request } from "../../http";
import type { LoginView } from "../sessions/types";
import type { ArchiveListing, HarnessStatus, HeadroomStatus, LoginItemStatus, PullStatus, RetentionPlan, RetentionRequest, StorageSummary, RestartOnNewVersion, SetupState, TelemetryStatus, UpdateStatus, UsageStatus } from "./types";

export interface HostApi {
  /**
   * GET /api/status. The mothership serves plain polls from a short-TTL cache; `fresh` asks it to
   * re-probe (`?fresh=1`), which is what Setup's "Check again" uses.
   */
  status(fresh?: boolean): Promise<HarnessStatus>;
  sandboxPull(): Promise<PullStatus>;
  sandboxPullStatus(): Promise<PullStatus>;
  headroom(): Promise<HeadroomStatus>;
  headroomDownload(): Promise<HeadroomStatus>;
  telemetry(): Promise<TelemetryStatus>;
  /** GET /api/setup: the advisory Setup rows marked "don't ask again" on this host. */
  setupState(): Promise<SetupState>;
  /** PUT /api/setup: dismiss (or bring back) one advisory row, kept server-side. */
  setSetupDismissed(id: string, dismissed: boolean): Promise<SetupState>;
  update(): Promise<UpdateStatus>;
  setUpdateCheck(enabled: boolean): Promise<UpdateStatus>;
  applyUpdate(): Promise<{ started: boolean }>;
  /**
   * POST /api/update/restart (issue #1097): stop and resume colonies still on a previous version's
   * components, by id or all of them, so they boot on this one. Runs in the background.
   */
  restartOnNewVersion(body: { ids: string[] } | { all: true }): Promise<RestartOnNewVersion>;
  setTelemetry(enabled: boolean): Promise<TelemetryStatus>;
  usage(): Promise<UsageStatus>;
  setUsage(enabled: boolean): Promise<UsageStatus>;
  /** GET /api/login-item. */
  loginItem(): Promise<LoginItemStatus>;
  /** POST /api/login-item: start the mothership at login, or stop doing so (never stops a running one). */
  setLoginItem(enabled: boolean): Promise<LoginItemStatus>;
  setGithubToken(token: string): Promise<{ login: string }>;
  deleteGithubToken(): Promise<unknown>;
  setClaudeToken(token: string): Promise<unknown>;
  deleteClaudeToken(): Promise<unknown>;
  claudeLogin(): Promise<LoginView>;
  claudeLoginStart(): Promise<LoginView>;
  claudeLoginCode(code: string): Promise<LoginView>;
  claudeLoginCancel(): Promise<LoginView>;
  /** GET /api/storage: disk usage plus the reclaimable / unpushed / orphan breakdown (issue #223). */
  storageSummary(): Promise<StorageSummary>;
  /** GET /api/archive (issue #496): the archived colony logs under `<data_dir>/archive`. */
  archive(): Promise<ArchiveListing>;
  /** POST /api/archive/retention (issue #496): preview (`dry_run`) or apply an archive cleanup pass. */
  archiveRetention(body: RetentionRequest): Promise<RetentionPlan>;
}

export const hostHttp: HostApi = {
  status: (fresh) => request(fresh ? "/api/status?fresh=1" : "/api/status"),
  sandboxPull: () => post("/api/sandbox/pull"),
  sandboxPullStatus: () => request("/api/sandbox/pull"),
  headroom: () => request("/api/headroom"),
  headroomDownload: () => post("/api/headroom/download"),
  telemetry: () => request("/api/telemetry"),
  setupState: () => request("/api/setup"),
  setSetupDismissed: (id, dismissed) => put("/api/setup", { id, dismissed }),
  update: () => request("/api/update"),
  setUpdateCheck: (enabled) => put("/api/update", { enabled }),
  applyUpdate: () => post("/api/update/apply"),
  restartOnNewVersion: (body) => post("/api/update/restart", body),
  setTelemetry: (enabled) => put("/api/telemetry", { enabled }),
  usage: () => request("/api/telemetry/usage"),
  setUsage: (enabled) => put("/api/telemetry/usage", { enabled }),
  loginItem: () => request("/api/login-item"),
  setLoginItem: (enabled) => post("/api/login-item", { enabled }),
  setGithubToken: (token) => post("/api/settings/github-token", { token }),
  deleteGithubToken: () => del("/api/settings/github-token"),
  setClaudeToken: (token) => post("/api/settings/claude-token", { token }),
  deleteClaudeToken: () => del("/api/settings/claude-token"),
  claudeLogin: () => request("/api/claude-login"),
  claudeLoginStart: () => post("/api/claude-login/start"),
  claudeLoginCode: (code) => post("/api/claude-login/code", { code }),
  claudeLoginCancel: () => post("/api/claude-login/cancel"),
  storageSummary: () => request("/api/storage"),
  archive: () => request("/api/archive"),
  archiveRetention: (body) => post("/api/archive/retention", body),
};
