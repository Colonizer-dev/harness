// The overview's host strip and colony rows read /api/status's `host` object and a colony's boot
// fields (issue #205). The renderer is dumb; these pure derivations own the shapes: a formatter
// always yields a number, and a host or colony with unmeasured inputs loses its segment outright —
// never a zero, never an "undefined".
import { describe, expect, it } from "vitest";

import { colonyFacts, formatBootMs, formatBytes, formatLoad, formatUptime, hostFacts } from "./host";
import type { HostInfo, Session } from "../types";

const FULL_HOST: HostInfo = {
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
  checked_at: "2026-09-20T09:00:00Z",
  microvms_live: 2,
  microvms_ceiling: 3,
  kvm_ok: true,
};

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "s1",
    repo: "acme/webshop",
    org: "acme",
    issue: 42,
    issue_title: "Checkout fails for guest users",
    status: "running",
    branch: "colonizer/issue-42-s1",
    base: "main",
    parent: null,
    worktree: "/wt/s1",
    git_admin_dir: "/git/s1",
    sandbox: "colony-s1",
    mesh: null,
    agent: "claude-code",
    autopilot: false,
    pr_url: null,
    error: null,
    cost_usd: null,
    cleaned_up: false,
    created_at: "2026-09-18T09:00:00Z",
    updated_at: "2026-09-18T09:10:00Z",
    attention: null,
    ...overrides,
  };
}

describe("formatUptime", () => {
  it("dates, hours, minutes and bare seconds", () => {
    expect(formatUptime(273_600)).toBe("3d 4h");
    expect(formatUptime(18_720)).toBe("5h 12m");
    expect(formatUptime(90)).toBe("1m 30s");
    expect(formatUptime(42)).toBe("42s");
  });

  it("reads zero as fresh", () => {
    expect(formatUptime(0)).toBe("0s");
  });
});

describe("formatLoad", () => {
  it("takes the 1-minute average at two decimals", () => {
    expect(formatLoad([0.4242, 0.38, 0.31])).toBe("0.42");
    expect(formatLoad([0.5, 0.4, 0.3])).toBe("0.50");
  });
});

describe("formatBootMs", () => {
  it("reads milliseconds as whole seconds", () => {
    expect(formatBootMs(94_000)).toBe("94s");
    expect(formatBootMs(1_450)).toBe("1s");
    expect(formatBootMs(5_700)).toBe("6s");
  });
});

describe("formatBytes", () => {
  it("is the app's compact G/M/K shape, like the colony host footprint", () => {
    expect(formatBytes(0)).toBe("0B");
    expect(formatBytes(512)).toBe("512B");
    expect(formatBytes(64 * 1024)).toBe("64K");
    expect(formatBytes(2 * 1024 ** 2)).toBe("2M");
    expect(formatBytes(3.5 * 1024 ** 3)).toBe("3.5G");
  });
});

describe("hostFacts", () => {
  it("renders every measured fact in order", () => {
    expect(hostFacts(FULL_HOST).map((f) => ({ icon: f.icon, value: f.value }))).toEqual([
      { icon: "server", value: "2/3" },
      { icon: "cpu", value: "8c · 0.42" },
      { icon: "memory", value: "16G/32G" },
      { icon: undefined, value: "disk 164G/512G" },
      { icon: undefined, value: "up 3d 4h" },
    ]);
  });

  it("drops every segment whose inputs the host could not measure", () => {
    const minimal: HostInfo = { id: "h", checked_at: "2026-09-20T09:00:00Z", microvms_live: 0, microvms_ceiling: 3 };
    expect(hostFacts(minimal).map((f) => f.value)).toEqual(["0/3"]);
  });

  it("needs both sides of a pair before showing it", () => {
    const usedOnly: HostInfo = { id: "h", checked_at: "2026-09-20T09:00:00Z", microvms_live: 0, microvms_ceiling: 3, memory_used_bytes: 1, disk_free_bytes: 1 };
    const values = hostFacts(usedOnly).map((f) => f.value);
    expect(values).toEqual(["0/3"]);
    expect(values.some((v) => v.includes("disk") || v.includes("M") || v.includes("G"))).toBe(false);
  });

  it("shows only the parts of the cpu segment that exist", () => {
    const coresOnly: HostInfo = { id: "h", checked_at: "2026-09-20T09:00:00Z", microvms_live: 0, microvms_ceiling: 3, cpu_cores: 8 };
    expect(hostFacts(coresOnly).map((f) => f.value)).toEqual(["0/3", "8c"]);
    const loadOnly: HostInfo = { id: "h", checked_at: "2026-09-20T09:00:00Z", microvms_live: 0, microvms_ceiling: 3, load: [0.42, 0.38, 0.31] };
    expect(hostFacts(loadOnly).map((f) => f.value)).toEqual(["0/3", "0.42"]);
  });
});

describe("colonyFacts", () => {
  it("lists the microVM, the boot, the mesh address and the agent", () => {
    const facts = colonyFacts(
      session({
        boot_cpus: 4,
        boot_memory: "8G",
        boot_timing: { total_ms: 94_000 },
        mesh: { name: "colony-s1", ip: "100.64.0.3" },
      }),
    );
    expect(facts.map((f) => ({ icon: f.icon, value: f.value }))).toEqual([
      { icon: "cpu", value: "4c · 8G" },
      { icon: undefined, value: "boot 94s" },
      { icon: undefined, value: "mesh 100.64.0.3" },
      { icon: undefined, value: "claude-code" },
    ]);
  });

  it("keeps a pre-change colony to what it has always had", () => {
    // No boot fields: nothing about the boot may render. The mesh is gone once a colony stops,
    // and only the agent remains — which it always had.
    expect(colonyFacts(session()).map((f) => f.value)).toEqual(["claude-code"]);
    expect(colonyFacts(session({ mesh: { name: "colony-s1", ip: "100.64.0.3" } })).map((f) => f.value)).toEqual([
      "mesh 100.64.0.3",
      "claude-code",
    ]);
  });

  it("shows the boot without the seated microVM, and vice versa", () => {
    expect(colonyFacts(session({ boot_timing: { total_ms: 4_120 } })).map((f) => f.value)).toContain("boot 4s");
    expect(colonyFacts(session({ boot_cpus: 2 })).map((f) => f.value)).toContain("2c");
  });
});