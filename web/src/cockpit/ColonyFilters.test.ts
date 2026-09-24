import { describe, expect, it } from "vitest";
import type { Session } from "../types";
import { NO_FILTERS, applyColonyFilters, filtersActive } from "./ColonyFilters";

const now = Date.parse("2026-09-24T12:00:00Z");
const s = (over: Partial<Session>): Session =>
  ({
    id: "x",
    repo: "acme/web",
    org: "acme",
    issue: 1,
    issue_title: "Fix the thing",
    status: "running",
    created_at: "2026-09-24T11:00:00Z",
    updated_at: "2026-09-24T11:30:00Z",
    ...over,
  }) as Session;

describe("applyColonyFilters", () => {
  const list = [
    s({ id: "a", summary: "Fix login redirect loop" }),
    s({ id: "b", org: "beta", repo: "beta/api", status: "queued", updated_at: "2026-09-20T00:00:00Z" }),
    s({ id: "c", status: "merged", cost_usd: 6 } as Partial<Session>),
  ];
  it("passes everything with no filters", () => {
    expect(filtersActive(NO_FILTERS)).toBe(false);
    expect(applyColonyFilters(list, NO_FILTERS, now)).toHaveLength(3);
  });
  it("filters by search over the summary, org, bucket, recency and spend", () => {
    expect(applyColonyFilters(list, { ...NO_FILTERS, query: "redirect" }, now).map((x) => x.id)).toEqual(["a"]);
    expect(applyColonyFilters(list, { ...NO_FILTERS, org: "beta" }, now).map((x) => x.id)).toEqual(["b"]);
    expect(applyColonyFilters(list, { ...NO_FILTERS, statuses: new Set(["queued"]) }, now).map((x) => x.id)).toEqual(["b"]);
    expect(applyColonyFilters(list, { ...NO_FILTERS, updated: "24h" }, now).map((x) => x.id)).toEqual(["a", "c"]);
    expect(applyColonyFilters(list, { ...NO_FILTERS, spent: "5" }, now).map((x) => x.id)).toEqual(["c"]);
  });
});
