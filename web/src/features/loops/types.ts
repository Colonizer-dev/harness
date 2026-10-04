// ---------------------------------------------------------------------------
// Red-team runs (issue #212): a swarm of hunter colonies raiding one repository
// ---------------------------------------------------------------------------

/**
 * Ordered lifecycle of a red-team run (issue #212). `armed` and `waiting` are gated —
 * the run is live but not raiding until the nest empties — `running`/`draining` are
 * raiding, and `done`/`stopped` are terminal.
 */
export type RedTeamState = "armed" | "waiting" | "running" | "draining" | "done" | "stopped";

export interface RedTeamHunter {
  session_id: string;
  title: string;
  module: string;
  /** The module's pinned release; null when the module has no version of its own. */
  version: string | null;
  focus: string;
}

/**
 * The synthesis step (issue #309): once a run is `done`, the mothership launches one colony that
 * merges every hunter's findings into a single deduplicated, severity-ranked report.
 */
export type RedTeamSynthesisState = "pending" | "running" | "done" | "failed";

export interface RedTeamSynthesis {
  state: RedTeamSynthesisState;
  /** The current (newest) synthesis colony. */
  session_id: string | null;
  /** Host path of the newest successful merged report. */
  report: string | null;
  /** Why it failed. */
  reason: string | null;
  /** Earlier synthesis session ids, oldest first. */
  superseded: string[];
}

/** Which focus list and briefing a run's hunters get: the bug hunt, or the security hunt with a pre-scan. */
export type RedTeamPreset = "general" | "security";

/** A security pre-scan lead: a deterministic heuristic hit, never a confirmed vulnerability. */
export interface PreScanLead {
  /** `P1`, `P2`, …; hunters and the synthesis cite it. */
  id: string;
  check: string | null;
  /** Index into the security preset's eight focus areas. */
  focus: number;
  path: string;
  line: number | null;
  commit: string | null;
  message: string;
}

/** Operator checklist item states. There is deliberately no "passed": code cannot prove these. */
export type ChecklistStatus = "needs_review" | "not_verifiable";

export interface ChecklistItem {
  id: string;
  title: string;
  status: ChecklistStatus;
  evidence: string;
}

/** A security run's pre-scan, run on the host mirror before the hunters launch. */
export interface PreScan {
  ran_at: string | null;
  commit: string | null;
  /** `gitleaks` when the host had it installed, `builtin` for the fallback, empty when it could not run. */
  secret_scanner: string;
  notes: string[];
  leads: PreScanLead[];
  checklist: ChecklistItem[];
}

/** GET /api/redteam/runs: one swarm against one repository. */
export interface RedTeamRun {
  id: string;
  repo: string;
  org: string;
  state: RedTeamState;
  swarm_size: number;
  modules: string[];
  /** Whether the swarm may merge its finds; off by default, so a raid never touches main. */
  autofix: boolean;
  hunters: RedTeamHunter[];
  /** `merged` is the distinct defects after the synthesis dedup; null until a synthesis finishes. */
  counts: { found: number; validated: number; rejected: number; filed: number; merged: number | null };
  /** The run's synthesis step; null until one has launched (the mothership does it at `done`). */
  synthesis: RedTeamSynthesis | null;
  created_at: string;
  started_at: string | null;
  ended_at: string | null;
  /** The server's reason for holding an armed run at the gate; null while none applies. */
  gate_reason: string | null;
  /** Who hunts: `swarm` (colony hunters). Absent from runs made before hunters were named. */
  hunter?: string;
  /** The hunters' orchestrator / subagent models when named; null uses the agent defaults. */
  model?: string | null;
  subagent_model?: string | null;
  /** The schedule that started this run, if one did. */
  schedule_id?: string | null;
  /** Absent from runs made before presets: read as general. */
  preset?: RedTeamPreset;
  /** A security run's pre-scan; null for general runs and until a security run launches. */
  prescan?: PreScan | null;
}

/** POST /api/redteam/runs. `arm: true` starts gated, waiting for the nest to empty. */
export interface StartRedTeamRunRequest {
  repo: string;
  swarm_size?: number;
  modules?: string[];
  autofix?: boolean;
  arm?: boolean;
  hunter?: string;
  model?: string | null;
  subagent_model?: string | null;
  /** `general` when unset. */
  preset?: RedTeamPreset;
}

/** When a red-team schedule fires, in UTC. `weekday` 0 = Monday; a monthly `day` past the month's end fires on its last day. */
export type RedTeamCadence =
  | { every: "weekly"; weekday: number; hour: number; minute: number }
  | { every: "monthly"; day: number; hour: number; minute: number };

/** A recurring red-team run (GET /api/redteam/schedules). */
export interface RedTeamSchedule {
  id: string;
  org: string;
  repos: string[];
  hunter: string;
  swarm_size: number;
  model: string | null;
  subagent_model: string | null;
  autofix: boolean;
  /** Absent from schedules saved before presets: read as general. */
  preset?: RedTeamPreset;
  cadence: RedTeamCadence;
  enabled: boolean;
  next_run_at: string;
  last_run_at: string | null;
  last_result: string | null;
  created_at: string;
}

/** POST /api/redteam/schedules, and PUT /api/redteam/schedules/{id} (a full replace). */
export interface NewRedTeamSchedule {
  org: string;
  repos: string[];
  hunter?: string;
  swarm_size?: number;
  model?: string | null;
  subagent_model?: string | null;
  autofix?: boolean;
  preset?: RedTeamPreset;
  cadence: RedTeamCadence;
  enabled?: boolean;
}

/**
 * When a loop runs, in UTC (loops.rs, schedule.rs). `self_paced`: each run names the next
 * (loop_next), else a day later. `every_days` runs whole days apart at one time of day; the server
 * refuses days outside 1–365.
 */
export type LoopCadence =
  | RedTeamCadence
  | { every: "interval"; minutes: number }
  | { every: "daily"; hour: number; minute: number }
  | { every: "every_days"; days: number; hour: number; minute: number }
  | { every: "self_paced" };

/** A scheduled colony (GET /api/loops). */
export interface Loop {
  id: string;
  name: string;
  org: string;
  repo: string;
  prompt: string;
  cadence: LoopCadence;
  /** What a run starts: a colony from `prompt` (the default), the repository's architecture map, or —
   * for the one built-in loop, id `disk-cleanup` — the mothership's own disk cleanup. */
  kind?: LoopKind;
  /** Map loops only: repositories still queued this cycle; `owner/*` is re-listed every run. */
  pending?: string[];
  /** The built-in disk-cleanup loop only: its settings, run history and attention item. */
  disk_cleanup?: DiskCleanupState;
  tz_offset_minutes: number;
  model: string | null;
  subagent_model: string | null;
  autopilot: boolean;
  max_runs: number | null;
  end_at: string | null;
  enabled: boolean;
  /** Null once the loop has ended. */
  next_run_at: string | null;
  runs: number;
  last_run: { session: string; at: string } | null;
  /** The last thing it did or was told: a skip, the colony's chosen next run, why it ended. */
  last_note: string | null;
  ended_reason: string | null;
  created_at: string;
}

export type LoopKind = "colony" | "map" | "disk_cleanup";

/** What the built-in disk-cleanup loop may clean, each with its own switch. */
export type DiskCleanupCategory = "build_output" | "worktrees" | "microvms" | "archives" | "host_paths";

/** The disk-cleanup loop's settings (PUT /api/loops/disk-cleanup's `disk_cleanup`). */
export interface DiskCleanupSettings {
  /** Run early when free space is under this percent of the disk; 0 is off. */
  trigger_free_pct: number;
  build_output: boolean;
  stopped_after_days: number;
  worktrees: boolean;
  microvms: boolean;
  archives: boolean;
  archive_keep_days: number;
  archive_max_gb: number | null;
  /** Owner only, off by default: Cargo target/ dirs under `extra_paths`. */
  host_paths: boolean;
  extra_paths: string[];
  host_min_age_days: number;
}

export interface DiskCleanupCategoryReport {
  category: DiskCleanupCategory;
  enabled: boolean;
  items: { path: string; bytes: number | null; colony?: string }[];
  count: number;
  /** Freed, or in a dry run, would be freed. */
  bytes: number;
  held?: { path: string; reason: string }[];
  failed?: string[];
  note?: string;
}

/** POST /api/loops/disk-cleanup/run-now[?dry_run=1], and each entry of the loop's history. */
export interface DiskCleanupReport {
  at: string;
  dry_run: boolean;
  trigger: "schedule" | "low_disk" | "manual" | string;
  bytes: number;
  categories: DiskCleanupCategoryReport[];
  free_bytes_after?: number;
  used_pct_after?: number;
  attention?: string;
}

export interface DiskCleanupState {
  settings: DiskCleanupSettings;
  /** Real runs, newest first. */
  history: DiskCleanupReport[];
  attention: string | null;
  /** When a dry run was last shown; null until the owner has seen one. */
  previewed_at: string | null;
}

// The built-in "Dependencies & supply chain" loop (GET/PUT /api/supply-chain-loop,
// POST /api/supply-chain-loop/run; supply_chain_loop.rs).

export type SupplySeverity = "critical" | "high" | "moderate" | "low";
export type SupplyFindingKind = "vulnerability" | "yanked" | "unmaintained" | "deprecated" | "license" | "outdated";

/** The loop's settings: off, with an empty allowlist, until the operator opts in. */
export interface SupplyChainSettings {
  enabled: boolean;
  /** Orgs (`acme`) and repositories (`acme/app`) opted in. */
  allow: string[];
  /** Daily by default; `interval` no tighter than 60 minutes. */
  cadence: LoopCadence;
  max_per_repo: number;
  max_per_run: number;
  cooldown_hours: number;
  /** The least severe finding that is dispatched; everything is reported. */
  min_severity: SupplySeverity;
  /** Also report direct dependencies a major version or more behind (never dispatched). */
  outdated: boolean;
  /** Check lockfiles no host scanner reads with the mothership's own OSV lookup. */
  builtin: boolean;
  autopilot: boolean;
}

export interface SupplyFinding {
  ecosystem: string;
  package: string;
  version: string | null;
  kind: SupplyFindingKind;
  severity: SupplySeverity;
  id: string | null;
  title: string;
  fixed: string | null;
  fix_available: boolean;
  fix_via?: string | null;
  major_bump: boolean;
  url: string | null;
  lockfile: string;
  scanner: string;
}

export interface SupplyRepoReport {
  repo: string;
  sha: string | null;
  scanners: string[];
  findings: SupplyFinding[];
  notes: string[];
  /** Files nothing checked, and what to install. */
  missing: string[];
  error: string | null;
}

export interface SupplyAttention {
  repo: string;
  ecosystem: string;
  package: string;
  version: string | null;
  id: string | null;
  severity: SupplySeverity;
  reason: string;
}

export interface SupplyChainReport {
  id: string;
  started_at: string;
  finished_at: string;
  dry_run: boolean;
  trigger: "schedule" | "manual";
  blocked: boolean;
  repos: SupplyRepoReport[];
  counts: Partial<Record<SupplySeverity, number>>;
  dispatched: { repo: string; ecosystem: string; session: string | null; title: string; findings: number; worst: SupplySeverity }[];
  skipped: { repo: string; ecosystem: string | null; reason: string; findings: number }[];
  attention: SupplyAttention[];
  note: string | null;
}

export interface SupplyChainRun {
  id: string;
  at: string;
  trigger: string;
  counts: Partial<Record<SupplySeverity, number>>;
  dispatched: number;
  skipped: number;
  attention: number;
  summary: string;
}

export interface SupplyChainLoop {
  name: string;
  settings: SupplyChainSettings;
  next_run_at: string | null;
  running: boolean;
  /** Which scanners the mothership's host has installed. */
  scanners: Record<string, boolean>;
  /** Whether COLONIZER_NO_EXTERNAL_EFFECTS holds every dispatch. */
  blocked: boolean;
  last_report: SupplyChainReport | null;
  history: SupplyChainRun[];
  attention: SupplyAttention[];
}

/** POST /api/loops, and PUT /api/loops/{id} (a full replace). */
export interface NewLoop {
  name: string;
  repo: string;
  prompt: string;
  cadence: LoopCadence;
  /** Colony loops (the default) or map loops; a map loop's `repo` may be `owner/*`. `disk_cleanup`
   * only on the built-in loop's own PUT. */
  kind?: LoopKind;
  tz_offset_minutes?: number;
  model?: string | null;
  subagent_model?: string | null;
  autopilot?: boolean;
  max_runs?: number | null;
  end_at?: string | null;
  enabled?: boolean;
  /** The built-in disk-cleanup loop only; left out, its settings are kept. */
  disk_cleanup?: DiskCleanupSettings;
}

// The built-in "TypeScript: remove any" loop (GET/PUT /api/ts-any-loop,
// POST /api/ts-any-loop/run; ts_any_loop.rs).

export type TsAnyForm = "annotation" | "as" | "angle" | "type_argument" | "array" | "array_generic" | "record" | "generic_default" | "other";
export type TsAnyMethod = "typescript" | "token_scan";

/** The loop's settings: off, with an empty allowlist, until the operator opts in. */
export interface TsAnySettings {
  enabled: boolean;
  /** Orgs (`acme`) and repositories (`acme/app`) opted in. */
  allow: string[];
  /** Daily by default; `interval` no tighter than 60 minutes. */
  cadence: LoopCadence;
  /** Occurrences given to one colony (20 by default). */
  batch_cap: number;
  max_per_run: number;
  cooldown_hours: number;
  /** Also count implicit any (only with the repository's own TypeScript). */
  implicit: boolean;
  /** Install from the lockfile, offline, when node_modules is absent. */
  offline_install: boolean;
  autopilot: boolean;
}

export interface TsAnyModuleCount {
  module: string;
  explicit: number;
  files: number;
}

export interface TsAnyFileCount {
  path: string;
  module: string;
  explicit: number;
  implicit?: number | null;
  as_casts: number;
  suppressions: number;
}

export interface TsAnyRepoReport {
  repo: string;
  sha: string | null;
  typescript: boolean;
  method: TsAnyMethod | null;
  method_note: string | null;
  ts_version: string | null;
  total: number;
  implicit: number | null;
  as_casts: number;
  suppressions: number;
  ts_files: number;
  forms: Partial<Record<TsAnyForm, number>>;
  modules: TsAnyModuleCount[];
  files: TsAnyFileCount[];
  /** Earlier real runs' totals, newest first. */
  previous: number[];
  notes: string[];
  error: string | null;
}

export interface TsAnyAttention {
  repo: string;
  module: string;
  session: string;
  pr_url: string | null;
  problems: string[];
  reason: string;
}

export interface TsAnyReport {
  id: string;
  started_at: string;
  finished_at: string;
  dry_run: boolean;
  trigger: "schedule" | "manual";
  blocked: boolean;
  repos: TsAnyRepoReport[];
  total: number;
  dispatched: { repo: string; module: string; session: string | null; title: string; occurrences: number; module_total: number }[];
  skipped: { repo: string; module: string | null; reason: string }[];
  checks: { session: string; repo: string; module: string; pr_url: string | null; flagged: boolean; summary: string }[];
  attention: TsAnyAttention[];
  note: string | null;
}

export interface TsAnyRun {
  id: string;
  at: string;
  trigger: string;
  total: number;
  totals: Record<string, number>;
  dispatched: number;
  skipped: number;
  flagged: number;
  summary: string;
}

export interface TsAnyLoop {
  name: string;
  settings: TsAnySettings;
  next_run_at: string | null;
  running: boolean;
  /** Whether node is on the host (the repository's own TypeScript needs it). */
  node: boolean;
  /** Whether COLONIZER_NO_EXTERNAL_EFFECTS holds every dispatch. */
  blocked: boolean;
  last_report: TsAnyReport | null;
  /** Newest first. */
  history: TsAnyRun[];
  attention: TsAnyAttention[];
  /** Totals per repository, oldest first. */
  trend: Record<string, { at: string; total: number; sha: string | null }[]>;
}

/** GET /api/hunters/{id}/probe: whether an external hunter is installed and could run here. */
export interface HunterProbe {
  manifest: { id: string; name: string; description: string; homepage: string; licence: string; available: boolean; needs_docker: boolean };
  installed: string | null;
  probe: { runtime_ok: boolean; docker_ok: boolean; ready: boolean; detail: string };
}

/** One colony's pull request and where it stands in its repository's merge train (issue #671). */
export interface MergeTrainPr {
  session: string;
  pr_url: string;
  title: string;
  status: "next" | "waiting_ci" | "needs_rebase" | "waiting" | "skipped" | "merged";
  /** Why the pull request stands where it does — always set, for every status. */
  reason: string;
}

/** Per-repository merge-train state: the base branch's CI, the train's last merge and the queued pull requests. */
export interface MergeTrainRepo {
  repo: string;
  /** `on` while the train drives the repository, `off` before opt-in, `denied` when the org sits on `merge_train_deny_orgs`. */
  state: "on" | "off" | "denied";
  /** The repository's default branch; null when the train is off or denied here, or it could not be read. */
  base: string | null;
  base_ci: "green" | "pending" | "failing" | "unknown";
  /** When the mothership last looked; null before the first pass. */
  checked_at: string | null;
  /** The train's most recent merge; null until it merged one. */
  last_merge: { pr_url: string; at: string } | null;
  prs: MergeTrainPr[];
}

/** GET /api/merge-train (issue #671): empty until a repository opts in. */
export interface MergeTrainStatus {
  repos: MergeTrainRepo[];
}

/** The merge-train loop's settings (issue #754): off, hourly, and no repository opted in by default. */
export interface MergeLoopSettings {
  enabled: boolean;
  cadence: LoopCadence;
  /** Opted-in `owner` or `owner/repo` entries; empty merges nowhere. */
  allow: string[];
  /** `owner` or `owner/repo` entries never merged in (upstream-review-only forks), whatever `allow` says. */
  never: string[];
  max_merges: number;
  /** Per-repository caps that replace `max_merges` there. */
  repo_max_merges: Record<string, number>;
  cooldown_secs: number;
  ci_wait_minutes: number;
  ci_poll_secs: number;
  /** Check names re-run once when they are all that fails; a trailing `*` matches a prefix. */
  flaky_checks: string[];
  self_heal: boolean;
  revert_on_red: boolean;
  redo_on_conflict: boolean;
  max_api_calls: number;
  min_call_gap_ms: number;
  /** Colony ids held out of the loop. */
  held: string[];
}

export type MergeLoopAction = "merged" | "updated" | "rebased" | "red" | "rerun" | "needs_redo" | "redo_dispatched" | "waiting" | "skipped";

export interface MergeLoopItem {
  session: string;
  pr_url: string;
  title: string;
  action: MergeLoopAction;
  reason: string;
}

export interface MergeLoopRepoReport {
  repo: string;
  /** Main's CI as the run last read it, in words. */
  main: string;
  paused: string | null;
  /** What the run did about a red main. */
  heal: string[];
  items: MergeLoopItem[];
}

/** One run's report: merged, updated (CI running), red, redo dispatched, skipped — each with its reason. */
export interface MergeLoopReport {
  started_at: string | null;
  finished_at: string | null;
  dry_run: boolean;
  /** The kill switch (COLONIZER_NO_EXTERNAL_EFFECTS) turned a real run into this dry run. */
  forced_dry_run: boolean;
  /** Why the run stopped early: GitHub pushed back (403/429), or the call budget ran out. */
  stopped: string | null;
  api_calls: number;
  summary: string;
  lines: string[];
  repos: MergeLoopRepoReport[];
}

/** GET /api/merge-train/loop. */
export interface MergeLoopView {
  settings: MergeLoopSettings;
  next_run_at: string | null;
  running: boolean;
  writes_blocked: boolean;
  repos: Record<string, { paused: string | null; needs_redo: Record<string, string> }>;
  last_report: MergeLoopReport | null;
  /** Newest first. */
  history: MergeLoopReport[];
}
