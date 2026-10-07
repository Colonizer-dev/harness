// The mock's per-call state slice for the loops feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { DiskCleanupReport, Loop, LoopHistoryRun, MergeLoopItem, MergeLoopView, NewLoop, NewRedTeamSchedule, RedTeamCadence, RedTeamRun, RedTeamSchedule, SupplyChainLoop, SupplyChainReport, TsAnyLoop, TsAnyReport } from "../../types";
import { ago, now } from "../../mockShared";
import { mockRuns, type MockLoopShape } from "./mockHistory";
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
    /** Every mock loop's runs for 90 days, oldest first, by history id (issue #1199). */
    loopRunHistory: Record<string, LoopHistoryRun[]>;
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
    settings: { enabled: true, allow: ["acme", "kontinuum-ai"], cadence: { every: "interval", minutes: 360 }, max_per_repo: 1, max_per_run: 3, cooldown_hours: 12, min_severity: "moderate", outdated: false, builtin: true, autopilot: true },
    next_run_at: new Date(Date.now() + 2 * 3_600_000).toISOString(),
    running: false,
    scanners: { "cargo-audit": false, "cargo-deny": false, "npm audit": true, "osv-scanner": false },
    blocked: false,
    last_report: { ...ms.supplySample, dry_run: false, trigger: "schedule", started_at: ago(48), finished_at: ago(47) },
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
      enabled: true,
      next_run_at: new Date(Date.now() + 35 * 60_000).toISOString(),
      runs: 412,
      last_run: { session: "", at: ago(25) },
      last_note: null,
      ended_reason: null,
      created_at: ago(60 * 24 * 40),
      disk_cleanup: {
        settings: { trigger_free_pct: 15, build_output: true, stopped_after_days: 7, worktrees: true, microvms: true, archives: false, archive_keep_days: 30, archive_max_gb: null, host_paths: false, extra_paths: [], host_min_age_days: 3 },
        history: [],
        attention: null,
        previewed_at: ago(60 * 24 * 40),
      },
    },
    ...customLoops(),
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
  ms.loopList[0].disk_cleanup!.history = [{ ...ms.cleanupReport(false), at: ago(25), trigger: "schedule" }];
  ms.mergeLoop = mergeLoopSeed();
  ms.loopRunHistory = seedHistories();
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
    settings: { enabled: true, allow: ["acme/webshop"], cadence: { every: "daily", hour: 7, minute: 43 }, batch_cap: 20, max_per_run: 3, cooldown_hours: 20, implicit: false, offline_install: true, autopilot: true },
    next_run_at: new Date(Date.now() + 5 * 3_600_000).toISOString(),
    running: false,
    node: true,
    blocked: false,
    last_report: { ...ms.tsAnySample, dry_run: false, trigger: "schedule", started_at: ago(60 * 3), finished_at: ago(60 * 3 - 1) },
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

// --- the seeded, busy install the Loops page draws with `?mock=1` (issue #1199) --------------------

/** What a custom loop of the mock looks like: a daily triage, a weekly dependency bump and a paused flaky-test hunt. */
function customLoops(): Loop[] {
  const base = { org: "acme", tz_offset_minutes: 0, model: null, subagent_model: null, autopilot: true, max_runs: null, end_at: null, ended_reason: null, kind: "colony" as const, needs_github: true };
  return [
    {
      ...base,
      id: "loop_triage",
      name: "Triage new issues",
      repo: "acme/webshop",
      prompt: "Triage the issues in /colonizer/github/issues.json: label them, ask for missing reproduction details, close exact duplicates and fix the small, clear ones in one pull request.",
      cadence: { every: "daily", hour: 9, minute: 0 },
      enabled: true,
      next_run_at: new Date(Date.now() + 14 * 3_600_000).toISOString(),
      runs: 34,
      last_run: { session: "old98765", at: ago(60 * 10) },
      last_note: null,
      created_at: ago(60 * 24 * 36),
    },
    {
      ...base,
      id: "loop_deps",
      name: "Keep dependencies current",
      repo: "acme/design-system",
      prompt: "Update outdated dependencies that have no breaking changes, run every check and open one pull request with a short changelog of what moved.",
      cadence: { every: "weekly", weekday: 0, hour: 8, minute: 0 },
      enabled: true,
      next_run_at: new Date(Date.now() + 3 * 86_400_000).toISOString(),
      runs: 9,
      last_run: { session: "close0987", at: ago(60 * 24 * 4) },
      last_note: null,
      needs_github: false,
      created_at: ago(60 * 24 * 70),
    },
    {
      ...base,
      id: "loop_flaky",
      name: "Fix flaky tests from last night's CI",
      repo: "acme/webshop",
      prompt: "Read /colonizer/github/ci-failures.json, find tests that failed there and then passed on a later run without a code change, and fix the flakiness at its cause.",
      cadence: { every: "daily", hour: 7, minute: 0 },
      enabled: false,
      next_run_at: null,
      runs: 21,
      last_run: { session: "stall5678", at: ago(60 * 24 * 12) },
      last_note: "paused",
      created_at: ago(60 * 24 * 50),
    },
  ];
}

const pr = (repo: string, n: number, title: string, action: MergeLoopItem["action"], reason: string): MergeLoopItem => ({ session: `mock-pr-${n}`, pr_url: `https://github.com/${repo}/pull/${n}`, title, action, reason });

const BILLING =
  "GitHub Actions did not start this pull request's checks: the job was not started because recent account payments have failed or your spending limit needs to be increased. Check the 'Billing & plans' section in your settings.";

/** The merge-train loop's last run: three merged, two red, twenty waiting, eleven of them on one billing error. */
function mergeLoopSeed(): MergeLoopView {
  const repos = [
    "kontinuum-ai/kontinuum",
    "acme/webshop",
    "acme/design-system",
    ...Array.from({ length: 18 }, (_, i) => `${i % 3 === 0 ? "kontinuum-ai" : "acme"}/${["api", "docs", "mobile", "billing", "search", "cli"][i % 6]}-${String(i + 1).padStart(2, "0")}`),
  ];
  const kontinuum = Array.from({ length: 11 }, (_, i) => pr("kontinuum-ai/kontinuum", 301 + i, ["Tune the sequencer swing", "Add a Mixolydian preset", "Fix MIDI clock drift", "Smooth the filter sweep", "Quantise live input", "Faster wavetable load", "Export stems as FLAC", "Undo for pattern edits", "Dark theme contrast", "Fix the arpeggiator reset", "Bump the DSP crate"][i], "waiting", BILLING));
  const queue = Array.from({ length: 6 }, (_, i) => pr("acme/api-01", 120 + i, ["Paginate the orders endpoint", "Rate limit login", "Retry webhook delivery", "Cache the price list", "Add the audit log", "Trim the response payload"][i], "waiting", "waiting behind #119, the head of the train"));
  const running = Array.from({ length: 3 }, (_, i) => pr("acme/design-system", 77 + i, ["Button focus ring", "Tooltip arrow", "Table density"][i], "waiting", "its checks are still running (3 of 5 done)"));
  const items: MergeLoopItem[] = [
    pr("acme/webshop", 61, "Price rounding in cart totals", "merged", "green and clean: merged by squash"),
    pr("acme/webshop", 63, "Guest checkout fix", "merged", "green and clean: merged by squash"),
    pr("acme/design-system", 74, "Spacing tokens for dense tables", "merged", "green and clean: merged by squash"),
    pr("acme/api-01", 118, "Move sessions to Redis", "red", "the required check `build` failed: 2 tests failed in src/session/store.test.ts"),
    pr("acme/search-05", 41, "Reindex on schema change", "red", "the required check `lint` failed: 14 errors in src/indexer"),
    ...kontinuum,
    ...queue,
    ...running,
  ];
  const by = (repo: string) => items.filter((i) => i.pr_url.includes(`github.com/${repo}/`));
  const reportRepos = [...new Set(items.map((i) => i.pr_url.split("/").slice(3, 5).join("/")))].map((repo) => ({ repo, main: "green", paused: null, heal: [], items: by(repo) }));
  const report = { started_at: ago(12), finished_at: ago(11), dry_run: false, forced_dry_run: false, stopped: null, api_calls: 188, summary: "merged 3 · updated (CI running) 0 · red 2 · redo dispatched 0 · skipped 0 · waiting 20", lines: [], repos: reportRepos };
  const settings = { ...defaultMergeLoopSettings(), enabled: true, allow: repos, flaky_checks: ["e2e*"], self_heal: true };
  return { settings, next_run_at: new Date(Date.now() + 28 * 60_000).toISOString(), running: false, writes_blocked: false, repos: {}, last_report: report, history: [report] };
}

const SHAPES: MockLoopShape[] = [
  {
    id: "merge-train", every: 60, weights: [35, 12, 8, 45], dispatch: 0.12, cost: [0.3, 1.4], seed: 0x6d657267,
    summary: {
      ok: ["merged 2 · updated 1 · waiting 6", "merged 1 · waiting 9"],
      partial: ["merged 3 · red 2 · waiting 20", "merged 1 · red 1 · waiting 4"],
      failed: ["red 3 · nothing merged", "GitHub Actions is blocked: nothing merged"],
      skipped: ["waiting 8 · nothing ready to merge"],
      running: [],
    },
  },
  {
    id: "supply-chain", every: 360, weights: [70, 12, 6, 12], dispatch: 0.3, cost: [0.6, 1.8], seed: 0x73757070,
    summary: {
      ok: ["3 repositories: no findings", "2 repositories: 1 moderate; dispatched 1"],
      partial: ["3 repositories: 1 critical, 1 high; dispatched 1, skipped 1, 1 needs attention"],
      failed: ["3 repositories: could not read the lockfiles"],
      skipped: ["nothing is opted in"],
      running: [],
    },
  },
  {
    id: "ts-any", every: 1440, weights: [60, 10, 10, 20], dispatch: 0.8, cost: [1.1, 2.7], seed: 0x74736e79,
    summary: {
      ok: ["1 TypeScript repository: 57 explicit any (-4); dispatched 1", "1 TypeScript repository: 61 explicit any; dispatched 1"],
      partial: ["1 TypeScript repository: 64 explicit any; dispatched 1, 1 batch flagged"],
      failed: ["1 TypeScript repository: could not count"],
      skipped: ["1 TypeScript repository: cooldown holds the next batch"],
      running: [],
    },
  },
  {
    id: "docs", every: 1440, weights: [55, 10, 5, 30], dispatch: 0.6, cost: [0.5, 1.4], seed: 0x646f6373,
    summary: {
      ok: ["2 repositories, 2 findings: 1 dispatched, 1 clean", "2 repositories, 0 findings: 2 clean"],
      partial: ["3 repositories, 5 findings: 1 dispatched, 1 skipped, 1 failed"],
      failed: ["2 repositories: could not read them"],
      skipped: ["2 repositories, 3 findings: the cooldown holds both"],
      running: [],
    },
  },
  {
    id: "disk-cleanup", every: 60, weights: [18, 3, 1, 78], dispatch: 0, cost: [0, 0], seed: 0x6469736b,
    summary: {
      ok: ["freed 3.2G (build output 3.1G, worktrees 100M)", "freed 640M (build output 640M)"],
      partial: ["freed 1.1G; the disk is still under 15% free"],
      failed: ["could not remove 3 paths"],
      skipped: ["nothing to clean"],
      running: [],
    },
  },
  {
    id: "loop_triage", every: 1440, weights: [72, 12, 8, 8], dispatch: 1, cost: [0.8, 3.4], seed: 0x74726961,
    summary: { ok: ["PR opened"], partial: ["stopped early"], failed: ["colony failed"], skipped: ["no changes"], running: [] },
  },
  {
    id: "loop_deps", every: 10_080, weights: [78, 10, 4, 8], dispatch: 1, cost: [1.2, 3.8], seed: 0x64657073,
    summary: { ok: ["PR opened"], partial: ["stopped early"], failed: ["colony failed"], skipped: ["no changes"], running: [] },
  },
  {
    id: "loop_flaky", every: 1440, weights: [60, 15, 10, 15], dispatch: 1, cost: [0.7, 2.6], seed: 0x666c616b,
    summary: { ok: ["PR opened"], partial: ["stopped early"], failed: ["colony failed"], skipped: ["no changes"], running: [] },
  },
];

/** The run a card calls "last" says what the loop's own last report says, so the page agrees with itself. */
const LAST: Record<string, { minutes: number; outcome: LoopHistoryRun["outcome"]; summary: string; cost: number }> = {
  "merge-train": { minutes: 12, outcome: "partial", summary: "merged 3 · red 2 · waiting 20", cost: 0 },
  "supply-chain": { minutes: 48, outcome: "partial", summary: "1 critical, 1 high, 1 moderate · dispatched 1, 1 needs attention", cost: 1.24 },
  "ts-any": { minutes: 180, outcome: "ok", summary: "57 explicit any (4 fewer) · dispatched 1", cost: 1.86 },
  docs: { minutes: 180, outcome: "ok", summary: "3 repositories · 1 colony dispatched · 1 skipped", cost: 0.94 },
  "disk-cleanup": { minutes: 25, outcome: "ok", summary: "freed 3.2 GB (build output)", cost: 0 },
};

function seedHistories(): Record<string, LoopHistoryRun[]> {
  const out: Record<string, LoopHistoryRun[]> = {};
  for (const shape of SHAPES) {
    let runs = mockRuns(shape);
    const pin = LAST[shape.id];
    if (pin) {
      const at = Date.now() - pin.minutes * 60_000;
      runs = runs.filter((r) => new Date(r.at).getTime() < at - 30 * 60_000);
      runs.push({ at: new Date(at).toISOString(), trigger: "schedule", outcome: pin.outcome, summary: pin.summary, counts: {}, colonies: pin.cost ? [`mock-${shape.id}-last`] : [], cost_usd: pin.cost });
    }
    // The paused loop stopped twelve days ago.
    out[shape.id] = shape.id === "loop_flaky" ? runs.filter((r) => Date.now() - new Date(r.at).getTime() > 12 * 86_400_000) : runs;
  }
  return out;
}
