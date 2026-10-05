// The `host` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { ago, ahead, clone, isLive, now, sleep } from "../../mockShared";
import type { HarnessStatus, HeadroomStatus, LoginItemStatus, PullStatus, RetentionPlan, RuntimeInfo, TelemetryStatus, UpdateStatus, UsageStatus } from "../../types";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { HostApi } from "./api";

// The same digest pins the real backend boots (crates/colonizer/images.lock), so the mock
// shows references in exactly the shape the app produces.
export const MOCK_PRESET_IMAGES: Record<string, string> = {
  // auto has no image of its own: it reads each repository's stack at boot and falls back to Node,
  // so the pre-pull resolves to the same pinned image node names.
  auto: "node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0",
  node: "node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0",
  python: "python:3.13-bookworm@sha256:933b46a028fd786c9c3d426ebabc237e29a15912231ea8de576e95f0e4f41a4c",
  rust: "rust:1-bookworm@sha256:9a73a5088750b4c95158ab26629c854c3d6fc4b173cb7bc8079ad252d8ed7bfa",
  go: "golang:1-bookworm@sha256:648f440f42a0958804efb24df176f806f9d353b41f1c0627f666428e40310f6b",
};
export const mockPulled = new Set<string>(["node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0"]);
export let mockPull: PullStatus = { image: "", state: "idle", started_at: null, finished_at: null, error: null };
// Headroom's bundle: a few seconds of download progress, then installed.
export let mockHeadroom: HeadroomStatus = { release: "0.37.0-1", state: "idle", bytes: 0, total: null, started_at: null, finished_at: null, error: null };

// The live map: not asked yet, so the prompt shows.
/** `?mesh=unavailable` models a Mac; `?mesh=error` models a mesh that actually failed. */
export const mockMeshParam = () => {
  try {
    return new URLSearchParams(location.search).get("mesh");
  } catch {
    return null;
  }
};

/**
 * `?runtime=` models the machines Setup has to tell apart (issue #129): `mac` is an Apple-silicon
 * Mac, `kvm` a Linux box whose `/dev/kvm` this user cannot use, `old` a mothership from before it
 * probed the machine at all, `other` a platform Setup calls unsupported (issue #214) — it also
 * carries the existing demo colonies, so the nest being replaced by Settings (before the fix) and
 * staying put (after) is reproducible in one load. Anything else — the default — is a healthy Linux box.
 */
export const mockRuntimeParam = () => {
  try {
    return new URLSearchParams(location.search).get("runtime");
  } catch {
    return null;
  }
};

export function mockRuntime(): RuntimeInfo | undefined {
  const linux = (kvm: RuntimeInfo["kvm"]): RuntimeInfo => ({
    platform: "linux-x86_64",
    kvm,
    git: { ok: true, version: "2.45.0" },
    gh: { ok: true, version: "2.60.0" },
    host_claude_bin: "/usr/local/bin/claude",
    host_claude_bin_error: null,
    os: { vendor: "ubuntu", name: "Ubuntu", version: "24.04", id: "ubuntu" },
  });
  switch (mockRuntimeParam()) {
    case "mac":
      return {
        platform: "darwin-arm64",
        kvm: null, // libkrun needs none on a Mac; the mothership reports null off Linux
        git: { ok: true, version: "2.52.0" },
        gh: { ok: true, version: "2.60.0" },
        host_claude_bin: "/Users/you/.local/bin/claude",
        host_claude_bin_error: null,
        os: { vendor: "apple", name: "macOS", version: "14.5", id: null },
      };
    case "kvm":
      return linux({ ok: false, error: "/dev/kvm: Permission denied" });
    case "old":
      return undefined;
    case "other":
      // A platform Setup must call unsupported (issue #214) — e.g. ARM Linux, which the
      // platform gate blocks the same way a KVM-less x86 box does; KVM reads null, as it
      // does wherever the mothership does not probe it.
      return {
        platform: "other",
        kvm: null,
        git: { ok: true, version: "2.45.0" },
        gh: { ok: true, version: "2.60.0" },
        host_claude_bin: "/usr/local/bin/claude",
        host_claude_bin_error: null,
        os: { vendor: "unknown", name: "Other", version: null, id: null },
      };
    default:
      return linux({ ok: true, error: null });
  }
}

/**
 * GET /api/status `host` (issue #205): the machine every colony boots on, with every number the
 * strip reads. `?runtime=` carries over the way it does for `mockRuntime`: a Mac is not Linux so
 * `kvm_ok` is omitted, `?runtime=kvm` is a host whose KVM this user cannot use, and anything else
 * is a healthy Linux box. `live` is the running-colony count, so the strip's paid/total reading
 * tracks the rest of the mock's state.
 */
export function mockHost(live: number): HarnessStatus["host"] {
  const kvm =
    mockRuntimeParam() === "mac" ? {} : mockRuntimeParam() === "kvm" ? { kvm_ok: false } : { kvm_ok: true };
  return {
    id: "1e6f2a84-c5b3-4f2a-9f1c-8d4e2a1b6c90",
    hostname: "archlinux",
    cpu_cores: 8,
    memory_total_bytes: 34_359_738_368, // 32G
    memory_used_bytes: 17_179_869_184, // 16G
    load: [0.42, 0.38, 0.31],
    uptime_secs: 273_600, // 3d 4h
    disk_total_bytes: 549_755_813_888, // 512G
    disk_used_bytes: 373_662_154_752, // 348G
    disk_free_bytes: 176_093_659_136, // 164G
    checked_at: now(),
    microvms_live: live,
    microvms_ceiling: 3,
    ...kvm,
  };
}
/**
 * GET /api/status `quota` (issue #404): null on a healthy mothership, which is the default.
 * `?quota=paused` parks the queue behind the BytePlus plan's limit (the subagent and background
 * roles route there) so the cockpit's global banner is exercisable — and flags the mock's stopped
 * colony quota-parked to match, so Resume all has something to resume. `?quota=account` pauses on
 * the Claude account's own session limit instead, with no colony parked yet.
 */
export const mockQuotaParam = () => {
  try {
    return new URLSearchParams(location.search).get("quota");
  } catch {
    return null;
  }
};

export function mockQuota(): HarnessStatus["quota"] {
  const param = mockQuotaParam();
  // Two hours and ten minutes out, so the banner's countdown reads like the real thing.
  const resetUnix = Math.floor(Date.now() / 1000) + 2 * 3600 + 10 * 60;
  const resetAt = new Date(resetUnix * 1000).toISOString().replace("T", " ").slice(5, 16) + " UTC";
  if (param === "account") {
    return {
      paused: true,
      reason: `queue paused — Claude account quota exhausted, resets ${resetAt} (0 waiting)`,
      reset_at: resetAt,
      reset_unix: resetUnix,
      providers: [],
      kind: "account",
      provider_details: [{ id: "anthropic", name: "Claude", used_by: ["orchestrator"] }],
    };
  }
  if (param !== "paused") return null;
  return {
    paused: true,
    reason: `queue paused — BytePlus plan exhausted, resets ${resetAt} (1 waiting)`,
    reset_at: resetAt,
    reset_unix: resetUnix,
    providers: ["byteplus"],
    kind: "provider",
    provider_details: [{ id: "byteplus", name: "BytePlus", used_by: ["subagents", "background"] }],
  };
}

/** The mesh payload for this load. A Mac vendors no tailscaled, so its mesh is `unavailable` by
 *  design (#32) and must never read as a fault (#128): `?runtime=mac` implies it unless `?mesh=`
 *  says otherwise, and `?mesh=error` stays a genuine failure. */
export function mockMesh(nodes: number): HarnessStatus["mesh"] {
  const param = mockMeshParam();
  const kind = param === "error" ? "error" : param === "unavailable" || mockRuntimeParam() === "mac" ? "unavailable" : "running";
  switch (kind) {
    case "error":
      return {
        enabled: true,
        provider: "headscale",
        state: "error",
        harness_ip: null,
        nodes: 0,
        error: "headscale did not start: address already in use",
      };
    case "unavailable":
      return {
        enabled: true,
        provider: "headscale",
        state: "unavailable",
        harness_ip: null,
        nodes: 0,
        detail: "colonies use a loopback port on this platform",
        error: null,
      };
    case "running":
      return { enabled: true, provider: "headscale", state: "running", harness_ip: "100.64.0.1", nodes, error: null };
  }
}

export const MOCK_LATEST = {
  version: "v0.1.4",
  url: "https://github.com/Colonizer-dev/harness/releases/tag/v0.1.4",
  notes: "- Colonies keep their worktree when the Mothership restarts\n- Settings shows which Claude account is connected",
  published_at: "2026-09-17T17:21:32Z",
};

// An install one release behind, so the update banner can be seen in the mock.
export let mockUpdate: UpdateStatus = {
  enabled: true,
  blocked_by: null,
  installed: { version: "v0.1.3", commit: "abc1234def5678", dirty: false, built_at: "2026-09-05T09:20:00Z", release: "v0.1.3" },
  latest: MOCK_LATEST,
  available: true,
  last_checked: "2026-09-17T18:00:00Z",
  error: null,
  can_apply: { ok: true, reason: null },
  apply: { phase: "idle", version: null, started_at: null, error: null, log: "", colonies: [], backup: null },
};

export let mockTelemetry: TelemetryStatus = {
  enabled: null,
  blocked_by: null,
  endpoint: "https://telemetry.colonizer.dev",
  map_url: "https://colonizer.dev/live",
  last_sent_at: null,
  last_error: null,
  heartbeat: { install_id: null, version: "0.1.3", platform: "darwin-arm64", colonies: 1 },
};

// The usage batch: reporting is on by default, so `enabled` arrives already resolved to true — the
// "never answered" distinction lives only in usage.json and is not exposed over the API. The batch
// is Cratefield's module-telemetry payload; it is sent at most once a day, only when the Mothership
// was given a collector endpoint.
export let mockLoginItem = false;
export let mockUsage: UsageStatus = {
  enabled: true,
  blocked_by: null,
  payload_version: 1,
  batch: {
    schema: 1,
    install: "0f8a6c1e4d2b4a9e9c3f5b7d1e2a6c48",
    client: { kind: "server", version: "0.1.3", platform: "macos", arch: "aarch64" },
    modules: ["mothership"],
    events: [
      { name: "colonies.parallel_now.1", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "colonies.pr_opened.2-3", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "colonies.no_changes.0", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "colonies.stopped.1", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "colonies.failed.0", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "sandbox.preset.node", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "sandbox.image_changed.false", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "autopilot.enabled.true", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "autopilot.held.0", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "setting.agent.model", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "setting.sandbox.preset", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "boot.issue.<1s", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "boot.vm-boot.5-15s", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "boot.agentd.1-2s", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "providers.1", outcome: "ok", error: "none", duration: "unknown", count: 1 },
      { name: "error.vm_stopped.1", outcome: "ok", error: "none", duration: "unknown", count: 1 },
    ],
  },
};

export function hostMock(ms: MockState): HostApi {
  return {
    status: () =>
      ms.later(() => {
    const live = [...ms.sessions.values()].filter((s) => isLive(s.session.status)).length;
    return {
      github: { connected: true, login: "octocat", name: "The Octocat", avatar_url: "https://avatars.githubusercontent.com/u/583231?v=4&s=64", source: ms.githubSource },
      claude: ms.claude,
      sandbox: { msb_version: "msb 0.6.18", image: "node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0", cpus: 4, memory: "8G", max_parallel: 3, claude_bin: "/opt/claude/bin/claude", claude_bin_error: null },
      mesh: mockMesh(live + 1),
      // ?runtime=mac models the Mac end to end: no KVM, and a mesh that is unavailable
      // by design, which Setup must keep green (#128, #129). ?runtime=old sends no
      // runtime at all, as a mothership from before the probe did not.
      runtime: mockRuntime(),
      // The host strip's numbers (issue #205); ?runtime=kvm shows a host whose KVM the
      // user cannot use, so the strip reads "no KVM".
      host: mockHost(live),
      // Reclamation counts for the sidebar's Storage dot (issue #223).
      reclaim: { reclaimable: 2, unpushed: 1 },
      // The drain flag (issue #880): off by default; ?draining=1 could model an update in flight.
      draining: false,
      // Disk health for the sidebar's Storage dot (issue #220): plenty free, so neither
      // low_disk nor admission_paused. ?runtime=old omits storage with the rest, as a
      // mothership from before the probe did not.
      storage: {
        ok: true,
        free_bytes: 12_884_901_888,
        warn_free_bytes: 5_368_709_120,
        min_free_bytes: 1_073_741_824,
        low_disk: false,
        admission_paused: false,
      },
      // The same verdict the providers list serves, read off its own seeds: strix is the
      // degraded one (issue #184's report), deepseek and lab have never been used.
      model_providers: ms.providers.map((p) => ({
        id: p.id,
        name: p.name,
        requests: p.usage?.requests ?? 0,
        failure_pct: p.health?.failure_pct ?? 0,
        avg_latency_ms: p.health?.avg_latency_ms ?? 0,
        degraded: p.health?.degraded ?? false,
      })),
      // The cockpit's global plan-limit banner (issue #404); null by default, paused behind the
      // BytePlus plan under `?quota=paused` and the Claude account under `?quota=account`.
      quota: mockQuota(),
    };
      }),
    sandboxPull: async () => {
      const sandbox = ms.modules.find((m) => m.kind === "sandbox");
      const preset = String(sandbox?.settings?.preset ?? "auto");
      const image = String(sandbox?.settings?.image ?? MOCK_PRESET_IMAGES[preset] ?? "node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0");
      if (mockPulled.has(image)) {
    mockPull = { image, state: "cached", started_at: null, finished_at: null, error: null };
    return clone(mockPull);
      }
      if (mockPull.state !== "pulling" || mockPull.image !== image) {
    mockPull = { image, state: "pulling", started_at: new Date().toISOString(), finished_at: null, error: null };
    const started = mockPull.started_at;
    setTimeout(() => {
      if (mockPull.image !== image || mockPull.started_at !== started) return;
      mockPulled.add(image);
      mockPull = { ...mockPull, state: "done", finished_at: new Date().toISOString() };
    }, 4000);
      }
      return clone(mockPull);
    },
    sandboxPullStatus: async () => clone(mockPull),
    applyUpdate: async () => {
      await sleep(200);
      const live = [...ms.sessions.values()].map((s) => s.session).filter((s) => isLive(s.status));
      // The real one drains first — holding the queue while the colonies still booting or publishing
      // finish, a publish being waited for rather than refused (issue #880) — then installs and
      // restarts. The mock just reports the phases.
      mockUpdate = {
    ...mockUpdate,
    apply: {
      phase: "draining",
      version: mockUpdate.latest?.version ?? null,
      started_at: new Date().toISOString(),
      error: null,
      log: "",
      colonies: live.map((s) => ({ id: s.id, repo: s.repo, outcome: "reconnected after the restart" })),
      backup: null,
    },
      };
      // The drain finishes (nothing is really in flight here), then the process is replaced.
      setTimeout(() => {
    mockUpdate = { ...mockUpdate, apply: { ...mockUpdate.apply, phase: "installing" } };
      }, 1500);
      setTimeout(() => {
    mockUpdate = { ...mockUpdate, apply: { ...mockUpdate.apply, phase: "restarting", log: "==> installed Colonizer" } };
      }, 2500);
      return { started: true };
    },
    headroom: async () => {
      if (mockHeadroom.state === "downloading" && mockHeadroom.started_at) {
    const total = 231_330_241;
    const bytes = Math.min(total, Math.round(((Date.now() - Date.parse(mockHeadroom.started_at)) / 5000) * total));
    mockHeadroom = bytes >= total ? { ...mockHeadroom, state: "installed", bytes, total, finished_at: new Date().toISOString() } : { ...mockHeadroom, bytes, total };
      }
      return clone(mockHeadroom);
    },
    headroomDownload: async () => {
      if (mockHeadroom.state === "idle" || mockHeadroom.state === "failed") {
    mockHeadroom = { ...mockHeadroom, state: "downloading", bytes: 0, total: 231_330_241, started_at: new Date().toISOString(), finished_at: null, error: null };
      }
      return clone(mockHeadroom);
    },
    update: async () => clone(mockUpdate),
    setUpdateCheck: async (enabled) => {
      await sleep(200);
      // Matches the backend: switching off forgets the last answer, so no banner
      // lingers for a check that is no longer running.
      mockUpdate = enabled
    ? { ...mockUpdate, enabled, latest: MOCK_LATEST, available: true, last_checked: new Date().toISOString() }
    : { ...mockUpdate, enabled, latest: null, available: false, last_checked: null, error: null };
      return clone(mockUpdate);
    },
    telemetry: async () => clone(mockTelemetry),
    setTelemetry: async (enabled) => {
      await sleep(250);
      const install_id = enabled ? (mockTelemetry.heartbeat.install_id ?? crypto.randomUUID()) : null;
      mockTelemetry = {
    ...mockTelemetry,
    enabled,
    last_sent_at: enabled ? new Date().toISOString() : mockTelemetry.last_sent_at,
    heartbeat: { ...mockTelemetry.heartbeat, install_id },
      };
      return clone(mockTelemetry);
    },
    usage: async () => clone(mockUsage),
    loginItem: () =>
      ms.later(() => ({ platform: "macos", installed: mockLoginItem, enabled: mockLoginItem, pid: mockLoginItem ? 4242 : null, definition: "~/Library/LaunchAgents/dev.colonizer.mothership.plist", binary: "~/.local/bin/colonizer", log: "~/.local/share/colonizer/mothership.out", note: null }) as LoginItemStatus),
    setLoginItem: (enabled) =>
      ms.later(() => {
        mockLoginItem = enabled;
        return { platform: "macos", installed: enabled, enabled, pid: enabled ? 4242 : null, definition: "~/Library/LaunchAgents/dev.colonizer.mothership.plist", binary: "~/.local/bin/colonizer", log: "~/.local/share/colonizer/mothership.out", note: null } as LoginItemStatus;
      }),
    setUsage: async (enabled) => {
      await sleep(250);
      if (mockUsage.blocked_by) throw new ApiError("usage reporting is kept off by the Mothership's environment", 409);
      // The batch is untouched by the switch: the id it carries is minted, kept and rotated by the
      // Mothership, and switching off here stops the sender, it does not rewrite the shown batch.
      mockUsage = { ...mockUsage, enabled };
      return clone(mockUsage);
    },
    archive: () =>
      ms.later(() => ({
        root: "/var/lib/colonizer/archive",
        count: ms.archiveEntries.length,
        bytes: ms.archiveEntries.reduce((total, e) => total + e.bytes, 0),
        entries: clone(ms.archiveEntries),
      })),
    archiveRetention: async (body): Promise<RetentionPlan> => {
      await sleep(200);
      // Negative or non-finite limits are a bad request, not a plan that silently keeps everything.
      const sane = (v: number | null) => v == null || (Number.isFinite(v) && v >= 0);
      if (!sane(body.keep_days) || !sane(body.max_gb)) throw new ApiError("keep_days and max_gb must be finite and not negative", 400);
      const cutoff = body.keep_days != null ? Date.now() - body.keep_days * 86_400_000 : null;
      const cap = body.max_gb != null ? body.max_gb * 1e9 : null;
      let total = ms.archiveEntries.reduce((t, e) => t + e.bytes, 0);
      const picked = [...ms.archiveEntries]
        .sort((a, b) => Date.parse(a.archived_at) - Date.parse(b.archived_at))
        .filter((e) => {
          const take = (cutoff != null && Date.parse(e.archived_at) < cutoff) || (cap != null && total > cap);
          if (take) total -= e.bytes;
          return take;
        });
      const planned = picked.map((e) => ({ bundle: e.bundle, session: e.session, bytes: e.bytes, archived_at: e.archived_at }));
      const remove: RetentionPlan["remove"] = body.allow_single_copy ? planned : [];
      const plan: RetentionPlan = {
        dry_run: body.dry_run,
        remove,
        count: remove.length,
        bytes: remove.reduce((t, r) => t + r.bytes, 0),
        kept_single_copy: body.allow_single_copy ? 0 : planned.length,
      };
      if (body.dry_run) return plan;
      const expect = body.expect;
      if (expect == null) throw new ApiError("applying retention needs expect: the bundle list the preview returned", 400);
      if (expect.length !== remove.length || remove.some((r, i) => r.bundle !== expect[i])) {
        throw new ApiError("the archive changed since the preview; re-run the dry run and apply its own expect", 409);
      }
      const gone = new Set(remove.map((r) => r.bundle));
      for (let i = ms.archiveEntries.length - 1; i >= 0; i--) if (gone.has(ms.archiveEntries[i].bundle)) ms.archiveEntries.splice(i, 1);
      return plan;
    },
    storageSummary: () =>
      ms.later(() => ({
    enabled: true,
    retention_secs: 43200,
    min_free_bytes: 1_073_741_824,
    warn_free_bytes: 5_368_709_120,
    free_bytes: 12_884_901_888,
    admission_paused: false,
    totals: {
      worktrees_bytes: 3_221_225_472,
      repos_bytes: 1_073_741_824,
      sessions_bytes: 268_435_456,
      // The two seeded archive bundles (issue #496), under <data_dir>/archive.
      archive_bytes: 5_242_880 + 2_621_440,
      // Microsandbox's home directory, holding the shared image cache: listed, never offered for cleanup.
      microsandbox_bytes: 2_147_483_648,
    },
    reclaimable: [
      { id: "old98765", status: "pr_opened", pr_url: "https://github.com/acme/webshop/pull/61", bytes: 214_748_364, updated_at: ago(1560), due: true },
      { id: "merge5678", status: "merged", pr_url: "https://github.com/acme/design-system/pull/18", bytes: 96_468_992, updated_at: ago(238), due: false },
    ],
    unpushed: [{ id: "fail4321", status: "stopped", bytes: 41_943_040, updated_at: ago(93) }],
    orphans: [{ path: "worktrees/acme/webshop/issue-9-orphan", bytes: 12_582_912, action: "reclaim at retention" }],
      })),
    setGithubToken: async (token) => {
      await sleep(300);
      if (!token.trim() || /\s/.test(token.trim())) throw new ApiError("empty or malformed token", 400);
      ms.githubSource = "saved token";
      return { login: "octocat" };
    },
    deleteGithubToken: async () => {
      ms.githubSource = "gh CLI login";
      return { ok: true };
    },
    setClaudeToken: async (token) => {
      if (!token.trim().startsWith("sk-ant-")) {
    throw new ApiError("expected a token from `claude setup-token` (sk-ant-oat…) or an API key (sk-ant-api…)", 400);
      }
      const apiKey = token.trim().startsWith("sk-ant-api");
      ms.claude = {
    configured: true,
    source: apiKey ? "saved API key" : "Claude subscription",
    kind: apiKey ? "ANTHROPIC_API_KEY" : "CLAUDE_CODE_OAUTH_TOKEN",
    account: null,
    account_note: apiKey
      ? "an API key does not identify an account"
      : "Anthropic does not resolve a `claude setup-token` to an account, so the account behind this token cannot be shown.",
    saved_at: now(),
    expires_at: apiKey ? null : ahead(365),
    expires_estimated: !apiKey,
      };
      return { ok: true };
    },
    deleteClaudeToken: async () => {
      ms.claude = { configured: false, source: null, kind: null, account: null, account_note: null, saved_at: null, expires_at: null, expires_estimated: false };
      return { ok: true };
    },
    claudeLogin: () => ms.later(() => ms.login, 60),
    claudeLoginStart: async () => {
      ms.login = { state: "starting", url: null, message: null };
      setTimeout(() => {
    if (ms.login.state === "starting") {
      ms.login = { state: "awaiting_code", url: "https://claude.com/cai/oauth/authorize?code=true&client_id=mock", message: null };
    }
      }, 800);
      return clone(ms.login);
    },
    claudeLoginCode: async (code) => {
      if (!/^[\x21-\x7e]+$/.test(code.trim())) throw new ApiError("that doesn't look like a sign-in code", 400);
      if (ms.login.state !== "awaiting_code") throw new ApiError("no Claude sign-in is waiting for a code", 409);
      ms.login = { ...ms.login, state: "verifying" };
      setTimeout(() => {
    ms.login = { state: "done", url: null, message: "Connected your Claude subscription" };
    ms.claude = {
      configured: true,
      source: "Claude subscription",
      kind: "CLAUDE_CODE_OAUTH_TOKEN",
      account: null,
      account_note: "Anthropic does not resolve a `claude setup-token` to an account, so the account behind this token cannot be shown.",
      saved_at: now(),
      expires_at: ahead(365),
      expires_estimated: true,
    };
      }, 1200);
      return clone(ms.login);
    },
    claudeLoginCancel: async () => {
      ms.login = { state: "idle", url: null, message: null };
      return clone(ms.login);
    }
  };
}
