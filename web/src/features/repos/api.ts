// Repositories, code & issues API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, enc, post, put, query, repoPath, request } from "../../http";
import type { CreatedIssue, Draft, EditsRequest, FileCommit, Issue, IssueDraft, IssueDrafts, MapFileDetail, PackagesDependencies, PackagesPublished, Repo, RepoBlame, RepoBlob, RepoBranches, RepoCoverage, RepoGitSummary, RepoLoc, RepoMap, RepoMeta, RepoPackages, RepoTree, ScanPending, SupplyChain, TouchedFiles } from "./types";

export interface ReposApi {
  repos(): Promise<Repo[]>;
  issues(repo: string): Promise<Issue[]>;
  /** POST /api/colonize/draft: free text as one or a few issue drafts, from the cheap summary model (the text itself when there is none). Files nothing. */
  draftIssues(body: { text: string; repo?: string }): Promise<IssueDrafts>;
  /** POST /api/repos/{owner}/{repo}/issues: files one issue with the Mothership's `gh`. */
  createIssue(repo: string, body: IssueDraft): Promise<CreatedIssue>;
  /** GET /api/repos/{owner}/{repo}/packages: monorepo detection. */
  repoPackages(repo: string): Promise<RepoPackages>;
  /** A repository's architecture map and the newest colony drawing it. */
  repoMap(repo: string): Promise<RepoMap>;
  /** GET /api/repos/{owner}/{repo}/meta: description, languages, weekly commits, contributors. */
  repoMeta(repo: string): Promise<RepoMeta>;
  /** GET /api/orgs/{org}/packages/published: what the workspace's repositories define and publish.
   *  These three answer from the mothership's cache; `refresh` asks it to recompute behind the answer. */
  orgPublished(org: string, refresh?: boolean): Promise<PackagesPublished | ScanPending>;
  /** GET /api/orgs/{org}/packages/dependencies: what they depend on, from their lockfiles. */
  orgDependencies(org: string, refresh?: boolean): Promise<PackagesDependencies | ScanPending>;
  /** GET /api/orgs/{org}/packages/supply-chain: risky dependencies, with reasons. */
  orgSupplyChain(org: string, refresh?: boolean): Promise<SupplyChain | ScanPending>;
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
  /** GET /api/maps/{owner}/{repo}/files: every file at the map's revision, from the local clone. */
  repoMapFiles(repo: string): Promise<{ repo: string; revision: string; paths: string[]; truncated: boolean }>;
  /** GET /api/maps/{owner}/{repo}/file?path=…: live colonies on one file, their calls on it and their diff. */
  repoMapFile(repo: string, path: string): Promise<MapFileDetail>;
  /** Launches a colony that draws the repository with archify (or returns the one already drawing). */
  mapRepo(repo: string): Promise<RepoMap>;
  /** The files each live colony's worktree has changed. */
  touched(): Promise<TouchedFiles>;
}

export const reposHttp: ReposApi = {
  repos: () => request("/api/repos"),
  issues: (repo) => {
    const [owner, name] = repo.split("/");
    return request(`/api/repos/${enc(owner)}/${enc(name)}/issues`);
  },
  draftIssues: (body) => post("/api/colonize/draft", body),
  createIssue: (repo, body) => {
    const [owner, name] = repo.split("/");
    return post(`/api/repos/${enc(owner)}/${enc(name)}/issues`, body);
  },
  repoPackages: (repo) => {
    const [owner, name] = repo.split("/");
    return request(`/api/repos/${enc(owner)}/${enc(name)}/packages`);
  },
  repoMap: (repo) => request(`/api/maps/${repo.split("/").map(enc).join("/")}`),
  repoMeta: (repo) => request(`/api/repos/${repo.split("/").map(enc).join("/")}/meta`),
  orgPublished: (org, refresh) => request(`/api/orgs/${enc(org)}/packages/published${refresh ? "?refresh=1" : ""}`),
  orgDependencies: (org, refresh) => request(`/api/orgs/${enc(org)}/packages/dependencies${refresh ? "?refresh=1" : ""}`),
  orgSupplyChain: (org, refresh) => request(`/api/orgs/${enc(org)}/packages/supply-chain${refresh ? "?refresh=1" : ""}`),
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
  repoMapFiles: (repo) => request(`/api/maps/${repo.split("/").map(enc).join("/")}/files`),
  repoMapFile: (repo, path) => request(`/api/maps/${repo.split("/").map(enc).join("/")}/file?path=${encodeURIComponent(path)}`),
  mapRepo: (repo) => post(`/api/maps/${repo.split("/").map(enc).join("/")}`),
  touched: () => request("/api/touched"),
};
