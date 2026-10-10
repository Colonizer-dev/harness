// The Queues view's pure logic (issue #1127): grouping, the reused wait wording, and the bulk
// outcome reducer — plain values in, plain values out.
import { describe, expect, it } from "vitest";

import { groupRows, reasonLabel, reasonTone, summarizeActions, type ActionResult } from "./model";
import type { QueuesRow } from "./types";

let n = 0;
const row = (over: Partial<QueuesRow> = {}): QueuesRow => ({
  id: `q${++n}`,
  org: "acme",
  repo: "acme/webshop",
  issue: n,
  title: `Work ${n}`,
  branch: null,
  host: "build-box",
  agent: "claude-code",
  status: "queued",
  created_at: "2026-10-09T09:00:00Z",
  priority: 0,
  reason: null,
  detail: null,
  attention: null,
  resumes_at: null,
  held: false,
  policy_hold: false,
  actions: { resume: true, stop: true, restart: false },
  why_not: {},
  ...over,
});

describe("groupRows", () => {
  const rows = [
    row({ id: "a", host: "build-box", reason: null }),
    row({ id: "b", host: null, reason: "provider_quota_exhausted", status: "parked", org: "acme", repo: "acme/api" }),
    row({ id: "c", host: "gpu-lab", reason: null }),
    row({ id: "d", host: null, reason: "provider_quota_exhausted", status: "parked", org: "acme", repo: "acme/infra" }),
  ];

  it("keeps one group holding everything when grouping is off", () => {
    const groups = groupRows(rows, "none");
    expect(groups).toHaveLength(1);
    expect(groups[0].rows).toEqual(rows);
  });

  it("groups by host, a missing host reading as unassigned, in first-appearance order", () => {
    expect(groupRows(rows, "host").map((g) => [g.key, g.rows.map((r) => r.id)])).toEqual([
      ["build-box", ["a"]],
      ["unassigned", ["b", "d"]],
      ["gpu-lab", ["c"]],
    ]);
  });

  it("groups by machine wait reason, null reading as queued", () => {
    expect(groupRows(rows, "reason").map((g) => g.key)).toEqual(["queued", "provider_quota_exhausted"]);
  });

  it("groups by org/repo", () => {
    expect(groupRows(rows, "repo").map((g) => g.key)).toEqual(["acme/webshop", "acme/api", "acme/infra"]);
  });

  it("labels a reason group in the maps' words, not the machine string", () => {
    const labels = groupRows(rows, "reason").map((g) => g.label);
    expect(labels).toEqual(["Queued", "provider quota exhausted"]);
  });
});

describe("reasonLabel", () => {
  it("speaks the park-reason map's wording and appends the resume time", () => {
    const label = reasonLabel(row({ reason: "repo_pr_rate_limit", resumes_at: "2026-10-10T00:00:00Z" }));
    expect(label).toMatch(/repo's daily PR cap reached/);
    expect(label).toMatch(/resumes /);
  });

  it("falls back to the raw reason, spelled out, when the map does not know it", () => {
    expect(reasonLabel(row({ reason: "something_new" }))).toBe("something new");
  });

  it("reads an attention row through attentionText", () => {
    expect(reasonLabel(row({ attention: { reason: "stalled", since: "2026-10-09T08:00:00Z", nudges: 3 } }))).toBe("No progress, nudged 3×");
  });

  it("reads a plain row as its status label", () => {
    expect(reasonLabel(row({ status: "queued", reason: null }))).toBe("Queued");
    expect(reasonLabel(row({ status: "waiting_for_answer" }))).toBe("Needs your answer");
  });
});

describe("reasonTone", () => {
  it("warns for parked and attention rows, waits for info, the rest neutral", () => {
    expect(reasonTone(row({ status: "parked", reason: "idle_timeout" }))).toBe("warn");
    expect(reasonTone(row({ attention: { reason: "stalled", since: "x", nudges: 1 } }))).toBe("warn");
    expect(reasonTone(row({ status: "queued" }))).toBe("info");
    expect(reasonTone(row({ status: "waiting_for_answer" }))).toBe("info");
    expect(reasonTone(row({ status: "blocked" }))).toBe("neutral");
  });
});

describe("summarizeActions", () => {
  const results: ActionResult[] = [
    { id: "a", ok: true },
    { id: "b", ok: false, error: "it is already running" },
    { id: "c", ok: true },
    { id: "d", ok: false },
  ];

  it("splits done from refused and keeps the server's words", () => {
    expect(summarizeActions(results)).toEqual({
      done: ["a", "c"],
      refused: [
        { id: "b", error: "it is already running" },
        { id: "d", error: "it could not be done" },
      ],
    });
  });

  it("is all done when nothing was refused", () => {
    expect(summarizeActions([{ id: "a", ok: true }])).toEqual({ done: ["a"], refused: [] });
  });
});
