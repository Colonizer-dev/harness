import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { EcoIcon, Freshness, PackagesView, filterDependencies, filterRisks, fixInstructions, publishedMatches, type PublishedFilters } from "./PackagesView";
import type { Dependency, PublishedPackage, SupplyRisk } from "../types";
import { ApiContext, ToastProvider } from "../context";
import { createMockApi } from "../mock";

const dep = (name: string, over: Partial<Dependency> = {}): Dependency => ({
  ecosystem: "npm",
  name,
  direct: true,
  dev: false,
  latest: null,
  outdated: false,
  vulnerable: false,
  drift: false,
  versions: [{ version: "1.0.0", behind: false, users: [{ repo: "acme/web", path: "bun.lock" }], vulns: [] }],
  ...over,
});

const risk = (over: Partial<SupplyRisk> = {}): SupplyRisk => ({
  severity: "high",
  kind: "vulnerability",
  ecosystem: "npm",
  name: "lodash",
  version: "4.17.20",
  reason: "GHSA-1: prototype pollution",
  fix: { available: true, version: "4.17.21" },
  url: "https://osv.dev/vulnerability/GHSA-1",
  direct: false,
  via: ["some-lib"],
  users: [{ repo: "acme/web", path: "bun.lock" }],
  ...over,
});

describe("Packages tab", () => {
  it("shows a cached answer's age and a running refresh instead of the scanning state", () => {
    const at = new Date(Date.now() - 5 * 60_000).toISOString();
    const refreshing = renderToStaticMarkup(<Freshness data={{ cached_at: at, refreshing: true }} onRefresh={() => {}} />);
    expect(refreshing).toContain("updated 5m ago");
    expect(refreshing).toContain("refreshing");
    expect(refreshing).toMatch(/<button[^>]* disabled=""[^>]*>Refresh<\/button>/);
    const settled = renderToStaticMarkup(<Freshness data={{ cached_at: at, refreshing: false }} onRefresh={() => {}} />);
    expect(settled).not.toContain("refreshing");
    expect(settled).not.toMatch(/ disabled=""/);
  });

  it("filters dependencies by ecosystem, directness, freshness, advisories and name", () => {
    const list = [
      dep("react", { outdated: true }),
      dep("lodash", { direct: false, vulnerable: true }),
      dep("serde", { ecosystem: "cargo" }),
    ];
    const base = { eco: "all" as const, directOnly: false, outdated: false, vulnerable: false, q: "" };
    expect(filterDependencies(list, base)).toHaveLength(3);
    expect(filterDependencies(list, { ...base, eco: "cargo" }).map((d) => d.name)).toEqual(["serde"]);
    expect(filterDependencies(list, { ...base, directOnly: true }).map((d) => d.name)).toEqual(["react", "serde"]);
    expect(filterDependencies(list, { ...base, outdated: true }).map((d) => d.name)).toEqual(["react"]);
    expect(filterDependencies(list, { ...base, vulnerable: true }).map((d) => d.name)).toEqual(["lodash"]);
    expect(filterDependencies(list, { ...base, q: "LOD" }).map((d) => d.name)).toEqual(["lodash"]);
  });

  it("filters risks by severity, fixability and kind", () => {
    const list = [risk(), risk({ severity: "low", kind: "missing-integrity", fix: { available: false } })];
    expect(filterRisks(list, { severity: "all", fixable: false, kind: "all" })).toHaveLength(2);
    expect(filterRisks(list, { severity: "high", fixable: false, kind: "all" })).toHaveLength(1);
    expect(filterRisks(list, { severity: "all", fixable: true, kind: "all" })).toHaveLength(1);
    expect(filterRisks(list, { severity: "all", fixable: false, kind: "missing-integrity" })[0].severity).toBe("low");
  });

  it("searches and filters published packages by status, ecosystem, repository and unreleased changes", () => {
    const pkg = (name: string, over: Partial<PublishedPackage> = {}): PublishedPackage => ({
      ecosystem: "npm", name, version: "1.0.0", repo: "acme/web", path: "packages/sdk", private: false, registry: null, status: "published", unreleased_changes: false, published: null, ...over,
    });
    const list = [pkg("@acme/sdk", { unreleased_changes: true }), pkg("pwa", { status: "private", path: "apps/pwa" }), pkg("harness", { ecosystem: "cargo", repo: "acme/harness", path: "crates/h" })];
    const all: PublishedFilters = { status: "all", eco: "all", repo: "all", unreleased: false };
    const names = (q: string, f: Partial<PublishedFilters>) => list.filter((p) => publishedMatches(p, q, { ...all, ...f })).map((p) => p.name);
    expect(names("", {})).toHaveLength(3);
    expect(names("", { status: "private" })).toEqual(["pwa"]);
    expect(names("", { eco: "cargo" })).toEqual(["harness"]);
    expect(names("", { repo: "acme/web" })).toEqual(["@acme/sdk", "pwa"]);
    expect(names("", { unreleased: true })).toEqual(["@acme/sdk"]);
    expect(names("web/apps", {})).toEqual(["pwa"]);
    expect(names("sdk", {})).toEqual(["@acme/sdk"]);
  });

  it("filters dependencies and risks by repository, and risks by ecosystem and search", () => {
    const deps = [dep("react"), dep("serde", { ecosystem: "cargo", versions: [{ version: "1", behind: false, users: [{ repo: "acme/harness", path: "Cargo.lock" }], vulns: [] }] })];
    const base = { eco: "all" as const, directOnly: false, outdated: false, vulnerable: false, q: "" };
    expect(filterDependencies(deps, { ...base, repo: "acme/harness" }).map((d) => d.name)).toEqual(["serde"]);
    expect(filterDependencies(deps, { ...base, q: "harness/cargo" }).map((d) => d.name)).toEqual(["serde"]);
    const risks = [risk(), risk({ name: "serde_yaml", ecosystem: "cargo", users: [{ repo: "acme/harness", path: "Cargo.lock" }] })];
    const f = { severity: "all" as const, fixable: false, kind: "all" };
    expect(filterRisks(risks, { ...f, repo: "acme/harness" }).map((r) => r.name)).toEqual(["serde_yaml"]);
    expect(filterRisks(risks, { ...f, eco: "npm" }).map((r) => r.name)).toEqual(["lodash"]);
    expect(filterRisks(risks, { ...f, q: "Prototype" })).toHaveLength(2);
    expect(filterRisks(risks, { ...f, q: "yaml" }).map((r) => r.name)).toEqual(["serde_yaml"]);
  });

  it("hands a colony the package, the reason, where it is used and what to upgrade", () => {
    const text = fixInstructions(risk());
    expect(text).toContain('npm package "lodash" 4.17.20');
    expect(text).toContain("prototype pollution");
    expect(text).toContain("bun.lock (acme/web)");
    expect(text).toContain("via some-lib");
    expect(text).toContain("Upgrade it to 4.17.21 or later");
    expect(text).toContain("Do not add any Claude/AI attribution");
  });

  it("marks each ecosystem and starts with the published view", () => {
    expect(renderToStaticMarkup(<EcoIcon eco="cargo" />)).toContain('aria-label="crates.io"');
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={createMockApi()}>
        <ToastProvider>
          <PackagesView org="acme" />
        </ToastProvider>
      </ApiContext.Provider>,
    );
    expect(html).toContain('aria-selected="true"');
    expect(html).toContain("Published");
    expect(html).toContain("Supply chain");
  });
});
