// The mock's per-call state slice for the loops feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { DiskCleanupReport, Loop, MergeLoopView, NewLoop, NewRedTeamSchedule, RedTeamCadence, RedTeamRun, RedTeamSchedule, SupplyChainLoop, SupplyChainReport, TsAnyLoop, TsAnyReport } from "../../types";
import { ago, now } from "../../mockShared";
import { defaultMergeLoopSettings } from "../../cockpit/mergeLoop";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";

export type LoopsMockState = {
    supplySample: SupplyChainReport;
    supplyLoop: SupplyChainLoop;
    redSchedules: RedTeamSchedule[];
    loopList: Loop[];
    cleanupReport: (dryRun: boolean) => DiskCleanupReport;
    mergeLoop: MergeLoopView;
    loopOf: (body: NewLoop, id: string, created: string, runs?: number) => Loop;
    tsAnySample: TsAnyReport;
    tsAnyLoop: TsAnyLoop;
    scheduleOf: (body: NewRedTeamSchedule, id: string, created: string) => RedTeamSchedule;
    redRuns: RedTeamRun[];
    redActive: (repo: string) => boolean;
};

export function installLoopsMockState(ms: MockState): void {
  // The built-in supply-chain loop: off, with an empty allowlist, and one sample report so the
  // demo has something to show.
  ms.supplySample = {
    id: "scr_demo01",
    started_at: ago(60 * 5),
    finished_at: ago(60 * 5 - 2),
    dry_run: true,
    trigger: "manual",
    blocked: false,
    repos: [
      {
        repo: "acme/webshop",
        sha: "4f2c9a1",
        scanners: ["npm audit", "built-in OSV lookup"],
        findings: [
          { ecosystem: "npm", package: "lodash.template", version: null, kind: "vulnerability", severity: "critical", id: "GHSA-35jh-r3h4-6jhm", title: "Command Injection in lodash.template (affects <=4.5.0)", fixed: null, fix_available: false, major_bump: false, url: "https://github.com/advisories/GHSA-35jh-r3h4-6jhm", lockfile: "package-lock.json", scanner: "npm audit" },
          { ecosystem: "npm", package: "vite", version: null, kind: "vulnerability", severity: "high", id: "GHSA-xxxx-yyyy-zzzz", title: "vite server.fs.deny bypass (affects >=5.0.0 <5.4.12)", fixed: "5.4.12", fix_available: true, major_bump: false, url: null, lockfile: "package-lock.json", scanner: "npm audit" },
          { ecosystem: "npm", package: "semver", version: "7.5.1", kind: "vulnerability", severity: "moderate", id: "GHSA-c2qf-rxjj-qqgw", title: "semver vulnerable to Regular Expression Denial of Service", fixed: "7.5.2", fix_available: true, major_bump: false, url: null, lockfile: "package-lock.json", scanner: "npm audit" },
        ],
        notes: ["no host scanner for Cargo.lock: checked with the built-in OSV lookup; install cargo-audit (cargo install --locked cargo-audit) or osv-scanner for a fuller check"],
        missing: [],
        error: null,
      },
    ],
    counts: { critical: 1, high: 1, moderate: 1 },
    dispatched: [{ repo: "acme/webshop", ecosystem: "npm", session: null, title: "Supply chain: fix 2 npm findings (high at worst)", findings: 2, worst: "high" }],
    skipped: [{ repo: "acme/webshop", ecosystem: null, reason: "not dispatched: 1 with no fixed version", findings: 1 }],
    attention: [{ repo: "acme/webshop", ecosystem: "npm", package: "lodash.template", version: null, id: "GHSA-35jh-r3h4-6jhm", severity: "critical", reason: "critical GHSA-35jh-r3h4-6jhm: no fixed version is published, so no colony can bump past it; replace the package, patch it, or accept the risk" }],
    note: null,
  };
  ms.supplyLoop = {
    name: "Dependencies & supply chain",
    settings: { enabled: false, allow: [], cadence: { every: "daily", hour: 6, minute: 17 }, max_per_repo: 1, max_per_run: 3, cooldown_hours: 12, min_severity: "moderate", outdated: false, builtin: true, autopilot: true },
    next_run_at: null,
    running: false,
    scanners: { "cargo-audit": false, "cargo-deny": false, "npm audit": true, "osv-scanner": false },
    blocked: false,
    last_report: ms.supplySample,
    history: [],
    attention: [],
  };
  ms.redSchedules = [];
  // The built-in disk cleanup (disk_cleanup.rs): every install has it, off until switched on.
  ms.loopList = [
    {
      id: "disk-cleanup",
      name: "Disk cleanup",
      org: "",
      repo: "",
      prompt: "",
      cadence: { every: "interval", minutes: 60 },
      kind: "disk_cleanup",
      tz_offset_minutes: 0,
      model: null,
      subagent_model: null,
      autopilot: false,
      max_runs: null,
      end_at: null,
      enabled: false,
      next_run_at: null,
      runs: 0,
      last_run: null,
      last_note: null,
      ended_reason: null,
      created_at: now(),
      disk_cleanup: {
        settings: { trigger_free_pct: 15, build_output: true, stopped_after_days: 7, worktrees: true, microvms: true, archives: false, archive_keep_days: 30, archive_max_gb: null, host_paths: false, extra_paths: [], host_min_age_days: 3 },
        history: [],
        attention: null,
        previewed_at: null,
      },
    },
  ];
  ms.cleanupReport = (dryRun: boolean): DiskCleanupReport => ({
    at: now(),
    dry_run: dryRun,
    trigger: "manual",
    bytes: 3_435_973_837,
    categories: [
      {
        category: "build_output",
        enabled: true,
        items: [
          { path: "/var/lib/colonizer/worktrees/acme/webshop/old98765/target", bytes: 2_899_102_924, colony: "old98765" },
          { path: "/var/lib/colonizer/worktrees/acme/design-system/merge5678/node_modules", bytes: 536_870_913, colony: "merge5678" },
        ],
        count: 2,
        bytes: 3_435_973_837,
        held: [{ path: "/var/lib/colonizer/worktrees/acme/api/stop4321", reason: "unpushed-commits" }],
      },
      { category: "worktrees", enabled: true, items: [], count: 0, bytes: 0 },
      { category: "microvms", enabled: true, items: [], count: 0, bytes: 0, note: "microVM images are kept: msb has no prune that can tell which images a colony still needs" },
      { category: "archives", enabled: false, items: [], count: 0, bytes: 0 },
      { category: "host_paths", enabled: false, items: [], count: 0, bytes: 0 },
    ],
  });
  ms.mergeLoop = { settings: defaultMergeLoopSettings(), next_run_at: null, running: false, writes_blocked: true, repos: {}, last_report: null, history: [] };
  ms.loopOf = (body: NewLoop, id: string, created: string, runs = 0): Loop => ({
    id,
    name: body.name,
    org: body.repo.split("/")[0],
    repo: body.repo,
    prompt: body.prompt,
    cadence: body.cadence,
    kind: body.kind,
    tz_offset_minutes: body.tz_offset_minutes ?? 0,
    model: body.model ?? null,
    subagent_model: body.subagent_model ?? null,
    autopilot: body.autopilot ?? true,
    max_runs: body.max_runs ?? null,
    end_at: body.end_at ?? null,
    enabled: body.enabled ?? true,
    next_run_at: body.enabled === false ? null : new Date(Date.now() + 3600_000).toISOString(),
    runs,
    last_run: null,
    last_note: null,
    ended_reason: null,
    created_at: created,
  });

  // The built-in TypeScript any loop: off, with an empty allowlist, and one sample report and a
  // short history so the demo has a trend to draw.
  ms.tsAnySample = {
    id: "tsa_demo01",
    started_at: ago(60 * 7),
    finished_at: ago(60 * 7 - 1),
    dry_run: true,
    trigger: "manual",
    blocked: false,
    repos: [
      {
        repo: "acme/webshop",
        sha: "4f2c9a1",
        typescript: true,
        method: "token_scan",
        method_note: "token scan: node_modules/typescript is absent and offline installs are off",
        ts_version: null,
        total: 57,
        implicit: null,
        as_casts: 12,
        suppressions: 3,
        ts_files: 214,
        forms: { annotation: 31, as: 12, array: 6, record: 5, type_argument: 3 },
        modules: [
          { module: "src/api", explicit: 24, files: 5 },
          { module: "src/checkout", explicit: 17, files: 4 },
          { module: "src/lib", explicit: 9, files: 3 },
        ],
        files: [{ path: "src/api/client.ts", module: "src/api", explicit: 11, implicit: null, as_casts: 2, suppressions: 0 }],
        previous: [61, 64],
        notes: [],
        error: null,
      },
    ],
    total: 57,
    dispatched: [{ repo: "acme/webshop", module: "src/api", session: null, title: "TypeScript: remove any in src/api (20 of 24)", occurrences: 20, module_total: 24 }],
    skipped: [],
    checks: [],
    attention: [],
    note: null,
  };
  ms.tsAnyLoop = {
    name: "TypeScript: remove any",
    settings: { enabled: false, allow: [], cadence: { every: "daily", hour: 7, minute: 43 }, batch_cap: 20, max_per_run: 3, cooldown_hours: 20, implicit: false, offline_install: true, autopilot: true },
    next_run_at: null,
    running: false,
    node: true,
    blocked: false,
    last_report: ms.tsAnySample,
    history: [64, 61, 57].reverse().map((total, i) => ({ id: `tsa_h${i}`, at: ago(60 * 24 * i + 60 * 7), trigger: "schedule", total, totals: { "acme/webshop": total }, dispatched: 1, skipped: 0, flagged: 0, summary: `1 TypeScript repository: ${total} explicit any` })),
    attention: [],
    trend: {},
  };
  /** The first time `cadence` fires after `from`, in UTC — the server's rule, month-end clamp included. */
  const nextRun = (cadence: RedTeamCadence, from: Date): string => {
    const at = (y: number, m: number, d: number) => new Date(Date.UTC(y, m, d, cadence.hour, cadence.minute));
    if (cadence.every === "weekly") {
      const weekday = (from.getUTCDay() + 6) % 7;
      const ahead = (cadence.weekday - weekday + 7) % 7;
      for (const extra of [0, 7]) {
        const when = at(from.getUTCFullYear(), from.getUTCMonth(), from.getUTCDate() + ahead + extra);
        if (when > from) return when.toISOString();
      }
    } else {
      for (let k = 0; k < 3; k++) {
        const y = from.getUTCFullYear();
        const m = from.getUTCMonth() + k;
        const last = new Date(Date.UTC(y, m + 1, 0)).getUTCDate();
        const when = at(y, m, Math.min(cadence.day, last));
        if (when > from) return when.toISOString();
      }
    }
    return new Date(from.getTime() + 7 * 86_400_000).toISOString();
  };
  ms.scheduleOf = (body: NewRedTeamSchedule, id: string, created: string): RedTeamSchedule => {
    if (body.repos.length === 0) throw new ApiError("pick at least one repository", 400);
    if (body.hunter && body.hunter !== "swarm") throw new ApiError(`${body.hunter} cannot run as a red-team hunter in this build yet`, 400);
    return {
      id,
      org: body.org,
      repos: [...body.repos],
      hunter: "swarm",
      swarm_size: body.swarm_size ?? 3,
      model: body.model ?? null,
      subagent_model: body.subagent_model ?? null,
      autofix: body.autofix ?? false,
      preset: body.preset ?? "general",
      cadence: body.cadence,
      enabled: body.enabled ?? true,
      next_run_at: nextRun(body.cadence, new Date()),
      last_run_at: null,
      last_result: null,
      created_at: created,
    };
  };
  ms.redRuns = [
    {
      id: "rt-demo1",
      repo: "acme/webshop",
      org: "acme",
      state: "running",
      swarm_size: 3,
      modules: ["checkout", "email"],
      autofix: false,
      hunters: [
        { session_id: "demo1234", title: "Guest checkout regression", module: "checkout", version: "1.2.0", focus: "guest conversion" },
        { session_id: "stall5678", title: "Dark-mode email fuzz", module: "email", version: null, focus: "template injection" },
        { session_id: "red-party1", title: "Discount stacking probe", module: "pricing", version: "0.9.1", focus: "multi-code carts" },
      ],
      counts: { found: 3, validated: 2, rejected: 1, filed: 1, merged: null },
      created_at: ago(40),
      started_at: ago(39),
      ended_at: null,
      gate_reason: null,
      synthesis: null,
    },
    {
      // A finished raid whose synthesis merged the hunters' duplicates (issue #309).
      id: "rt-demo2",
      repo: "acme/webshop",
      org: "acme",
      state: "done",
      swarm_size: 2,
      modules: ["checkout", "email"],
      autofix: false,
      hunters: [
        { session_id: "close0987", title: "Discount stacking probe", module: "pricing", version: "0.9.1", focus: "multi-code carts" },
        { session_id: "old98765", title: "Cart totals rounding", module: "checkout", version: "1.2.0", focus: "money math" },
      ],
      counts: { found: 7, validated: 4, rejected: 2, filed: 3, merged: 4 },
      created_at: ago(1500),
      started_at: ago(1499),
      ended_at: ago(1400),
      gate_reason: null,
      synthesis: {
        state: "done",
        session_id: "synth9f2a",
        report: "/var/lib/colonizer/redteam/rt-demo2/report.json",
        reason: null,
        superseded: ["synth7c01"],
      },
    },
  ];
  ms.redActive = (repo: string) =>
    ms.redRuns.some((r) => r.repo === repo && r.state !== "done" && r.state !== "stopped" && r.state !== "cancelled");

}
