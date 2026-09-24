import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import type { HarnessStatus, HostInfo } from "../types";
import { HostView, allocation } from "./HostView";
import { appendSample, parseSize, sampleOf } from "./hostHistory";
import { session } from "./testFixtures";

const host: HostInfo = {
  id: "h-1",
  hostname: "studio",
  cpu_cores: 14,
  memory_total_bytes: 64 * 1024 ** 3,
  memory_used_bytes: 48 * 1024 ** 3,
  load: [7, 5.5, 4],
  uptime_secs: 90_000,
  disk_total_bytes: 1000,
  disk_used_bytes: 950,
  disk_free_bytes: 50,
  checked_at: "2026-09-24T06:00:00Z",
  microvms_live: 3,
  microvms_ceiling: 5,
};

describe("host samples", () => {
  it("reads load against cores and used against total", () => {
    const s = sampleOf(host);
    expect(s.cpu).toBeCloseTo(0.5);
    expect(s.memory).toBeCloseTo(0.75);
    expect(s.disk).toBeCloseTo(0.95);
    expect(sampleOf({ ...host, load: undefined, memory_total_bytes: undefined }).cpu).toBeNull();
  });

  it("keeps one sample per probe, newest last, capped", () => {
    const a = sampleOf(host);
    const once = appendSample([], a);
    expect(appendSample(once, a)).toBe(once);
    const many = [1, 2, 3, 4].reduce((h, i) => appendSample(h, { ...a, at: i }, 3), [] as ReturnType<typeof appendSample>);
    expect(many.map((s) => s.at)).toEqual([2, 3, 4]);
  });
});

describe("parseSize", () => {
  it("reads the sandbox's memory shapes", () => {
    expect(parseSize("8G")).toBe(8 * 1024 ** 3);
    expect(parseSize("512M")).toBe(512 * 1024 ** 2);
    expect(parseSize("2048")).toBe(2048 * 1024 ** 2);
    expect(parseSize("lots")).toBeNull();
    expect(parseSize(null)).toBeNull();
  });
});

describe("allocation", () => {
  it("sums what the live colonies booted with and counts the ones without sizes", () => {
    const a = allocation([
      session({ id: "a", status: "running", boot_cpus: 6, boot_memory: "12G" }),
      session({ id: "b", status: "waiting_for_answer", boot_cpus: 2, boot_memory: "4G" }),
      session({ id: "c", status: "running", boot_cpus: null, boot_memory: null }),
      session({ id: "d", status: "merged", boot_cpus: 6, boot_memory: "12G" }),
    ]);
    expect(a).toEqual({ colonies: 3, cpus: 8, memory: 16 * 1024 ** 3, unknown: 1 });
  });
});

describe("HostView", () => {
  const api = { storageSummary: () => new Promise(() => {}) } as unknown as Api;
  const render = (status: HarnessStatus | null) =>
    renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <HostView status={status} sessions={[session({ status: "running", boot_cpus: 6, boot_memory: "12G" })]} onOpenColony={() => {}} />
      </ApiContext.Provider>,
    );

  it("names the machine and reads its resources", () => {
    const html = render({ host } as HarnessStatus);
    expect(html).toContain(">studio</h1>");
    expect(html).toContain("CPU load");
    expect(html).toContain("7.00 · 5.50 · 4.00 (1/5/15 min) · 14 cores");
    expect(html).toContain("6 of 14 cores");
    expect(html).toContain("3 of 5");
    expect(html).toContain("the mothership keeps no host history");
  });

  it("says so while there is no host reading", () => {
    expect(render(null)).toContain("Waiting for the mothership");
  });
});
