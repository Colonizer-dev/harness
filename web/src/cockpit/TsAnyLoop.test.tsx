import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { TsAnyReport, TsAnyRun } from "../types";
import { Sparkline, TsAnyReportView, deltaLine, parseTsAnyAllow, trendOf, tsAnyCadenceChoice, tsAnyCadenceFor } from "./TsAnyLoop";

const report = (over: Partial<TsAnyReport> = {}): TsAnyReport => ({
  id: "tsa_1",
  started_at: "2026-09-29T09:00:00Z",
  finished_at: "2026-09-29T09:01:00Z",
  dry_run: false,
  trigger: "schedule",
  blocked: false,
  repos: [
    {
      repo: "acme/app",
      sha: "abc",
      typescript: true,
      method: "typescript",
      method_note: "the repository's TypeScript 5.6.3 (compiler API)",
      ts_version: "5.6.3",
      total: 29,
      implicit: null,
      as_casts: 2,
      suppressions: 0,
      ts_files: 40,
      forms: { annotation: 29 },
      modules: [
        { module: "src/big", explicit: 25, files: 2 },
        { module: "src/small", explicit: 3, files: 1 },
      ],
      files: [],
      previous: [33, 35],
      notes: [],
      error: null,
    },
    { repo: "acme/plain", sha: null, typescript: false, method: null, method_note: null, ts_version: null, total: 0, implicit: null, as_casts: 0, suppressions: 0, ts_files: 0, forms: {}, modules: [], files: [], previous: [], notes: ["no tsconfig.json"], error: null },
  ],
  total: 29,
  dispatched: [{ repo: "acme/app", module: "src/big", session: "c0ffee", title: "TypeScript: remove any in src/big (20 of 25)", occurrences: 20, module_total: 25 }],
  skipped: [{ repo: "acme/app", module: "src/lib", reason: "colony beef already targets this module" }],
  checks: [{ session: "beef", repo: "acme/app", module: "src/lib", pr_url: "https://github.com/acme/app/pull/7", flagged: true, summary: "any in the module 9 → 9: 2 suppression comments added" }],
  attention: [{ repo: "acme/app", module: "src/lib", session: "beef", pr_url: "https://github.com/acme/app/pull/7", problems: ["2 suppression comments added"], reason: "needs review" }],
  note: null,
  ...over,
});

const run = (total: number, i: number): TsAnyRun => ({ id: `r${i}`, at: "2026-09-29T09:00:00Z", trigger: "schedule", total, totals: {}, dispatched: 1, skipped: 0, flagged: 0, summary: "" });

describe("TsAnyLoop settings", () => {
  it("reads the allowlist as typed, without duplicates or wildcards", () => {
    expect(parseTsAnyAllow(" acme, globex/web\nACME  initech/* ")).toEqual(["acme", "globex/web", "initech"]);
    expect(parseTsAnyAllow("")).toEqual([]);
  });

  it("offers hourly to weekly and keeps anything else as saved", () => {
    const daily = { every: "daily", hour: 7, minute: 43 } as const;
    expect(tsAnyCadenceChoice(daily)).toBe("daily");
    expect(tsAnyCadenceChoice({ every: "interval", minutes: 60 })).toBe("hourly");
    expect(tsAnyCadenceChoice({ every: "interval", minutes: 90 })).toBe("custom");
    expect(tsAnyCadenceFor("hourly", daily)).toEqual({ every: "interval", minutes: 60 });
    expect(tsAnyCadenceFor("daily", daily)).toBe(daily);
    expect(tsAnyCadenceFor("weekly", daily)).toEqual({ every: "weekly", weekday: 0, hour: 7, minute: 43 });
  });
});

describe("TsAnyLoop trend", () => {
  it("draws the totals oldest first and says the change", () => {
    const history = [run(29, 0), run(33, 1), run(35, 2)];
    expect(trendOf(history)).toEqual([35, 33, 29]);
    expect(deltaLine(29, [33])).toBe("−4 since the last run");
    expect(deltaLine(30, [29])).toBe("+1 since the last run");
    expect(deltaLine(29, [29])).toBe("unchanged since the last run");
    expect(deltaLine(29, [])).toBe("");
    const svg = renderToStaticMarkup(<Sparkline values={[35, 33, 29]} />);
    expect(svg).toContain("<polyline");
    expect(svg).toContain("trend: 35, 33, 29");
    expect(renderToStaticMarkup(<Sparkline values={[29]} />)).toBe("");
  });
});

describe("TsAnyReportView", () => {
  it("shows the count and its method, the busiest modules, what was dispatched and skipped, and the flagged recount", () => {
    const html = renderToStaticMarkup(<TsAnyReportView report={report()} now={Date.parse("2026-09-29T10:00:00Z")} />);
    expect(html).toContain("Last run");
    expect(html).toContain("29 explicit any");
    expect(html).toContain("TypeScript 5.6.3");
    expect(html).toContain("−4 since the last run");
    expect(html).toContain("src/big");
    expect(html).toContain("25");
    expect(html).toContain("Dispatched");
    expect(html).toContain("TypeScript: remove any in src/big (20 of 25)");
    expect(html).toContain("Skipped");
    expect(html).toContain("already targets this module");
    expect(html).toContain("Needs attention");
    expect(html).toContain("2 suppression comments added");
    expect(html).toContain("Recounted after publishing");
    expect(html).not.toContain("acme/plain");
  });

  it("says a dry run would dispatch, and a blocked run only reports", () => {
    const html = renderToStaticMarkup(<TsAnyReportView report={report({ dry_run: true, blocked: true })} />);
    expect(html).toContain("Dry run");
    expect(html).toContain("Would dispatch");
    expect(html).toContain("report only");
    const tokens = report();
    tokens.repos[0] = { ...tokens.repos[0], method: "token_scan", method_note: "token scan: node is not installed on the host" };
    expect(renderToStaticMarkup(<TsAnyReportView report={tokens} />)).toContain("token scan: node is not installed");
    const empty = renderToStaticMarkup(<TsAnyReportView report={report({ repos: [], total: 0, dispatched: [], skipped: [], checks: [], attention: [] })} />);
    expect(empty).toContain("no TypeScript repository counted");
  });
});
