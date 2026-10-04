// The mock's shared, per-call state (issue #827): the core state plus one slice per feature, kept
// in src/features/<feature>/mockState.ts. Every field is a property on one object so a helper and the
// per-feature methods mutate the same slot, and installing all slices costs one line per feature.
import { MockSession, baseSession } from "./features/sessions/mockSession";
import type { ActivityEntry, Session } from "./types";
import { ago, clone, now, sleep } from "./mockShared";
import { mockQuotaParam } from "./features/host/mock";
import { ApiError } from "./http";
import { mockSplitLabels } from "./features/repos/mock";
import type { MemoryMockState } from "./features/memory/mockState";
import type { RemoteMockState } from "./features/remote/mockState";
import type { FleetMockState } from "./features/fleet/mockState";
import type { ReposMockState } from "./features/repos/mockState";
import type { ChatMockState } from "./features/chat/mockState";
import type { HostMockState } from "./features/host/mockState";
import type { ProvidersMockState } from "./features/providers/mockState";
import type { OrgsMockState } from "./features/orgs/mockState";
import type { ModulesMockState } from "./features/modules/mockState";
import type { SessionsMockState } from "./features/sessions/mockState";
import type { LoopsMockState } from "./features/loops/mockState";
import { installMemoryMockState } from "./features/memory/mockState";
import { installRemoteMockState } from "./features/remote/mockState";
import { installFleetMockState } from "./features/fleet/mockState";
import { installReposMockState } from "./features/repos/mockState";
import { installChatMockState } from "./features/chat/mockState";
import { installHostMockState } from "./features/host/mockState";
import { installProvidersMockState } from "./features/providers/mockState";
import { installOrgsMockState } from "./features/orgs/mockState";
import { installModulesMockState } from "./features/modules/mockState";
import { installSessionsMockState } from "./features/sessions/mockState";
import { installLoopsMockState } from "./features/loops/mockState";

export type CoreMockState = {
    later: <T>(value: () => T, ms?: number) => Promise<T>;
    find: (id: string) => MockSession;
    sessions: Map<string, MockSession>;
    mockId: () => string;
    logActivity: (entry: Omit<ActivityEntry, "seq" | "ts"> & {
        ts?: string;
    }) => void;
    activityLog: ActivityEntry[];
    sourceLabels: () => string[];
};

export type MockState = CoreMockState & MemoryMockState & RemoteMockState & FleetMockState & ReposMockState & ChatMockState & HostMockState & ProvidersMockState & OrgsMockState & ModulesMockState & SessionsMockState & LoopsMockState;

function installCoreMockState(ms: MockState): void {
  ms.sessions = new Map<string, MockSession>();
  ms.mockId = () => Math.random().toString(16).slice(2, 10);
  const demo = new MockSession({
    ...baseSession("demo1234", "acme/webshop", 42, "Checkout fails for guest users"),
    status: "running",
    mesh: { name: "colony-demo1234", ip: "100.64.0.3" },
    // Booted after issue #205, so the overview row's second line has something to read.
    boot_cpus: 4,
    boot_memory: "8G",
    boot_timing: {
      total_ms: 94_320,
      phases: [
        { name: "issue", ms: 240 },
        { name: "git", ms: 1_180 },
        { name: "providers", ms: 310 },
        { name: "mesh-start", ms: 2_050 },
        { name: "image-pull", ms: 1_900 },
        { name: "vm-boot", ms: 86_400 },
        { name: "mesh-join", ms: 1_460 },
        { name: "agentd", ms: 720 },
      ],
    },
    created_at: ago(6),
  });
  const old = new MockSession(
    {
      ...baseSession("old98765", "acme/webshop", 37, "Price rounding in cart totals"),
      status: "pr_opened",
      mesh: null,
      pr_url: "https://github.com/acme/webshop/pull/61",
      needs_rebase: true,
      cost_usd: 1.12,
      created_at: ago(1600),
      updated_at: ago(1560),
    },
    true,
  );
  old.session.updated_at = ago(1560);
  const failed = new MockSession({
    ...baseSession("fail4321", "acme/design-system", 7, "Button focus ring is invisible on dark backgrounds"),
    status: "stopped",
    mesh: null,
    created_at: ago(95),
  });
  failed.seedFailedHistory();
  failed.session.updated_at = ago(93);
  // A colony whose publish failed after committing (issue #85): "Finish PR" completes it on the
  // existing worktree, no resume needed. It sits on a stack two deep — the demo checkout colony
  // below the stalled one — so the stacked-on chip and the "N stacked on this" count have a chain
  // to show without a mothership.
  const stuck = new MockSession({
    ...baseSession("stuck2468", "acme/webshop", 43, "Add dark mode to the order confirmation email"),
    status: "failed",
    // A failure nobody has opened yet (issue #744): the badge counts it until the colony is opened.
    unseen_failure: true,
    mesh: null,
    parent: "stall5678",
    base: "colonizer/issue-43-stall5678",
    error: "publish failed: git push was rejected by the remote",
    publish_stage: "committed",
    created_at: ago(50),
  });
  stuck.seedFailedHistory();
  stuck.log("Publish committed 2 files on colonizer/issue-43-stuck2468");
  stuck.log("Publish failed: git push was rejected by the remote", "error");
  stuck.session.updated_at = ago(48);
  // A burn-down colony the scheduler auto-launched (issue #210): `origin: "burn_down"` is what the
  // real mothership persists, so the colony's "Burn-down" badge and the global stop are exercisable.
  const burn = new MockSession({
    ...baseSession("burn_a1b2c3", "acme/webshop", 61, "Fix the flaky checkout retry"),
    status: "running",
    origin: "burn_down",
    cost_usd: 12.4,
    created_at: ago(3),
  });
  ms.sessions.set(burn.session.id, burn);
  ms.sessions.set(demo.session.id, demo);
  ms.sessions.set(failed.session.id, failed);
  ms.sessions.set(old.session.id, old);
  ms.sessions.set(stuck.session.id, stuck);
  if (mockQuotaParam() === "paused") {
    // The banner's Resume all needs a parked colony to resume: stopped, flagged, worktree kept.
    ms.sessions.get("fail4321")?.patch({ attention: { reason: "provider_quota_exhausted", since: ago(10), nudges: 0 } });
  }

  // A running colony the watchdog nudged, stacked on the demo checkout colony (its base is the
  // demo branch, not main), and a colony in a second org.
  const stalled = new MockSession({
    ...baseSession("stall5678", "acme/webshop", 43, "Add dark mode to the order confirmation email"),
    status: "running",
    mesh: { name: "colony-stall5678", ip: "100.64.0.7" },
    parent: "demo1234",
    base: "colonizer/issue-42-demo1234",
    created_at: ago(28),
  });
  stalled.seedStalled(ms.proposals[0]);
  stalled.session.updated_at = ago(4);
  ms.sessions.set(stalled.session.id, stalled);
  const octo = new MockSession({
    ...baseSession("octo2468", "octocat/hello-world", null, "Refresh the README examples"),
    status: "stopped",
    mesh: null,
    cost_usd: 0.18,
    created_at: ago(320),
    updated_at: ago(300),
  });
  octo.session.updated_at = ago(300);
  ms.sessions.set(octo.session.id, octo);

  // The rest of the lifecycle: a launch waiting for a slot, and a PR that was merged or closed.
  // The queued one is stacked on the failed colony above it, so the queue reads as waiting for the
  // parent colony rather than for a parallelism slot — and it is superseded (issue #673), so the
  // hold banner has something to hold.
  const queued = new MockSession({
    ...baseSession("queue1357", "acme/webshop", 51, "Rate-limit the checkout API"),
    status: "queued",
    mesh: null,
    parent: "stuck2468",
    queued_behind: "stuck2468",
    base: "colonizer/issue-43-stuck2468",
    supply_chain: { package: "lodash", advisory: "ghsa-7fm4-wx8h-p9q3" },
    superseded: {
      by: "merge_w1",
      pr_url: "https://github.com/acme/webshop/pull/71",
      pr: 71,
      title: "Retry failed webhooks with backoff",
      reason: "files",
      at: ago(1500), // shortly after pull/71 merged (a day ago, in the overview seeds below)
      kept: false,
    },
    created_at: ago(2),
  });
  queued.session.updated_at = ago(2);
  ms.sessions.set(queued.session.id, queued);
  const merged = new MockSession({
    ...baseSession("merge5678", "acme/design-system", 12, "Add focus ring tokens for dark mode"),
    status: "merged",
    mesh: null,
    pr_url: "https://github.com/acme/design-system/pull/18",
    cost_usd: 0.87,
    created_at: ago(240),
    merged_at: ago(238),
  });
  merged.session.updated_at = ago(238);
  ms.sessions.set(merged.session.id, merged);
  const closed = new MockSession({
    ...baseSession("close0987", "acme/webshop", 29, "Support multiple discount codes at checkout"),
    status: "closed",
    mesh: null,
    pr_url: "https://github.com/acme/webshop/pull/44",
    cost_usd: 0.64,
    created_at: ago(700),
  });
  closed.session.updated_at = ago(690);
  ms.sessions.set(closed.session.id, closed);

  // Overview dashboard seeds (issue #398): merged PRs spread over the last month so the
  // merged-per-day chart has something honest to stack, a few failures for the change-failure
  // reading, and colonies waiting on answers at staggered waits so the needs-you queue's
  // oldest-first order is visible. Merged seeds carry a merged_at shortly after created_at,
  // so the demo buckets them by merge date.
  const HOUR = 60;
  const DAY = 24 * HOUR;
  const mergedSeeds: Array<{ id: string; repo: string; issue: number; title: string; daysAgo: number; cost: number; pr: number }> = [
    { id: "merge_w1", repo: "acme/webshop", issue: 58, title: "Retry failed webhooks with backoff", daysAgo: 1, cost: 0.84, pr: 71 },
    { id: "merge_w2", repo: "acme/webshop", issue: 57, title: "Guest cart survives sign-in", daysAgo: 2, cost: 1.12, pr: 70 },
    { id: "merge_w2b", repo: "acme/webshop", issue: 59, title: "Scoped tokens for the API", daysAgo: 2, cost: 0.58, pr: 72 },
    { id: "merge_d1", repo: "acme/design-system", issue: 15, title: "Export logo set as SVG", daysAgo: 3, cost: 0.22, pr: 22 },
    { id: "merge_w3", repo: "acme/webshop", issue: 56, title: "Paginate the activity feed", daysAgo: 5, cost: 2.05, pr: 69 },
    { id: "merge_o1", repo: "octocat/hello-world", issue: 9, title: "Refresh the README examples", daysAgo: 6, cost: 0.31, pr: 10 },
    { id: "merge_d2", repo: "acme/design-system", issue: 14, title: "Dark-mode token scale", daysAgo: 8, cost: 0.64, pr: 21 },
    { id: "merge_w4", repo: "acme/webshop", issue: 55, title: "Stricter CSP headers", daysAgo: 11, cost: 1.48, pr: 68 },
    { id: "merge_w5", repo: "acme/webshop", issue: 54, title: "Locale fallback for dates", daysAgo: 15, cost: 0.92, pr: 67 },
    { id: "merge_o2", repo: "octocat/hello-world", issue: 8, title: "Pin base images in CI", daysAgo: 19, cost: 0.18, pr: 9 },
    { id: "merge_d3", repo: "acme/design-system", issue: 13, title: "Skeleton loaders for balances", daysAgo: 24, cost: 0.77, pr: 20 },
    { id: "merge_w6", repo: "acme/webshop", issue: 53, title: "Index on transfers.created_at", daysAgo: 28, cost: 1.9, pr: 66 },
    // Older than the default 30d window, so the compare toggle has a previous period to draw.
    { id: "merge_w7", repo: "acme/webshop", issue: 52, title: "Cache the pricing lookup", daysAgo: 35, cost: 1.05, pr: 65 },
    { id: "merge_d4", repo: "acme/design-system", issue: 11, title: "High-contrast focus states", daysAgo: 45, cost: 0.41, pr: 19 },
    { id: "merge_o3", repo: "octocat/hello-world", issue: 7, title: "Document the webhook secret", daysAgo: 55, cost: 0.12, pr: 8 },
    { id: "merge_w8", repo: "acme/webshop", issue: 51, title: "Trim the session payload", daysAgo: 32, cost: 0.66, pr: 64 },
    { id: "merge_o4", repo: "octocat/hello-world", issue: 6, title: "Bump the actions pins", daysAgo: 35, cost: 0.09, pr: 7 },
    { id: "merge_w9", repo: "acme/webshop", issue: 50, title: "Order confirmation copy", daysAgo: 41, cost: 1.31, pr: 63 },
    { id: "merge_d5", repo: "acme/design-system", issue: 10, title: "Spinner alignment pass", daysAgo: 41, cost: 0.35, pr: 18 },
    { id: "merge_w10", repo: "acme/webshop", issue: 49, title: "Discount code validation", daysAgo: 52, cost: 0.97, pr: 62 },
    { id: "merge_o5", repo: "octocat/hello-world", issue: 5, title: "Fix the broken badge", daysAgo: 52, cost: 0.14, pr: 6 },
  ];
  for (const seed of mergedSeeds) {
    const merged = new MockSession({
      ...baseSession(seed.id, seed.repo, seed.issue, seed.title),
      status: "merged",
      mesh: null,
      pr_url: `https://github.com/${seed.repo}/pull/${seed.pr}`,
      cost_usd: seed.cost,
      created_at: ago(seed.daysAgo * DAY),
      merged_at: ago(Math.max(0, seed.daysAgo * DAY - 300)),
      updated_at: ago(Math.max(0, seed.daysAgo * DAY - 300)),
    });
    ms.sessions.set(merged.session.id, merged);
  }
  const failedSeeds: Array<{ id: string; repo: string; issue: number; title: string; daysAgo: number; error: string }> = [
    { id: "fail_w1", repo: "acme/webshop", issue: 62, title: "Migrate to Postgres 17", daysAgo: 6, error: "colony failed: the migration timed out on staging" },
    { id: "fail_d1", repo: "acme/design-system", issue: 16, title: "Android 15 edge-to-edge", daysAgo: 13, error: "colony failed: checks never reported" },
  ];
  for (const seed of failedSeeds) {
    const failedSeed = new MockSession({
      ...baseSession(seed.id, seed.repo, seed.issue, seed.title),
      status: "failed",
      mesh: null,
      error: seed.error,
      created_at: ago(seed.daysAgo * DAY),
      updated_at: ago(Math.max(0, seed.daysAgo * DAY - 120)),
    });
    ms.sessions.set(failedSeed.session.id, failedSeed);
  }
  // Colonies paused on a question at staggered waits: the needs-you queue sorts oldest first.
  const waitingSeeds: Array<{ id: string; repo: string; issue: number; title: string; waitMinutes: number }> = [
    { id: "wait_w1", repo: "acme/webshop", issue: 63, title: "Checkout copy review before release", waitMinutes: 7 * HOUR },
    { id: "wait_w2", repo: "acme/webshop", issue: 64, title: "Fleet panel: flag stalled peers", waitMinutes: HOUR },
    { id: "wait_o1", repo: "octocat/hello-world", issue: 11, title: "Rotate staging TLS certs", waitMinutes: 24 },
  ];
  for (const seed of waitingSeeds) {
    const since = ago(seed.waitMinutes);
    const waiting = new MockSession({
      ...baseSession(seed.id, seed.repo, seed.issue, seed.title),
      status: "waiting_for_answer",
      mesh: { name: `colony-${seed.id}`, ip: `100.64.0.${20 + seed.id.length}` },
      attention: { reason: "waiting_for_answer", since, nudges: 0 },
      last_activity_at: since,
      created_at: ago(seed.waitMinutes + 180),
      updated_at: since,
    });
    ms.sessions.set(waiting.session.id, waiting);
  }

  ms.find = (id: string): MockSession => {
    const session = ms.sessions.get(id);
    if (!session) throw new ApiError("no such colony", 404);
    return session;
  };
  ms.later = async <T>(value: () => T, ms = 160): Promise<T> => {
    await sleep(ms);
    return clone(value());
  };

  // The activity log (GET /api/activity): outcomes recorded at the transition, and what "you" did.
  // Seeded oldest first so `seq` climbs with time, and shaped like the report that prompted the
  // History redesign: a sweep of stops on the same issue three times over, a run of colonies that
  // found nothing to change, and outcomes whose colony's `updated_at` a later housekeeping write
  // moved (the log keeps the real time; the page must not show them twice).
  ms.activityLog = [];
  let activitySeq = 0;
  ms.logActivity = (entry: Omit<ActivityEntry, "seq" | "ts"> & { ts?: string }) => {
    ms.activityLog.push({ ...entry, seq: ++activitySeq, ts: entry.ts ?? now() });
  };
  {
    type Seed = Omit<ActivityEntry, "seq">;
    const seeds: Seed[] = [];
    const minutesAgo = (at: string) => Math.max(0, (Date.now() - Date.parse(at)) / 60_000);
    const OUTCOME: Partial<Record<Session["status"], string>> = {
      pr_opened: "outcome.pr_opened",
      merged: "outcome.merged",
      closed: "outcome.closed",
      no_changes: "outcome.no_changes",
      stopped: "outcome.stopped",
      failed: "outcome.failed",
      waiting_for_answer: "outcome.question",
    };
    const colonyFields = (s: Session) => ({ org: s.org, repo: s.repo, issue: s.issue, colony: s.id, title: s.issue_title, pr_url: s.pr_url });
    for (const { session: s } of ms.sessions.values()) {
      if (!s.origin) seeds.push({ ts: s.created_at, kind: "colony.launch", actor: "you", via: "cockpit", ...colonyFields(s), pr_url: null });
      const kind = OUTCOME[s.status];
      // Five minutes before its `updated_at`: the write after the outcome (a cleanup, a PR-watch
      // poll) moved the colony's timestamp, not the event's.
      if (kind) seeds.push({ ts: ago(minutesAgo(s.updated_at) + 5), kind, actor: "colony", ...colonyFields(s), detail: s.status === "failed" ? s.error : null });
    }
    const gone = (id: string, repo: string, issue: number | null, title: string) => ({ org: repo.split("/")[0], repo, issue, colony: id, title });
    // 00:16-style sweep: you stopped the same two issues' colonies several times over.
    const sweep: [string, string, number, string][] = [
      ["ca04bc02", "acme/webshop", 44, "Wire the payment method picker into checkout"],
      ["d4ed94b7", "acme/api", 12, "QRIS callback signature check"],
      ["c1c9215b", "acme/webshop", 44, "Wire the payment method picker into checkout"],
      ["bc4f8f51", "acme/api", 12, "QRIS callback signature check"],
      ["34418674", "acme/webshop", 44, "Wire the payment method picker into checkout"],
    ];
    sweep.forEach(([id, repo, issue, title], i) => seeds.push({ ts: ago(16 + i * 0.1), kind: "outcome.stopped", actor: "you", via: "cockpit", ...gone(id, repo, issue, title) }));
    const quiet: [string, number, string][] = [
      ["acme/app", 1215, "Bump the lockfile"],
      ["acme/app", 1211, "Remove the dead feature flag"],
      ["acme/infra", 1878, "Rotate the staging certificate"],
      ["acme/infra", 1881, "Pin the base image"],
      ["acme/infra", 1877, "Tidy the backup cron"],
      ["acme/api", 4503, "Drop the unused index"],
      ["acme/infra", 1879, "Document the restore drill"],
      ["acme/app", 1246, "Translate the settings screen"],
      ["acme/webshop", 3469, "Fix the footer year"],
      ["acme/infra", 1876, "Rename the deploy job"],
      ["acme/app", 1250, "Update the splash asset"],
      ["acme/api", 4499, "Tighten the rate limiter"],
    ];
    quiet.forEach(([repo, issue, title], i) => seeds.push({ ts: ago(17 + i * 0.08), kind: "outcome.no_changes", actor: "colony", ...gone(`nc${i.toString(16).padStart(6, "0")}`, repo, issue, title) }));
    seeds.push(
      { ts: ago(34), kind: "settings.save", actor: "you", via: "cockpit", target: "provider openrouter", section: "providers" },
      { ts: ago(35), kind: "settings.save", actor: "you", via: "cockpit", target: "secret provider-keys:openrouter", section: "secrets" },
      { ts: ago(52), kind: "workspace.disable", actor: "you", via: "cockpit", org: "globex", target: "globex", section: "org:globex" },
      { ts: ago(71), kind: "loop.pause", actor: "you", via: "cockpit", org: "acme", repo: "acme/webshop", target: "Nightly dependency bumps", section: "loops" },
      { ts: ago(95), kind: "redteam.start", actor: "you", via: "cockpit", org: "acme", repo: "acme/webshop", target: "red-team run", section: "redteam" },
      { ts: ago(118), kind: "chat.colony", actor: "you", via: "cockpit", ...gone("c7a91e02", "acme/design-system", null, "Tokens for the new dark palette") },
      { ts: ago(131), kind: "outcome.question", actor: "colony", ...gone("q51f00aa", "acme/webshop", 51, "Currency rounding for IDR") },
      { ts: ago(126), kind: "colony.answer", actor: "you", via: "cockpit", ...gone("q51f00aa", "acme/webshop", 51, "Currency rounding for IDR") },
      { ts: ago(60 * 26), kind: "outcome.pr_opened", actor: "colony", ...gone("p1e0a001", "acme/api", 88, "Idempotency keys on refunds"), pr_url: "https://github.com/acme/api/pull/412" },
      { ts: ago(60 * 26 + 12), kind: "outcome.merged", actor: "colony", ...gone("p1e0a002", "acme/webshop", 39, "Guest checkout email validation"), pr_url: "https://github.com/acme/webshop/pull/58" },
      { ts: ago(60 * 26 + 40), kind: "outcome.failed", actor: "colony", ...gone("f1e0a003", "acme/app", 1203, "Offline queue for tap-to-pay"), detail: "tests failed: 3 of 212 (queue.spec.ts); the colony stopped after its budget" },
      { ts: ago(60 * 27), kind: "settings.save", actor: "you", via: "api", target: "module agent", section: "module:agent" },
      { ts: ago(60 * 27 + 20), kind: "loop.run_now", actor: "you", via: "cockpit", org: "acme", repo: "acme/api", target: "Weekly flaky-test hunt", section: "loops" },
      { ts: ago(60 * 50), kind: "settings.remove", actor: "you", via: "cockpit", target: "provider strix-old", section: "providers" },
      { ts: ago(60 * 51), kind: "workspace.enable", actor: "you", via: "cockpit", org: "acme", target: "acme", section: "org:acme" },
      { ts: ago(60 * 52), kind: "remote.disable", actor: "you", via: "cockpit", target: "remote access", section: "remote" },
    );
    seeds.sort((a, b) => Date.parse(a.ts) - Date.parse(b.ts));
    for (const seedEntry of seeds) ms.logActivity(seedEntry);
  }
  /** The Source module's include labels, as the mothership reads them for filing (github::split_labels). */
  ms.sourceLabels = (): string[] => mockSplitLabels(String(ms.modules.find((m) => m.kind === "source")?.settings?.include_labels ?? ""));
}

export function createMockState(): MockState {
  const ms = {} as MockState;
  installMemoryMockState(ms);
  installCoreMockState(ms);
  installRemoteMockState(ms);
  installFleetMockState(ms);
  installReposMockState(ms);
  installChatMockState(ms);
  installHostMockState(ms);
  installProvidersMockState(ms);
  installOrgsMockState(ms);
  installModulesMockState(ms);
  installSessionsMockState(ms);
  installLoopsMockState(ms);
  return ms;
}
