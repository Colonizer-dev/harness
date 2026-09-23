// Org dashboard + redesigned overview markup (issue #398), static only: renderToStaticMarkup
// runs no effects, so the spend-history fetch never fires and every history-backed figure must
// read gracefully as "—" or "no data" rather than crashing.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { OrgEntry } from "../orgs";
import type { Session, SpendHistory } from "../types";
import { OrgDashboard, type ProviderErrorSnapshot } from "./OrgDashboard";

const noop = () => {};

function session(overrides: Partial<Session> = {}): Session {
  return { id: "s1", repo: "acme/webshop", org: "acme", issue: 42, issue_title: "Checkout fails", status: "running", branch: "b", base: "main", parent: null, worktree: "/w", git_admin_dir: null, sandbox: "s", mesh: null, agent: "claude-code", autopilot: false, pr_url: null, error: null, cost_usd: 2, routed_cost_usd: null, cleaned_up: false, keep_worktree: false, created_at: "2026-09-18T09:00:00Z", updated_at: "2026-09-18T09:10:00Z", attention: null, ...overrides };
}

const ACME: OrgEntry = {
  org: "acme",
  live: 2,
  queued: 0,
  total: 3,
  pending: 0,
  avatar: null,
  spend: { cost_usd: 6, routed_cost_usd: null, tokens: { input: 10, output: 5, cache_read: 0, cache_write: 0 }, models: [{ model: "claude-opus", tokens: 15, cost_usd: 6 }] },
};

function histDay(d: string, launched: number, returned: number, cost: number): SpendHistory["days"][number] {
  return { day: d, orgs: [{ org: "acme", cost_usd: cost, routed_cost_usd: null, tokens: { input: 4, output: 1, cache_read: 0, cache_write: 0 }, models: [{ model: "claude-opus", tokens: 5, cost_usd: cost }], launched, returned }] };
}

const history: SpendHistory = { days: [histDay("2026-09-17", 2, 1, 2), histDay("2026-09-18", 1, 2, 4)] };

const orgSessions = () => [
  session({ id: "a", status: "running" }),
  session({ id: "b", status: "merged", repo: "acme/api", issue: 7, cost_usd: 4 }),
  session({ id: "c", status: "pr_opened", repo: "acme/webshop", issue: 9 }),
  session({ id: "d", status: "failed", repo: "acme/api", issue: 8 }),
];

describe("OrgDashboard", () => {
  const render = (h: SpendHistory | null = history, extra: { providers?: ProviderErrorSnapshot[]; initialRepo?: string | null } = {}) =>
    renderToStaticMarkup(<OrgDashboard org={ACME} sessions={orgSessions()} history={h} range={30} compare onBack={noop} {...extra} />);

  it("renders the measured KPIs and names the unmeasured ones once", () => {
    const html = render();
    for (const kpi of ["Merged PRs", "Change failure rate", "Cost per merged PR", "Spend"]) {
      expect(html).toContain(kpi);
    }
    expect(html).toContain("Lead time, PR cycle time, Time to recover, CI pass rate and Coverage are not measured yet");
    expect(html).toContain("API error rate: no data source yet");
    // Real figures: 1 merged of 4 colonies, 1 failed of 4, $6 rollup ÷ 1 merged.
    expect(html).toContain("25% of 4 colonies");
    expect(html).toContain("25.0%");
    expect(html).toContain("1 failed of 4");
    expect(html).toContain("$6.00");
    // No provider tallies by default: the API-error tile stays empty, not zero.
    expect(html).not.toContain("0.00%");
  });

  it("derives API error rate from cumulative provider tallies, labelled as a snapshot", () => {
    const html = render(history, { providers: [{ name: "Strix Halo", requests: 32_689, failures: 9_599, avgLatencyMs: 12_800, since: "2026-03-12T09:00:00Z" }] });
    expect(html).toContain("29.36%");
    expect(html).toContain("since 2026-03-12");
    expect(html).toContain("Strix Halo avg 12.8s");
  });

  it("renders the repository chip row, and a repo filter narrows the whole dashboard", () => {
    const html = render();
    expect(html).toContain('aria-label="Repository"');
    for (const chip of [">all <", "webshop", "api"]) expect(html).toContain(chip);
    const filtered = render(history, { initialRepo: "acme/api" });
    // Only the api colonies remain; the webshop rows are gone from table and list.
    expect(filtered).not.toContain("webshop#9");
    expect(filtered).toContain("api#7");
    // Spend history is per org: the spend panel says it stays org-wide while filtered.
    expect(filtered).toContain("org-wide: spend history is per org");
  });

  it("buckets outcomes by merge day for merged, launch day otherwise, and keeps the CI-green funnel step empty", () => {
    const html = render();
    expect(html).toContain("Colony outcomes");
    expect(html).toContain("merged by merge day, the rest by launch day · current status");
    for (const legend of ["Merged", "PR open", "No changes", "Failed", "Stopped"]) expect(html).toContain(legend);
    expect(html).toContain("Delivery funnel");
    expect(html).toContain("Colonies launched");
    expect(html).toContain("PR opened");
    expect(html).toContain("CI green");
    // The funnel note reads off real statuses: 1 merged of 4 colonies.
    expect(html).toContain("25% of colonies end in a merged PR");
  });

  it("keeps the repositories table and colonies list honest", () => {
    const html = render();
    for (const col of [">Repository<", ">Colonies<", ">Merge rate<", ">Fail<", ">Spend<", ">$ / PR<"]) {
      expect(html).toContain(col);
    }
    expect(html).toContain("Click a row to filter the dashboard");
    expect(html).toMatch(/>Colonies<\/h2><span[^>]*>4</);
    // No CI/coverage API: those cells stay dashes with the reason in the title.
    expect(html).toContain("the API serves no CI results");
    expect(html).toContain("the API serves no coverage");
  });

  it("adds previous-period deltas and sparklines to the measured KPIs", () => {
    const days = ["2026-09-11", "2026-09-12", "2026-09-13", "2026-09-14", "2026-09-15", "2026-09-16", "2026-09-17", "2026-09-18"];
    const h: SpendHistory = { days: days.map((d) => histDay(d, 1, 0, 1)) };
    const list = [
      session({ id: "a", status: "merged", created_at: "2026-09-11T09:00:00Z" }),
      session({ id: "b", status: "merged", created_at: "2026-09-18T09:00:00Z" }),
    ];
    const html = renderToStaticMarkup(<OrgDashboard org={ACME} sessions={list} history={h} range={7} compare onBack={noop} />);
    // 1 merged in range vs 1 before: a flat zero delta, plus the sparkline area.
    expect(html).toContain("0.0%");
    expect(html).toContain('viewBox="0 0 100 28"');
    // The unmeasured KPIs are named, not drawn.
    expect(html).toContain("not measured yet");
  });

  it("renders gracefully with no history: dashes, not crashes", () => {
    const html = render(null);
    expect(html).toContain("← All workspaces");
    expect(html).toContain("Merged PRs");
    expect(html).toContain("no model spend in range");
    expect(html).toContain("no data in range");
    expect(html).toContain("webshop");
  });

  it("renders gracefully with no colonies", () => {
    const html = renderToStaticMarkup(<OrgDashboard org={ACME} sessions={[]} history={null} range={30} compare={false} onBack={noop} />);
    expect(html).toContain("No colonies right now.");
    expect(html).toContain("no colonies in scope");
  });
});
