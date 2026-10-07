import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Pagination, optionsBy } from "./ListControls";
import { PAGE_SIZE, matchesQuery, pageNumbers, pageOf, pagedReducer, usePagedFilter, type PagedState } from "./paging";
import { COLONY_LIST_ALL, colonyListMatches } from "./ColonyFilters";
import type { Session } from "../types";

interface F {
  kind: string;
}
const start: PagedState<F> = { query: "", filters: { kind: "all" }, page: 0 };

describe("pagedReducer", () => {
  it("moves between pages", () => {
    const s = pagedReducer(start, { type: "page", page: 3 });
    expect(s.page).toBe(3);
    expect(pagedReducer(s, { type: "page", page: -2 }).page).toBe(0);
  });

  it("goes back to page one when the search changes", () => {
    const on3 = pagedReducer(start, { type: "page", page: 3 });
    const searched = pagedReducer(on3, { type: "query", query: "react" });
    expect(searched).toEqual({ query: "react", filters: { kind: "all" }, page: 0 });
    // The same query again is not a change: the page stays.
    const paged = pagedReducer(searched, { type: "page", page: 2 });
    expect(pagedReducer(paged, { type: "query", query: "react" })).toBe(paged);
  });

  it("goes back to page one when a filter changes, keeping the other filters and the search", () => {
    const s: PagedState<{ a: string; b: boolean }> = { query: "x", filters: { a: "all", b: false }, page: 4 };
    expect(pagedReducer(s, { type: "filters", patch: { b: true } })).toEqual({ query: "x", filters: { a: "all", b: true }, page: 0 });
  });

  it("resets search, filters and page together", () => {
    const s: PagedState<F> = { query: "x", filters: { kind: "npm" }, page: 2 };
    expect(pagedReducer(s, { type: "reset", filters: { kind: "all" } })).toEqual(start);
  });
});

describe("pageOf", () => {
  const items = Array.from({ length: 54 }, (_, i) => i);

  it("slices ten rows a page with one-based bounds", () => {
    expect(PAGE_SIZE).toBe(10);
    const first = pageOf(items, 0);
    expect(first.rows).toEqual([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    expect([first.from, first.to, first.total, first.pageCount]).toEqual([1, 10, 54, 6]);
    const last = pageOf(items, 5);
    expect(last.rows).toEqual([50, 51, 52, 53]);
    expect([last.from, last.to]).toEqual([51, 54]);
  });

  it("shows the last page when the list shrinks under the current one", () => {
    const v = pageOf(items.slice(0, 12), 5);
    expect(v.page).toBe(1);
    expect(v.rows).toEqual([10, 11]);
  });

  it("is one empty page when nothing matches", () => {
    expect(pageOf([], 3)).toEqual({ rows: [], total: 0, page: 0, pageCount: 1, from: 0, to: 0 });
  });
});

describe("pageNumbers", () => {
  it("lists every page when there are few", () => {
    expect(pageNumbers(0, 5)).toEqual([0, 1, 2, 3, 4]);
  });

  it("elides the middle of a long run, keeping the ends and the neighbours", () => {
    expect(pageNumbers(0, 20)).toEqual([0, 1, 2, 3, 4, null, 19]);
    expect(pageNumbers(10, 20)).toEqual([0, null, 9, 10, 11, null, 19]);
    expect(pageNumbers(19, 20)).toEqual([0, null, 15, 16, 17, 18, 19]);
  });
});

describe("usePagedFilter", () => {
  interface Row {
    name: string;
    kind: string;
  }
  const rows: Row[] = Array.from({ length: 25 }, (_, i) => ({ name: `pkg-${i}`, kind: i % 5 === 0 ? "cargo" : "npm" }));
  const match = (r: Row, q: string, f: F) => (f.kind === "all" || r.kind === f.kind) && matchesQuery(q, r.name);

  function Probe({ kind }: { kind: string }) {
    const list = usePagedFilter(rows, { filters: { kind }, match });
    return (
      <p>
        {list.rows.map((r) => r.name).join(",")}|{list.total}|{list.pageCount}
      </p>
    );
  }

  it("filters, then shows the first page of what matches", () => {
    expect(renderToStaticMarkup(<Probe kind="all" />)).toBe("<p>pkg-0,pkg-1,pkg-2,pkg-3,pkg-4,pkg-5,pkg-6,pkg-7,pkg-8,pkg-9|25|3</p>");
    expect(renderToStaticMarkup(<Probe kind="cargo" />)).toBe("<p>pkg-0,pkg-5,pkg-10,pkg-15,pkg-20|5|1</p>");
  });

  it("matches the search against any field, case-insensitively", () => {
    expect(matchesQuery("", "x")).toBe(true);
    expect(matchesQuery("web/", "react", null, "Web/bun.lock")).toBe(true);
    expect(matchesQuery("vue", "react", undefined)).toBe(false);
  });
});

describe("list controls", () => {
  it("names the range and offers the pages", () => {
    const html = renderToStaticMarkup(<Pagination view={pageOf(Array.from({ length: 54 }, (_, i) => i), 1)} onPage={() => {}} noun="packages" />);
    expect(html).toContain("11–20 of 54 packages");
    expect(html).toContain('aria-label="previous page"');
    expect(html).toMatch(/aria-current="page"[^>]*>2</);
  });

  it("draws only the count when everything fits on one page, and nothing when empty", () => {
    const one = renderToStaticMarkup(<Pagination view={pageOf([1, 2, 3], 0)} onPage={() => {}} noun="risks" />);
    expect(one).toContain("3 risks");
    expect(one).not.toContain("next page");
    expect(renderToStaticMarkup(<Pagination view={pageOf([], 0)} onPage={() => {}} />)).toBe("");
  });

  it("builds filter options per distinct value, most common first", () => {
    const opts = optionsBy([{ r: ["a/x", "a/y"] }, { r: ["a/x"] }, { r: ["a/x", "a/x"] }], (i) => i.r, (k) => k.split("/")[1]);
    expect(opts).toEqual([
      { value: "a/x", label: "x", count: 3 },
      { value: "a/y", label: "y", count: 1 },
    ]);
  });
});

describe("colonyListMatches", () => {
  const s = (over: Partial<Session>): Session => ({ id: "1", repo: "acme/web", issue: 12, issue_title: "Fix checkout", status: "running", agent: "claude", ...over }) as Session;

  it("filters by status, repository and agent, and searches title, repository and issue", () => {
    expect(colonyListMatches(s({}), "", COLONY_LIST_ALL)).toBe(true);
    expect(colonyListMatches(s({ status: "failed" }), "", { ...COLONY_LIST_ALL, status: "running" })).toBe(false);
    expect(colonyListMatches(s({}), "", { ...COLONY_LIST_ALL, repo: "acme/api" })).toBe(false);
    expect(colonyListMatches(s({}), "", { ...COLONY_LIST_ALL, agent: "codex" })).toBe(false);
    expect(colonyListMatches(s({}), "checkout", COLONY_LIST_ALL)).toBe(true);
    expect(colonyListMatches(s({}), "#12", COLONY_LIST_ALL)).toBe(true);
    expect(colonyListMatches(s({}), "acme/web", COLONY_LIST_ALL)).toBe(true);
    expect(colonyListMatches(s({}), "billing", COLONY_LIST_ALL)).toBe(false);
  });
});
