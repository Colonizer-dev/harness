import type { SessionStatus } from "../sessions/types";

export interface Repo {
  full_name: string;
  description: string | null;
  private: boolean;
  fork: boolean;
  archived: boolean;
  open_issues_count: number;
  pushed_at: string | null;
  has_issues?: boolean;
}

export interface Issue {
  number: number;
  title: string;
  body: string | null;
  labels: { name: string; color: string }[];
  author: { login: string } | null;
  updatedAt: string;
  url: string;
  /** Set by the mothership's issue list when the issue is an epic (sub-issues, an `epic` label, or a title marking it): why, and how many sub-issues. A launch on it answers 409 without `allow_epic`. */
  epic?: { reason: string; sub_issues: number } | null;
}

/** One issue Colonize drafted from free text, shown for a confirm or an edit before it is filed. */
export interface IssueDraft {
  title: string;
  body: string;
}

/** POST /api/colonize/draft: `model` is null (and `note` says why) when the text came back as its own draft. */
export interface IssueDrafts {
  issues: IssueDraft[];
  model: string | null;
  note?: string;
  /** The Source module's include labels, which filing adds so the filtered list still offers the issue. Absent from older motherships. */
  labels?: string[];
}

/** POST /api/repos/{owner}/{repo}/issues: the issue just filed; `number` is null if gh's answer named none. */
export interface CreatedIssue {
  repo: string;
  number: number | null;
  title: string;
  url: string;
  /** The Source labels the issue carries. Absent from older motherships. */
  labels?: string[];
  /** Source labels it could not be given (missing on the repository and not creatable); the issue is filed anyway. */
  labels_skipped?: string[];
}

/** GET /api/repos/{owner}/{repo}/packages: whether the repository is a monorepo, and its packages. */
export interface RepoPackages {
  monorepo: boolean;
  tool: string | null;
  packages: { name: string; path: string }[];
}

/** One component of a repository's architecture map (GET /api/maps/{owner}/{repo}, from an archify
 *  architecture diagram): archify's own layout (`pos` top-left, `size`), and the repository files
 *  it lives in. */
export interface ArchComponent {
  id: string;
  type: string;
  label: string;
  sublabel?: string | null;
  pos: [number, number];
  size: [number, number];
  sources: { path: string; line?: number; label?: string }[];
}

/** The fields of an archify architecture diagram the cockpit draws, as the mothership stores them. */
export interface ArchMap {
  title: string;
  subtitle?: string | null;
  components: ArchComponent[];
  connections: { from: string; to: string; label?: string }[];
  boundaries: { label: string; wraps: string[] }[];
}

/** GET /api/maps/{owner}/{repo}: the stored map, if any, and the newest mapping colony, if any. */
/** One tool call a colony made on a file (GET /api/maps/{owner}/{repo}/file). */
export interface MapFileActivity {
  ts: string;
  tool: string;
  summary: string;
  /** The subagent (settler) the call ran in, when the event says. */
  agent: string | null;
}

/** A live colony on one file: what it did there, and its diff of it. */
export interface MapFileColony {
  id: string;
  title: string;
  issue: number | null;
  status: SessionStatus;
  mode: "changing" | "reading";
  activity: MapFileActivity[];
  diff: string | null;
  diff_truncated: boolean;
}

/** GET /api/maps/{owner}/{repo}/file?path=…: every live colony changing or reading one file. */
export interface MapFileDetail {
  repo: string;
  path: string;
  colonies: MapFileColony[];
}

export interface RepoMap {
  repo: string;
  map: { repo: string; revision: string | null; generated_at: string; session: string; map: ArchMap } | null;
  mapping: { id: string; status: SessionStatus; created_at: string } | null;
}

/** GET /api/touched: the files each live colony's worktree has changed, keyed by session id. */
export interface TouchedFiles {
  sessions: Record<string, string[]>;
  /** The files each live colony's recent tool calls looked at, newest first; absent from an older mothership. */
  reading?: Record<string, string[]>;
}

/** GET /api/repos/{owner}/{repo}/meta: what the repository picker shows about a repository. */
export interface RepoMeta {
  full_name: string;
  description: string | null;
  homepage: string | null;
  stars: number;
  primary_language: string | null;
  languages: { name: string; bytes: number; percent: number }[];
  /** 52 weekly commit counts, oldest first; empty while GitHub is still computing them. */
  commits_weekly: number[];
  stats_pending: boolean;
  contributors: { login: string; avatar_url: string; contributions: number }[];
  pushed_at: string | null;
  html_url: string | null;
}

// ---------------------------------------------------------------------------
// The Code page (code.rs): a repository read from the mothership's bare clone
// ---------------------------------------------------------------------------

/** GET /api/repos/{o}/{r}/loc: lines of code by language at the default branch. */
export interface RepoLoc {
  ref: string;
  sha: string;
  total: number;
  by_language: { name: string; files: number; code: number; blank: number }[];
}

/** GET /api/repos/{o}/{r}/coverage: line coverage from CI artifacts, or why there is none. */
export type RepoCoverage =
  | { measured: true; percent: number; format: string; file: string; artifact: string; run: number; at?: string }
  | { measured: false; reason: string };

/** GET /api/repos/{o}/{r}/git-summary. */
export interface RepoGitSummary {
  repo: string;
  branches: number;
  open_prs: number | null;
  release: { tagName: string; name: string; publishedAt: string } | null;
  latest_tag: string | null;
}

export interface RepoBranch {
  name: string;
  sha: string;
  date: string;
  author: string;
  message: string;
  default: boolean;
  protected: boolean;
  colony: boolean;
  ahead: number;
  behind: number;
  pr: { number: number; title: string; url: string; isDraft: boolean } | null;
}

export interface RepoBranches {
  repo: string;
  default: string;
  branches: RepoBranch[];
}

export interface RepoTree {
  repo: string;
  ref: string;
  sha: string;
  paths: string[];
  truncated: boolean;
}

export interface RepoBlob {
  path: string;
  ref: string;
  sha: string;
  size: number;
  binary: boolean;
  too_large: boolean;
  text: string | null;
}

export interface FileCommit {
  sha: string;
  author: string;
  date: string;
  message: string;
}

export interface RepoBlame {
  path: string;
  ref: string;
  sha: string;
  commits: Record<string, { author?: string; time?: number; summary?: string }>;
  /** Per line (0-based index = line - 1), the commit that last touched it. */
  lines: string[];
}

export interface EditsRequest {
  base?: string;
  branch: string;
  message: string;
  title: string;
  body: string;
  files: { path: string; content: string }[];
}

/** An autosaved edit on the mothership (never on GitHub until a pull request is confirmed). */
export interface Draft {
  ref: string;
  path: string;
  content: string;
  base_sha: string;
  saved_at: string;
}

// ---------------------------------------------------------------------------
// Packages (GET /api/orgs/{org}/packages/*): published, dependencies, supply chain
// ---------------------------------------------------------------------------

/** A scan that has not landed yet: ask again in a few seconds. */
export interface ScanPending {
  status: "scanning";
  message: string;
}

/** What the mothership adds to an answer served from its cache: when it was computed, and whether
 *  a refresh is running behind it. */
export interface CacheInfo {
  cached_at?: string;
  refreshing?: boolean;
}

export type Ecosystem = "npm" | "cargo" | "pypi" | "go" | "dart" | "swift";

export interface ScannedRepo {
  repo: string;
  sha?: string;
  error?: string;
  lockfiles?: string[];
  skipped?: string[];
  defined?: number;
}

export interface RegistryInfo {
  latest: string | null;
  published_at: string | null;
  created_at?: string | null;
  downloads: number | null;
  downloads_period?: string;
  url: string;
}

export interface PublishedPackage {
  ecosystem: Ecosystem;
  name: string;
  version: string | null;
  repo: string;
  path: string;
  private: boolean;
  registry: string | null;
  status: "published" | "unpublished" | "private";
  /** The repository's version is ahead of the registry's latest. */
  unreleased_changes: boolean;
  published: RegistryInfo | null;
}

export interface GithubPackage {
  name: string;
  type: string;
  visibility: string;
  versions: number | null;
  updated_at: string | null;
  url: string | null;
  repo: string | null;
}

export interface PackagesPublished extends CacheInfo {
  org: string;
  scanned_at: string;
  repos: ScannedRepo[];
  packages: PublishedPackage[];
  github_packages: { packages: GithubPackage[]; note: string | null };
}

export interface Advisory {
  id: string;
  summary?: string | null;
  severity: string;
  fixed?: string | null;
  url?: string;
}

export interface DependencyVersion {
  version: string;
  behind: boolean;
  users: { repo: string; path: string }[];
  vulns: Advisory[];
}

export interface Dependency {
  ecosystem: Ecosystem;
  name: string;
  direct: boolean | null;
  dev: boolean;
  latest: string | null;
  outdated: boolean;
  vulnerable: boolean;
  drift: boolean;
  versions: DependencyVersion[];
}

export interface PackagesDependencies extends CacheInfo {
  org: string;
  scanned_at: string;
  repos: ScannedRepo[];
  ecosystems: { ecosystem: Ecosystem; direct: number; transitive: number }[];
  totals: { direct: number; transitive: number; outdated: number; vulnerable: number };
  packages: Dependency[];
}

export type RiskSeverity = "critical" | "high" | "moderate" | "low";

export interface SupplyRisk {
  severity: RiskSeverity;
  kind: string;
  ecosystem: Ecosystem;
  name: string;
  version: string | null;
  reason: string;
  fix: { available: boolean; version?: string | null };
  url: string;
  direct: boolean;
  via: string[];
  users: { repo: string; path: string }[];
}

export interface SupplyChain extends CacheInfo {
  org: string;
  scanned_at: string;
  repos: ScannedRepo[];
  counts: Partial<Record<RiskSeverity, number>>;
  fixable: number;
  risks: SupplyRisk[];
  note: string;
}
