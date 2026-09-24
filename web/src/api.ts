// Typed client for the harness browser API (docs/protocol.md §4, §6.3).
import type {
  Draft,
  EditsRequest,
  FileCommit,
  RepoBlame,
  RepoBlob,
  RepoBranches,
  RepoCoverage,
  RepoGitSummary,
  RepoLoc,
  RepoTree,
  Loop,
  NewLoop,
  ChatMessage,
  ChatMeta,
  ChatModels,
  ChatSendRequest,
  ChatStreamEvent,
  MapFileDetail,
  RepoPackages,
  BurnDownStatus,
  SecretRow,
  ColonySecretRequest,
  SecretsListing,
  RepoMap,
  RepoMeta,
  TouchedFiles,
  FleetHost,
  FindingRecord,
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
  RedTeamRun,
  RedTeamSchedule,
  NewRedTeamSchedule,
  HunterProbe,
  Repo,
  SaveProviderRequest,
  Session,
  SessionStatus,
  SpendHistory,
  StartRedTeamRunRequest,
  StorageSummary,
  TelemetryStatus,
  UpdateStatus,
  UsageStatus,
  VoiceStatus,
  LoginItemStatus,
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

/**
 * Whether a colony in this status holds its issue against a second launch — the cockpit mirror
 * of the mothership's `issue_held_by` (crates/colonizer/src/sessions.rs): queued, live,
 * publishing, or with its pull request still open. Stopped, failed, no_changes, merged and
 * closed leave the issue free for a retry.
 */
export function holdsIssue(status: SessionStatus): boolean {
  return (
    status === "queued" ||
    status === "starting" ||
    status === "running" ||
    status === "waiting_for_answer" ||
    status === "idle" ||
    status === "publishing" ||
    status === "pr_opened"
  );
}

/** The colony already holding `(repo, issue)`, if any — the launch that POST /api/sessions would refuse with a 409. */
export function heldByFor(sessions: Session[], repo: string, issue: number): Session | null {
  return sessions.find((s) => s.repo === repo && s.issue === issue && holdsIssue(s.status)) ?? null;
}

/** The issues of a batch launch another colony already holds, in the order given — each one a 409 waiting to happen. */
export function heldInBatch(sessions: Session[], repo: string, issues: Iterable<number>): number[] {
  return [...issues].filter((issue) => heldByFor(sessions, repo, issue) !== null);
}

export interface SaveModuleRequest {
  provider: string;
  enabled: boolean;
  settings: Record<string, unknown>;
}

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
  /** GET /api/login-item. */
  loginItem(): Promise<LoginItemStatus>;
  /** POST /api/login-item: start the mothership at login, or stop doing so (never stops a running one). */
  setLoginItem(enabled: boolean): Promise<LoginItemStatus>;
  repos(): Promise<Repo[]>;
  issues(repo: string): Promise<Issue[]>;
  /** GET /api/repos/{owner}/{repo}/packages: monorepo detection. */
  repoPackages(repo: string): Promise<RepoPackages>;
  sessions(): Promise<Session[]>;
  session(id: string): Promise<Session>;
  /** The colony's finding ledger, in the order it was written (an append-only record per finding stage). */
  findings(id: string): Promise<FindingRecord[]>;
  createSession(body: NewSessionRequest): Promise<Session>;
  publishSession(id: string): Promise<Session>;
  resumeSession(id: string): Promise<Session>;
  stopSession(id: string): Promise<StopReply>;
  cleanupSession(id: string): Promise<Session>;
  /** GET /api/storage: disk usage plus the reclaimable / unpushed / orphan breakdown (issue #223). */
  storageSummary(): Promise<StorageSummary>;
  /** POST /api/sessions/{id}/retain: keep (`{keep: true}`) or release this colony's worktree from automatic reclamation. */
  setKeep(id: string, keep: boolean): Promise<Session>;
  /** Forgets a colony: worktree, local branch, chat and logs. Its pull request stays on GitHub. */
  deleteSession(id: string): Promise<unknown>;
  /** GET /api/sessions/{id}/behind: how far the colony branch lags origin/{base} (issue #173). */
  behindSession(id: string): Promise<BehindInfo>;
  /** POST /api/sessions/{id}/catch-up: merge origin/{base} into the colony branch (issue #173). */
  catchUpSession(id: string): Promise<CatchUpResult>;
  /** GET /api/burn-down: the burn-down scheduler's read on the weekly token plan (issue #210). */
  burnDown(): Promise<BurnDownStatus>;
  /** POST /api/burn-down/stop: switches the scheduler off and stops every colony it launched. */
  stopBurnDown(): Promise<void>;
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
  /** GET /api/spend/history: per-org daily totals for the last `days` (default 30); the overview's sparklines (issue #209). */
  spendHistory(days?: number): Promise<SpendHistory>;
  /** Returns `{org, settings}`; colony and memory counts come from the next `orgs()`. */
  saveOrg(org: string, settings: OrgSettings): Promise<Pick<OrgInfo, "org" | "settings">>;
  /** GET /api/secrets: every saved secret and where it lives; values never leave the mothership. */
  secrets(): Promise<SecretsListing>;
  /** PUT /api/secrets/{id}: sets or replaces a secret; `location` also moves it there. */
  saveSecret(id: string, value: string, location?: "keychain" | "file"): Promise<SecretRow>;
  /** DELETE /api/secrets/{id}; a colony secret answers `{id, removed}` since its row is gone. */
  deleteSecret(id: string): Promise<SecretRow | { id: string; removed: true }>;
  /** POST /api/secrets/colony: adds a colony secret or changes its hosts, scope or value. */
  saveColonySecret(body: ColonySecretRequest): Promise<{ id: string }>;
  /** POST /api/secrets/{id}/move: between the system keychain and the 0600 file. */
  moveSecret(id: string, to: "keychain" | "file"): Promise<SecretRow>;
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
  /** A repository's architecture map and the newest colony drawing it. */
  repoMap(repo: string): Promise<RepoMap>;
  /** GET /api/repos/{owner}/{repo}/meta: description, languages, weekly commits, contributors. */
  repoMeta(repo: string): Promise<RepoMeta>;
  // The Code page (code.rs), read from the mothership's bare clone.
  repoLoc(repo: string): Promise<RepoLoc>;
  repoCoverage(repo: string): Promise<RepoCoverage>;
  repoGitSummary(repo: string): Promise<RepoGitSummary>;
  repoBranches(repo: string): Promise<RepoBranches>;
  repoTree(repo: string, ref?: string): Promise<RepoTree>;
  repoBlob(repo: string, path: string, ref?: string): Promise<RepoBlob>;
  fileHistory(repo: string, path: string, ref?: string): Promise<{ path: string; ref: string; commits: FileCommit[] }>;
  fileBlame(repo: string, path: string, ref?: string): Promise<RepoBlame>;
  /** Commits edited files to a new branch and opens a pull request; only after an explicit confirm. */
  createEdits(repo: string, body: EditsRequest): Promise<{ url: string; branch: string; base: string }>;
  /** A quick answer about a file from the cheap summary model. */
  askFile(repo: string, body: { path: string; question: string; content: string; selection?: [number, number] | null }): Promise<{ answer: string; model: string }>;
  drafts(repo: string, ref?: string): Promise<{ repo: string; autosave: boolean; drafts: Draft[] }>;
  saveDraft(repo: string, body: { ref: string; path: string; content: string; base_sha: string }): Promise<{ saved_at: string }>;
  deleteDrafts(repo: string, ref: string, path?: string): Promise<{ removed: number }>;
  editorSettings(): Promise<{ autosave: boolean }>;
  saveEditorSettings(body: { autosave: boolean }): Promise<{ autosave: boolean }>;
  /** Chat (docs/protocol.md): direct conversations with a model, stored on the mothership. */
  chats(): Promise<{ chats: ChatMeta[] }>;
  chatModels(): Promise<ChatModels>;
  createChat(body: { title?: string; model?: string; system?: string; max_tokens?: number; workspace?: string }): Promise<ChatMeta>;
  chat(id: string): Promise<{ chat: ChatMeta; messages: ChatMessage[] }>;
  patchChat(id: string, body: Partial<Pick<ChatMeta, "title" | "model" | "system" | "max_tokens" | "workspace">>): Promise<ChatMeta>;
  deleteChat(id: string): Promise<unknown>;
  /** Streams the reply; `onEvent` gets each line; aborting `signal` stops the reply (kept as stopped). */
  sendChat(id: string, body: ChatSendRequest, onEvent: (event: ChatStreamEvent) => void, signal?: AbortSignal): Promise<void>;
  /** GET /api/maps/{owner}/{repo}/files: every file at the map's revision, from the local clone. */
  repoMapFiles(repo: string): Promise<{ repo: string; revision: string; paths: string[]; truncated: boolean }>;
  /** GET /api/maps/{owner}/{repo}/file?path=…: live colonies on one file, their calls on it and their diff. */
  repoMapFile(repo: string, path: string): Promise<MapFileDetail>;
  /** Launches a colony that draws the repository with archify (or returns the one already drawing). */
  mapRepo(repo: string): Promise<RepoMap>;
  /** The files each live colony's worktree has changed. */
  touched(): Promise<TouchedFiles>;
  /** The voice module's active speech-to-text service. */
  voice(): Promise<VoiceStatus>;
  /** Saves a voice service's key on the Mothership; an empty string removes it. */
  saveVoiceKey(provider: string, apiKey: string): Promise<VoiceStatus>;
  /** Sends a recorded clip to the connected service; the Mothership adds the key. */
  transcribe(audio: Blob): Promise<{ text: string; provider: string }>;
  /** Red-team runs: a swarm of hunter colonies raiding one repository (issue #212). 409 without `arm` when any colony is live or a run is already active for the repo. */
  redTeamRuns(): Promise<RedTeamRun[]>;
  startRedTeamRun(body: StartRedTeamRunRequest): Promise<RedTeamRun>;
  stopRedTeamRun(id: string): Promise<RedTeamRun>;
  /** GET /api/loops: the scheduled colonies. */
  loops(): Promise<Loop[]>;
  createLoop(body: NewLoop): Promise<Loop>;
  updateLoop(id: string, body: NewLoop): Promise<Loop>;
  deleteLoop(id: string): Promise<void>;
  /** POST /api/loops/{id}/run-now: start the next run now (409 while the previous run is live). */
  runLoopNow(id: string): Promise<Session>;
  /** GET /api/loops/{id}/runs: the loop's colonies, newest first. */
  loopRuns(id: string): Promise<Session[]>;
  redTeamSchedules(): Promise<RedTeamSchedule[]>;
  createRedTeamSchedule(body: NewRedTeamSchedule): Promise<RedTeamSchedule>;
  updateRedTeamSchedule(id: string, body: NewRedTeamSchedule): Promise<RedTeamSchedule>;
  deleteRedTeamSchedule(id: string): Promise<void>;
  probeHunter(id: string): Promise<HunterProbe>;
  openEvents(sessionId: string, since: number, epoch?: number): SocketLike;
  openTerminal(sessionId: string, cols: number, rows: number): SocketLike;
  /** GET /api/stream: the dashboard's realtime feed (issue #446); same-origin cookie auth, like openEvents. */
  openStream(): SocketLike;
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
const repoPath = (repo: string) => `/api/repos/${repo.split("/").map(encodeURIComponent).join("/")}`;
/** `?a=1&b=2` from the defined values, or "". */
const query = (params: Record<string, string | undefined | null>) => {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v != null && v !== "") q.set(k, v);
  const s = q.toString();
  return s ? `?${s}` : "";
};

const enc = encodeURIComponent;

/** Splits newline-delimited JSON: the complete lines parsed, and the unfinished tail to carry over. */
export function splitNdjson(buffer: string): { events: ChatStreamEvent[]; rest: string } {
  const lines = buffer.split("\n");
  const rest = lines.pop() ?? "";
  const events: ChatStreamEvent[] = [];
  for (const line of lines) {
    if (!line.trim()) continue;
    try {
      events.push(JSON.parse(line) as ChatStreamEvent);
    } catch {
      /* a torn line: skipped */
    }
  }
  return { events, rest };
}

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
  loginItem: () => request("/api/login-item"),
  setLoginItem: (enabled) => post("/api/login-item", { enabled }),
  repos: () => request("/api/repos"),
  issues: (repo) => {
    const [owner, name] = repo.split("/");
    return request(`/api/repos/${enc(owner)}/${enc(name)}/issues`);
  },
  repoPackages: (repo) => {
    const [owner, name] = repo.split("/");
    return request(`/api/repos/${enc(owner)}/${enc(name)}/packages`);
  },
  sessions: () => request("/api/sessions"),
  session: (id) => request(`/api/sessions/${enc(id)}`),
  findings: (id) => request(`/api/sessions/${enc(id)}/findings`),
  createSession: (body) => post("/api/sessions", body),
  publishSession: (id) => post(`/api/sessions/${enc(id)}/publish`),
  resumeSession: (id) => post(`/api/sessions/${enc(id)}/resume`),
  stopSession: (id) => post(`/api/sessions/${enc(id)}/stop`),
  cleanupSession: (id) => post(`/api/sessions/${enc(id)}/cleanup`),
  storageSummary: () => request("/api/storage"),
  setKeep: (id, keep) => post(`/api/sessions/${enc(id)}/retain`, { keep }),
  deleteSession: (id) => del(`/api/sessions/${enc(id)}`),
  behindSession: (id) => request(`/api/sessions/${enc(id)}/behind`),
  catchUpSession: (id) => post(`/api/sessions/${enc(id)}/catch-up`),
  burnDown: () => request("/api/burn-down"),
  stopBurnDown: () => post("/api/burn-down/stop"),
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
  spendHistory: (days) => request(`/api/spend/history?days=${days ?? 30}`),
  saveOrg: (org, settings) => put(`/api/orgs/${enc(org)}`, { settings }),
  secrets: () => request("/api/secrets"),
  saveSecret: (id, value, location) => put(`/api/secrets/${enc(id)}`, location ? { value, location } : { value }),
  deleteSecret: (id) => del(`/api/secrets/${enc(id)}`),
  saveColonySecret: (body) => post("/api/secrets/colony", body),
  moveSecret: (id, to) => post(`/api/secrets/${enc(id)}/move`, { to }),
  memory: (scope, key) => request(`/api/memory?scope=${enc(scope)}&key=${enc(key)}`),
  memoryProposals: () => request("/api/memory/proposals"),
  approveProposal: (id, edits) => post(`/api/memory/proposals/${enc(id)}/approve`, edits ?? {}),
  rejectProposal: (id) => post(`/api/memory/proposals/${enc(id)}/reject`),
  createNote: (body) => post("/api/memory/notes", body),
  deleteNote: ({ id, scope, key }) => del(`/api/memory/notes/${enc(id)}?scope=${enc(scope)}&key=${enc(key)}`),
  mem0Status: () => request("/api/memory/mem0"),
  saveMem0Key: (apiKey) => put("/api/memory/mem0", { api_key: apiKey }),
  checkMem0: () => post("/api/memory/mem0/check"),
  repoMap: (repo) => request(`/api/maps/${repo.split("/").map(enc).join("/")}`),
  repoMeta: (repo) => request(`/api/repos/${repo.split("/").map(enc).join("/")}/meta`),
  repoLoc: (repo) => request(`${repoPath(repo)}/loc`),
  repoCoverage: (repo) => request(`${repoPath(repo)}/coverage`),
  repoGitSummary: (repo) => request(`${repoPath(repo)}/git-summary`),
  repoBranches: (repo) => request(`${repoPath(repo)}/branches`),
  repoTree: (repo, ref) => request(`${repoPath(repo)}/tree${query({ ref })}`),
  repoBlob: (repo, path, ref) => request(`${repoPath(repo)}/blob${query({ path, ref })}`),
  fileHistory: (repo, path, ref) => request(`${repoPath(repo)}/history${query({ path, ref })}`),
  fileBlame: (repo, path, ref) => request(`${repoPath(repo)}/blame${query({ path, ref })}`),
  createEdits: (repo, body) => post(`${repoPath(repo)}/edits`, body),
  askFile: (repo, body) => post(`${repoPath(repo)}/ask`, body),
  drafts: (repo, ref) => request(`${repoPath(repo)}/drafts${query({ ref })}`),
  saveDraft: (repo, body) => put(`${repoPath(repo)}/drafts`, body),
  deleteDrafts: (repo, ref, path) => del(`${repoPath(repo)}/drafts${query({ ref, path })}`),
  editorSettings: () => request("/api/editor/settings"),
  saveEditorSettings: (body) => put("/api/editor/settings", body),
  chats: () => request("/api/chat"),
  chatModels: () => request("/api/chat/models"),
  createChat: (body) => post("/api/chat", body),
  chat: (id) => request(`/api/chat/${enc(id)}`),
  patchChat: (id, body) => request(`/api/chat/${enc(id)}`, { method: "PATCH", body: JSON.stringify(body) }),
  deleteChat: (id) => del(`/api/chat/${enc(id)}`),
  sendChat: async (id, body, onEvent, signal) => {
    const res = await fetch(`/api/chat/${enc(id)}/messages`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      signal,
    });
    if (!res.ok || !res.body) {
      const text = await res.text().catch(() => "");
      let message = text || res.statusText;
      try {
        message = String((JSON.parse(text) as { error?: unknown }).error ?? message);
      } catch {
        /* not JSON */
      }
      throw new ApiError(message, res.status);
    }
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let rest = "";
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      const split = splitNdjson(rest + decoder.decode(value, { stream: true }));
      rest = split.rest;
      for (const event of split.events) onEvent(event);
    }
    for (const event of splitNdjson(rest + "\n").events) onEvent(event);
  },
  repoMapFiles: (repo) => request(`/api/maps/${repo.split("/").map(enc).join("/")}/files`),
  repoMapFile: (repo, path) => request(`/api/maps/${repo.split("/").map(enc).join("/")}/file?path=${encodeURIComponent(path)}`),
  mapRepo: (repo) => post(`/api/maps/${repo.split("/").map(enc).join("/")}`),
  touched: () => request("/api/touched"),
  voice: () => request("/api/voice"),
  saveVoiceKey: (provider, apiKey) => put("/api/voice/key", { provider, api_key: apiKey }),
  transcribe: (audio) =>
    // The raw clip as the body, typed by what MediaRecorder produced (audio/webm;codecs=opus in Chrome).
    request("/api/voice/transcribe", { method: "POST", body: audio, headers: { "content-type": audio.type || "audio/webm" } }),
  redTeamRuns: () => request("/api/redteam/runs"),
  startRedTeamRun: (body) => post("/api/redteam/runs", body),
  stopRedTeamRun: (id) => post(`/api/redteam/runs/${enc(id)}/stop`),
  loops: () => request("/api/loops"),
  createLoop: (body) => post("/api/loops", body),
  updateLoop: (id, body) => put(`/api/loops/${enc(id)}`, body),
  deleteLoop: (id) => del(`/api/loops/${enc(id)}`),
  runLoopNow: (id) => post(`/api/loops/${enc(id)}/run-now`),
  loopRuns: (id) => request(`/api/loops/${enc(id)}/runs`),
  redTeamSchedules: () => request("/api/redteam/schedules"),
  createRedTeamSchedule: (body) => post("/api/redteam/schedules", body),
  updateRedTeamSchedule: (id, body) => put(`/api/redteam/schedules/${enc(id)}`, body),
  deleteRedTeamSchedule: (id) => del(`/api/redteam/schedules/${enc(id)}`),
  probeHunter: (id) => request(`/api/hunters/${enc(id)}/probe`),
  openEvents: (id, since, epoch = 0) => new WebSocket(wsUrl(`/api/sessions/${enc(id)}/events?since=${since}&epoch=${epoch}`)),
  openTerminal: (id, cols, rows) =>
    new WebSocket(wsUrl(`/api/sessions/${enc(id)}/terminal?cols=${cols}&rows=${rows}`)),
  openStream: () => new WebSocket(wsUrl("/api/stream")),
};

/** `?mock=1` swaps in an in-browser backend so the UI can be exercised without a harness. */
export async function loadApi(): Promise<Api> {
  if (new URLSearchParams(location.search).get("mock") === "1") {
    const { createMockApi } = await import("./mock");
    return createMockApi();
  }
  return httpApi;
}
