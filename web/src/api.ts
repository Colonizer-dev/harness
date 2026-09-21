// Typed client for the harness browser API (docs/protocol.md §4, §6.3).
import type {
  FleetHost,
  HarnessStatus,
  HeadroomStatus,
  Issue,
  LoginView,
  Mem0Check,
  Mem0Status,
  MemoryListing,
  MemoryNote,
  MemoryProposal,
  MemoryScope,
  ModelOption,
  ModelProvider,
  ModuleInfo,
  NewNoteRequest,
  NewSessionRequest,
  OrgInfo,
  OrgSettings,
  PluginListing,
  ProviderHealth,
  PullStatus,
  Repo,
  SaveProviderRequest,
  Session,
  TelemetryStatus,
  UpdateStatus,
  UsageStatus,
} from "./types";

/** The part of the WebSocket interface the UI uses, so the mock can stand in for it. */
export type SocketLike = Pick<
  WebSocket,
  "binaryType" | "readyState" | "onopen" | "onmessage" | "onclose" | "onerror" | "send" | "close"
>;

export const SOCKET_OPEN = 1;

export class ApiError extends Error {
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

export interface SaveModuleRequest {
  provider: string;
  enabled: boolean;
  settings: Record<string, unknown>;
}

export interface Api {
  readonly mock: boolean;
  /**
   * GET /api/status. The mothership serves plain polls from a short-TTL cache; `fresh` asks it to
   * re-probe (`?fresh=1`), which is what Setup's "Check again" uses.
   */
  status(fresh?: boolean): Promise<HarnessStatus>;
  /** GET /api/hosts (issue #231): self plus every peer configured via COLONIZER_FLEET_PEERS, polled live on each call. */
  hosts(): Promise<{ hosts: FleetHost[] }>;
  modules(): Promise<ModuleInfo[]>;
  saveModule(kind: string, body: SaveModuleRequest): Promise<ModuleInfo>;
  sandboxPull(): Promise<PullStatus>;
  sandboxPullStatus(): Promise<PullStatus>;
  headroom(): Promise<HeadroomStatus>;
  headroomDownload(): Promise<HeadroomStatus>;
  telemetry(): Promise<TelemetryStatus>;
  update(): Promise<UpdateStatus>;
  setUpdateCheck(enabled: boolean): Promise<UpdateStatus>;
  applyUpdate(): Promise<{ started: boolean }>;
  setTelemetry(enabled: boolean): Promise<TelemetryStatus>;
  usage(): Promise<UsageStatus>;
  setUsage(enabled: boolean): Promise<UsageStatus>;
  repos(): Promise<Repo[]>;
  issues(repo: string): Promise<Issue[]>;
  sessions(): Promise<Session[]>;
  session(id: string): Promise<Session>;
  createSession(body: NewSessionRequest): Promise<Session>;
  publishSession(id: string): Promise<Session>;
  resumeSession(id: string): Promise<Session>;
  stopSession(id: string): Promise<Session>;
  cleanupSession(id: string): Promise<Session>;
  /** Forgets a colony: worktree, local branch, chat and logs. Its pull request stays on GitHub. */
  deleteSession(id: string): Promise<unknown>;
  setGithubToken(token: string): Promise<{ login: string }>;
  deleteGithubToken(): Promise<unknown>;
  setClaudeToken(token: string): Promise<unknown>;
  deleteClaudeToken(): Promise<unknown>;
  claudeLogin(): Promise<LoginView>;
  claudeLoginStart(): Promise<LoginView>;
  claudeLoginCode(code: string): Promise<LoginView>;
  claudeLoginCancel(): Promise<LoginView>;
  plugins(): Promise<PluginListing>;
  providers(): Promise<ModelProvider[]>;
  saveProvider(id: string, body: SaveProviderRequest): Promise<ModelProvider>;
  deleteProvider(id: string): Promise<unknown>;
  /** Probes the provider from the Mothership; can take ~5 s. */
  providerHealth(id: string): Promise<ProviderHealth>;
  models(): Promise<ModelOption[]>;
  orgs(): Promise<OrgInfo[]>;
  /** Returns `{org, settings}`; colony and memory counts come from the next `orgs()`. */
  saveOrg(org: string, settings: OrgSettings): Promise<Pick<OrgInfo, "org" | "settings">>;
  memory(scope: MemoryScope, key: string): Promise<MemoryListing>;
  memoryProposals(): Promise<MemoryProposal[]>;
  approveProposal(id: string, edits?: { title?: string; content?: string }): Promise<MemoryNote>;
  rejectProposal(id: string): Promise<unknown>;
  createNote(body: NewNoteRequest): Promise<MemoryNote>;
  deleteNote(note: Pick<MemoryNote, "id" | "scope" | "key">): Promise<unknown>;
  mem0Status(): Promise<Mem0Status>;
  /** Saves the key on the Mothership; an empty string removes it. */
  saveMem0Key(apiKey: string): Promise<Mem0Status>;
  /** Tries the saved key against the configured endpoint. */
  checkMem0(): Promise<Mem0Check>;
  openEvents(sessionId: string, since: number): SocketLike;
  openTerminal(sessionId: string, cols: number, rows: number): SocketLike;
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: { "content-type": "application/json", ...(init.headers ?? {}) },
  });
  const text = await res.text();
  let data: unknown = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    data = text;
  }
  if (!res.ok) {
    const message =
      data && typeof data === "object" && "error" in data
        ? String((data as { error: unknown }).error)
        : text || res.statusText;
    throw new ApiError(message, res.status);
  }
  return data as T;
}

const post = <T>(path: string, body?: unknown) =>
  request<T>(path, { method: "POST", body: body === undefined ? undefined : JSON.stringify(body) });

const put = <T>(path: string, body: unknown) => request<T>(path, { method: "PUT", body: JSON.stringify(body) });

const del = <T>(path: string) => request<T>(path, { method: "DELETE" });

const enc = encodeURIComponent;

function wsUrl(path: string): string {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${location.host}${path}`;
}

export const httpApi: Api = {
  mock: false,
  status: (fresh) => request(fresh ? "/api/status?fresh=1" : "/api/status"),
  hosts: () => request("/api/hosts"),
  modules: () => request("/api/modules"),
  saveModule: (kind, body) => put(`/api/modules/${enc(kind)}`, body),
  sandboxPull: () => post("/api/sandbox/pull"),
  sandboxPullStatus: () => request("/api/sandbox/pull"),
  headroom: () => request("/api/headroom"),
  headroomDownload: () => post("/api/headroom/download"),
  telemetry: () => request("/api/telemetry"),
  update: () => request("/api/update"),
  setUpdateCheck: (enabled) => put("/api/update", { enabled }),
  applyUpdate: () => post("/api/update/apply"),
  setTelemetry: (enabled) => put("/api/telemetry", { enabled }),
  usage: () => request("/api/telemetry/usage"),
  setUsage: (enabled) => put("/api/telemetry/usage", { enabled }),
  repos: () => request("/api/repos"),
  issues: (repo) => {
    const [owner, name] = repo.split("/");
    return request(`/api/repos/${enc(owner)}/${enc(name)}/issues`);
  },
  sessions: () => request("/api/sessions"),
  session: (id) => request(`/api/sessions/${enc(id)}`),
  createSession: (body) => post("/api/sessions", body),
  publishSession: (id) => post(`/api/sessions/${enc(id)}/publish`),
  resumeSession: (id) => post(`/api/sessions/${enc(id)}/resume`),
  stopSession: (id) => post(`/api/sessions/${enc(id)}/stop`),
  cleanupSession: (id) => post(`/api/sessions/${enc(id)}/cleanup`),
  deleteSession: (id) => del(`/api/sessions/${enc(id)}`),
  setGithubToken: (token) => post("/api/settings/github-token", { token }),
  deleteGithubToken: () => del("/api/settings/github-token"),
  setClaudeToken: (token) => post("/api/settings/claude-token", { token }),
  deleteClaudeToken: () => del("/api/settings/claude-token"),
  claudeLogin: () => request("/api/claude-login"),
  claudeLoginStart: () => post("/api/claude-login/start"),
  claudeLoginCode: (code) => post("/api/claude-login/code", { code }),
  claudeLoginCancel: () => post("/api/claude-login/cancel"),
  plugins: () => request("/api/plugins"),
  providers: () => request("/api/providers"),
  saveProvider: (id, body) => put(`/api/providers/${enc(id)}`, body),
  deleteProvider: (id) => del(`/api/providers/${enc(id)}`),
  providerHealth: (id) => request(`/api/providers/${enc(id)}/health`),
  models: () => request("/api/models"),
  orgs: () => request("/api/orgs"),
  saveOrg: (org, settings) => put(`/api/orgs/${enc(org)}`, { settings }),
  memory: (scope, key) => request(`/api/memory?scope=${enc(scope)}&key=${enc(key)}`),
  memoryProposals: () => request("/api/memory/proposals"),
  approveProposal: (id, edits) => post(`/api/memory/proposals/${enc(id)}/approve`, edits ?? {}),
  rejectProposal: (id) => post(`/api/memory/proposals/${enc(id)}/reject`),
  createNote: (body) => post("/api/memory/notes", body),
  deleteNote: ({ id, scope, key }) => del(`/api/memory/notes/${enc(id)}?scope=${enc(scope)}&key=${enc(key)}`),
  mem0Status: () => request("/api/memory/mem0"),
  saveMem0Key: (apiKey) => put("/api/memory/mem0", { api_key: apiKey }),
  checkMem0: () => post("/api/memory/mem0/check"),
  openEvents: (id, since) => new WebSocket(wsUrl(`/api/sessions/${enc(id)}/events?since=${since}`)),
  openTerminal: (id, cols, rows) =>
    new WebSocket(wsUrl(`/api/sessions/${enc(id)}/terminal?cols=${cols}&rows=${rows}`)),
};

/** `?mock=1` swaps in an in-browser backend so the UI can be exercised without a harness. */
export async function loadApi(): Promise<Api> {
  if (new URLSearchParams(location.search).get("mock") === "1") {
    const { createMockApi } = await import("./mock");
    return createMockApi();
  }
  return httpApi;
}
