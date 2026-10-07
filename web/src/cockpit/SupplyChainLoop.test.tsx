import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { SupplyChainReport } from "../types";
import { SupplyReport, cadenceChoice, cadenceFor, countsLine, parseAllow } from "./SupplyChainLoop";

const report = (over: Partial<SupplyChainReport> = {}): SupplyChainReport => ({
  id: "scr_1",
  started_at: "2026-09-29T09:00:00Z",
  finished_at: "2026-09-29T09:01:00Z",
  dry_run: false,
  trigger: "schedule",
  blocked: false,
  repos: [
    {
      repo: "acme/app",
      sha: "abc",
      scanners: ["cargo-audit"],
      findings: [
        { ecosystem: "cargo", package: "hyper", version: "0.14.28", kind: "vulnerability", severity: "critical", id: "RUSTSEC-2026-0012", title: "Request smuggling", fixed: "0.14.32", fix_available: true, major_bump: false, url: null, lockfile: "Cargo.lock", scanner: "cargo-audit" },
        { ecosystem: "npm", package: "lodash.template", version: null, kind: "vulnerability", severity: "critical", id: "GHSA-35jh", title: "Command Injection", fixed: null, fix_available: false, major_bump: false, url: null, lockfile: "package-lock.json", scanner: "npm audit" },
        { ecosystem: "cargo", package: "atty", version: "0.2.14", kind: "unmaintained", severity: "low", id: null, title: "`atty` is unmaintained", fixed: null, fix_available: false, major_bump: false, url: null, lockfile: "Cargo.lock", scanner: "cargo-audit" },
      ],
      notes: [],
      missing: ["deny.toml sets a licence policy that was not checked: install cargo-deny (cargo install --locked cargo-deny)"],
      error: null,
    },
  ],
  counts: { critical: 2, low: 1 },
  dispatched: [{ repo: "acme/app", ecosystem: "cargo", session: "c0ffee", title: "Supply chain: fix 1 cargo finding (critical at worst)", findings: 1, worst: "critical" }],
  skipped: [{ repo: "acme/app", ecosystem: "npm", reason: "colony beef already targets these findings", findings: 1 }],
  attention: [{ repo: "acme/app", ecosystem: "npm", package: "lodash.template", version: null, id: "GHSA-35jh", severity: "critical", reason: "critical GHSA-35jh: no fixed version is published" }],
  note: null,
  ...over,
});

describe("SupplyChainLoop settings", () => {
  it("reads the allowlist as typed, without duplicates or wildcards", () => {
    expect(parseAllow(" acme, globex/api\nACME  initech/* ")).toEqual(["acme", "globex/api", "initech"]);
    expect(parseAllow("")).toEqual([]);
  });

  it("offers hourly to weekly and keeps anything else as saved", () => {
    const daily = { every: "daily", hour: 6, minute: 17 } as const;
    expect(cadenceChoice(daily)).toBe("daily");
    expect(cadenceChoice({ every: "interval", minutes: 60 })).toBe("hourly");
    expect(cadenceChoice({ every: "interval", minutes: 90 })).toBe("custom");
    expect(cadenceFor("hourly", daily)).toEqual({ every: "interval", minutes: 60 });
    expect(cadenceFor("daily", daily)).toBe(daily);
    expect(cadenceFor("weekly", daily)).toEqual({ every: "weekly", weekday: 0, hour: 6, minute: 17 });
    expect(cadenceFor("custom", { every: "interval", minutes: 90 })).toEqual({ every: "interval", minutes: 90 });
  });
});

describe("SupplyReport", () => {
  it("shows findings by severity, what was dispatched, what was skipped and why, and what needs attention", () => {
    const html = renderToStaticMarkup(<SupplyReport report={report()} now={Date.parse("2026-09-29T10:00:00Z")} />);
    expect(countsLine(report().counts)).toBe("2 critical · 1 low");
    expect(html).toContain("Last run");
    expect(html).toContain("2 critical");
    expect(html).toContain("hyper");
    expect(html).toContain("0.14.32");
    expect(html).toContain("RUSTSEC-2026-0012");
    expect(html).toContain("unmaintained");
    expect(html).toContain("Needs attention");
    expect(html).toContain("no fixed version is published");
    expect(html).toContain("Dispatched");
    expect(html).toContain("Supply chain: fix 1 cargo finding");
    expect(html).toContain("Skipped");
    expect(html).toContain("already targets these findings");
    expect(html).toContain("install cargo-deny");
  });

  it("says a dry run would dispatch, and a blocked run only reports", () => {
    const html = renderToStaticMarkup(<SupplyReport report={report({ dry_run: true, blocked: true })} />);
    expect(html).toContain("Dry run");
    expect(html).toContain("Would dispatch");
    expect(html).toContain("report only");
    const empty = renderToStaticMarkup(<SupplyReport report={report({ repos: [], counts: {}, dispatched: [], skipped: [], attention: [] })} />);
    expect(empty).toContain("no findings");
    expect(countsLine({})).toBe("no findings");
  });
});
