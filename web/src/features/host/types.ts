import type { QuotaCard, SessionStatus, StallInfo } from "../sessions/types";
import type { ModelProviderStatus, StatusQuota } from "../providers/types";

export interface HarnessStatus {
  github: { connected: boolean; login?: string; name?: string | null; avatar_url?: string | null; source?: string; error?: string };
  claude: { configured: boolean; source: string | null; kind: string | null; account?: string | null; account_note?: string | null; saved_at?: string | null; expires_at?: string | null; expires_estimated?: boolean };
  sandbox: {
    msb_version: string | null;
    image: string;
    cpus?: number;
    memory?: string;
    max_parallel?: number;
    claude_bin?: string | null;
    claude_bin_error?: string | null;
  };
  mesh?: {
    enabled: boolean;
    provider?: string;
    state?: string;
    harness_ip?: string | null;
    nodes?: number;
    /** A fact about the platform, not a fault: shown plainly, never as an error. */
    detail?: string | null;
    error?: string | null;
  } | null;
  /** `{ ok: true }` alone until there is a storage alert: a failed disk write, which can recover, or colony records lost at startup, which cannot (see `StorageHealth.kind`). `ok` is the current write verdict, not a latch: a failure sets it false and the next write through sets it true again with `recovered_at`. The disk-space readings below ride every poll regardless (issue #220); older mothership builds omit the whole object. */
  storage?: StorageHealth;
  /** Aggregate reclamation counts from the same poll (issue #223); older mothership builds omit it. */
  reclaim?: { reclaimable: number; unpushed: number };
  /** The machine facts a colony's first minute depends on (issue #129); older mothership builds omit it. */
  runtime?: RuntimeInfo;
  /** The machine every colony in the overview boots on (issue #205); older mothership builds omit it. */
  host?: HostInfo | null;
  /** One entry per configured model provider, so the status poll can answer "is it the provider?" without the providers screen; older mothership builds omit it. */
  model_providers?: ModelProviderStatus[];
  /** Quota exhaustion across providers (issue #225); older mothership builds omit it. */
  quota?: StatusQuota | null;
  /** "Provider out of quota" cards (issue #767), the same list GET /api/attention serves; older builds omit it. */
  quota_cards?: QuotaCard[];
  /** Queue-wide stall readout (issue #230); null when nothing is stalled, omitted by older builds. */
  stall?: StallInfo | null;
  /** The shared anti-spam ledger's tallies (issue #311): what notify and the autonomous judge delivered, held for the digest, or dropped, by class, with the limits in force. Counts by class only — no colony ids. Older mothership builds omit it. */
  ledger?: LedgerStatus | null;
}

/** GET /api/status `ledger` (issue #311): the anti-spam ledger's running tallies. */
export interface LedgerStatus {
  /** Per class (e.g. `question`, `provider_degraded`, `judge`): how many candidates were delivered, held for the digest, or dropped. */
  counters: Record<string, { delivered: number; digested: number; dropped: number }>;
  /** Candidates held since the last digest line went out, by class. */
  pending_digest: Record<string, number>;
  /** When the last digest line was delivered; null until the first one. */
  last_digest: string | null;
  /** 1 when a corrupt `ledger.json` was quarantined aside at startup, else 0. */
  quarantined: number;
  limits: { notify: LedgerLimits; judge: LedgerLimits };
}

/** The rules one ledger claimant lives under; see `LedgerStatus`. */
export interface LedgerLimits {
  quiet_hours: { start: number; end: number } | null;
  per_hour: number;
  per_day: number;
  topic_cooldown_minutes: number;
  dedup_window_hours: number;
  topic_daily_cap: number;
}

/** GET /api/status `runtime` (issue #129): what kind of machine the mothership runs on, and what it can reach. The mothership re-probes all of it; the frontend only reads. */
export interface RuntimeInfo {
  /** `linux-x86_64`, `darwin-arm64`, or `other` for a platform Setup must call unsupported. */
  platform: string;
  /** Whether `/dev/kvm` is readable and writable by the mothership. Linux only; null on other platforms. */
  kvm: { ok: boolean; error: string | null } | null;
  /** Git on the mothership's PATH; colonies use it. */
  git: { ok: boolean; version?: string; error?: string | null };
  /** The GitHub CLI on the mothership's PATH; the installer and colonies use it. */
  gh: { ok: boolean; version?: string; error?: string | null };
  /** The Claude Code binary on the host, used for subscription login; null when not found. */
  host_claude_bin: string | null;
  /** Why the host binary is missing, when it is. */
  host_claude_bin_error: string | null;
  /**
   * Which operating system the mothership runs on, as the host distro tooling reports it. Additive
   * display info only — `platform` stays the supported/unsupported gate. Older mothership builds
   * omit the whole field.
   */
  os?: OsInfo;
}

/**
 * GET /api/status `runtime.os` (issue #208): which operating system the mothership runs on.
 * Additive display info only — `platform` remains the supported/unsupported gate. Older mothership
 * builds omit the whole block.
 */
export interface OsInfo {
  /** The family key, one of: ubuntu, debian, fedora, rhel, centos, rocky, almalinux, arch, omarchy, manjaro, endeavouros, nixos, alpine, opensuse, linux, apple, unknown. */
  vendor: string;
  /** Display name, e.g. "Ubuntu", "macOS". */
  name: string;
  /** e.g. "24.04", "14.5"; null when there is no version to report. */
  version: string | null;
  /** Raw os-release ID, Linux only; null off Linux and where the probe found none. */
  id: string | null;
}

/**
 * GET /api/status `host` (issue #205): the machine every colony boots on, re-probed on each status
 * poll. Every measurable is optional and omitted — never null, never zero-filled — when the host
 * cannot read it, so the overview never draws a number the mothership did not measure.
 */
export interface HostInfo {
  /** Stable per-install host id (uuid). It keys the host: a second machine can be summed into the
   * overview later instead of being mistaken for this one (issue #205). */
  id: string;
  /** Omitted when unmeasurable. */
  hostname?: string;
  cpu_cores?: number;
  memory_total_bytes?: number;
  memory_used_bytes?: number;
  /** 1, 5 and 15 minute load averages; the overview shows the first. */
  load?: [number, number, number];
  uptime_secs?: number;
  disk_total_bytes?: number;
  disk_used_bytes?: number;
  disk_free_bytes?: number;
  /** When the probe ran, RFC3339; always present. */
  checked_at: string;
  /** Always present. */
  microvms_live: number;
  /** Always present. */
  microvms_ceiling: number;
  /** Whether this host can boot a microVM at all; omitted on non-Linux. false means colonies cannot start here, which reads as idle rather than broken. */
  kvm_ok?: boolean;
}

/** GET /api/status `storage`: whether the mothership can still write its own files (sessions.json, colony event logs). */
export interface StorageHealth {
  /** False while writes are failing; true when every write was confirmed, or once one succeeds after a failure (then `recovered_at` is set). Always true for load damage. */
  ok: boolean;
  /**
   * Which alert this is (issue #371). `write`: a disk write failed; it recovers once one goes through.
   * `load_damage`: sessions.json was unreadable or partly damaged at startup, so colony records were
   * lost; `ok` only says writes work, it never recovers, and `message` names the `.corrupt-` copy.
   * Older motherships omit it: read a missing kind as `write`.
   */
  kind?: "write" | "load_damage" | null;
  /** The underlying write error (for load damage: what was lost and where the original went), for showing verbatim. Kept after a recovery: the gap it reports still happened. */
  message?: string | null;
  /** When the latest failure was recorded (for load damage: when startup found it); same representation as a harness_log `ts`. */
  ts?: string | null;
  /** Failed writes since the mothership started; a recovery does not reset it. A load_damage alert always carries 1, which is not a write count. */
  failures?: number | null;
  /** When a write first succeeded after the latest failure; null while writes are still failing. Absent from older motherships, whose alert stays until a restart. */
  recovered_at?: string | null;
  /** Free bytes on the data dir's volume at the queue's last check (issue #220); null when there is no reading yet or df failed. Absent on older motherships. */
  free_bytes?: number | null;
  /** Warn threshold in free bytes; 0 means the warning is off. Absent on older motherships. */
  warn_free_bytes?: number;
  /** Floor in free bytes; 0 means the floor is off. Below it the queue stops starting new colonies (`admission_paused`). Absent on older motherships. */
  min_free_bytes?: number;
  /** Free space is below the warn threshold (or the floor). Absent on older motherships. */
  low_disk?: boolean;
  /**
   * Free space is below the floor: the queue is not starting new colonies. Running colonies keep
   * running and admission resumes on its own when space returns; the pause itself deletes nothing.
   * Below the floor the reclaim sweep still reclaims finished colonies whose work is already pushed;
   * unpushed work is never deleted. Absent on older motherships.
   */
  admission_paused?: boolean;
}

/** GET /api/storage: disk usage and what automatic reclamation can (and pointedly will not) take (issues #223, #220). */
export interface StorageSummary {
  enabled: boolean;
  retention_secs: number;
  min_free_bytes: number;
  warn_free_bytes: number;
  /** Free bytes on the data dir's volume; null when there is no reading yet or df failed. */
  free_bytes: number | null;
  /** Free space is below the floor: the queue is not starting new colonies (running ones keep running). */
  admission_paused: boolean;
  /** Data-dir usage by category. `archive_bytes` is the log archive under `<data_dir>/archive`; `microsandbox_bytes` is microsandbox's whole home directory (holding the shared OCI image cache) — informational, never offered for cleanup; null when unmeasured. */
  totals: { worktrees_bytes: number; repos_bytes: number; sessions_bytes: number; archive_bytes: number; microsandbox_bytes: number | null };
  /** Finished colonies whose work is pushed (a PR, or no_changes) and not yet cleaned up; `due` means past the auto-reclaim retention window. */
  reclaimable: Array<{ id: string; status: SessionStatus; pr_url: string | null; bytes: number; updated_at: string; due: boolean }>;
  /** Terminal colonies with no PR: listed for a person, never auto-deleted. */
  unpushed: Array<{ id: string; status: SessionStatus; bytes: number; updated_at: string }>;
  /** Worktree directories with no colony behind them, and what the sweep will do. */
  orphans: Array<{ path: string; bytes: number; action: string }>;
}

/**
 * One archived colony's logs under `<data_dir>/archive` (issue #496). The backend's sidecar record
 * carries every key on every entry — absent reads as `null`, never a missing field — and sends
 * several more (`org`, `pr_url`, `cost_usd`, `model_usage`, `model_tier`, `agent`, `created_at`,
 * `updated_at`, `mothership`, `fingerprint`) that nothing here reads.
 */
export interface ArchiveEntry {
  session: string;
  repo: string;
  /** null for a colony started on a repository without an issue. */
  issue: number | null;
  title: string;
  status: string;
  bundle: string;
  bytes: number;
  archived_at: string;
  revision: number;
}

/** GET /api/archive (issue #496): the whole log archive, bundles included. */
export interface ArchiveListing {
  root: string;
  count: number;
  bytes: number;
  entries: ArchiveEntry[];
}

/** POST /api/archive/retention (issue #496): preview a cleanup pass (`dry_run: true`) or apply one. */
export interface RetentionRequest {
  keep_days: number | null;
  max_gb: number | null;
  /** Bundles that are the only copy are never removed unless this is set. */
  allow_single_copy: boolean;
  dry_run: boolean;
  /** On apply, the previewed bundle list; the server answers 409 when the archive no longer matches. */
  expect: string[] | null;
}

/** POST /api/archive/retention's answer (issue #496): what the pass takes, or would take. */
export interface RetentionPlan {
  dry_run: boolean;
  remove: Array<{ bundle: string; session: string; bytes: number; archived_at: string }>;
  count: number;
  bytes: number;
  /** Bundles held back because they are the only copy and `allow_single_copy` was false. */
  kept_single_copy: number;
}

/** GET /api/providers/{id}/health */
/** A background pull of the colony image. No percentage: msb reports none when piped. */
export type PullState = "idle" | "cached" | "pulling" | "done" | "failed";

export interface PullStatus {
  image: string;
  state: PullState;
  started_at: string | null;
  finished_at: string | null;
  error: string | null;
}

/** GET /api/headroom: the Headroom bundle a colony runs Headroom from, downloaded when it is switched on. */
export type HeadroomState = "idle" | "installed" | "downloading" | "unpacking" | "failed" | "unavailable";

export interface HeadroomStatus {
  /** The pinned bundle release; null when none is published for this machine's architecture. */
  release: string | null;
  state: HeadroomState;
  bytes: number;
  total: number | null;
  started_at: string | null;
  finished_at: string | null;
  error: string | null;
}

/** GET /api/version: what this mothership was built from. */
export interface BuildInfo {
  version: string;
  commit: string | null;
  dirty: boolean;
  built_at: string;
  release: string | null;
}

/** GET /api/update: the installed build, and the latest release if the check is on. */
export interface UpdateStatus {
  enabled: boolean;
  blocked_by: string | null;
  installed: BuildInfo;
  latest: { version: string; url: string; notes: string; published_at: string | null } | null;
  available: boolean;
  last_checked: string | null;
  error: string | null;
  /// Whether this install can update itself, and why not if it cannot.
  can_apply: { ok: boolean; reason: string | null };
  apply: {
    phase: "idle" | "installing" | "restarting" | "failed";
    version: string | null;
    started_at: string | null;
    error: string | null;
    log: string;
    colonies: { id: string; repo: string; outcome: string }[];
    backup: string | null;
  };
}

/** GET /api/telemetry: the live map on colonizer.dev (docs/telemetry.md). */
export interface TelemetryStatus {
  /** null until the user has answered. */
  enabled: boolean | null;
  /** An environment variable keeping it off whatever Settings says (DO_NOT_TRACK or COLONIZER_TELEMETRY). */
  blocked_by: string | null;
  endpoint: string;
  map_url: string;
  last_sent_at: string | null;
  last_error: string | null;
  /** Exactly what the next heartbeat carries; install_id is null until the live map is first switched on. */
  heartbeat: { install_id: string | null; version: string; platform: string; colonies: number };
}

/** The anonymous usage batch, exactly what a sender transmits: Cratefield's module-telemetry payload (Cratefield/harness#413), the grammar the collector on the other side parses. Built whatever the switch says. */
export interface UsageBatch {
  schema: number;
  /** 32 lowercase hex — the per-on-period usage id without its dashes. All zeros while reporting is off or held off by the environment: a batch the sender refuses to post. */
  install: string;
  client: { kind: string; version: string; platform: string; arch: string };
  /** The declared modules this batch speaks for: the mothership, and nothing else it composes. */
  modules: string[];
  /**
   * One event per observation; each name's `.`-separated parts come from a closed vocabulary, so the
   * label a field used to carry rides in the name (`colonies.parallel_now.2-3`, `boot.vm-boot.5-15s`,
   * `setting.agent.model`). Outcome, error class, duration and count stay at their neutral values: a
   * usage batch is a set of observations, not runs. docs/usage-data.md maps field by field.
   */
  events: { name: string; outcome: "ok" | "error" | "cancelled"; error: string; duration: string; count: number }[];
}

/** GET /api/telemetry/usage, and of a successful PUT. The batch is built whatever the switch says, so it can be read in full — and it is the same value the sender posts, at most once a day when an endpoint is named. */
export interface UsageStatus {
  /** Already resolved: true when reporting is on — including when nobody has answered, since it is on by default — false once declined or held off by the environment. */
  enabled: boolean;
  /** An environment variable keeping it off whatever Settings says (COLONIZER_TELEMETRY, DO_NOT_TRACK or CI). */
  blocked_by: string | null;
  /** The payload schema the batch speaks; the same value as `batch.schema`. */
  payload_version: number;
  batch: UsageBatch;
}

/** GET/POST /api/login-item: whether the mothership starts at login (a LaunchAgent or systemd user unit). */
export interface LoginItemStatus {
  platform: "macos" | "linux" | "unsupported";
  installed: boolean;
  enabled: boolean;
  pid: number | null;
  definition: string;
  binary: string;
  log: string;
  note: string | null;
}
