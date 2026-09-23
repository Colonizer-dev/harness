// Org dashboard + redesigned overview markup (issue #398), static only: renderToStaticMarkup
// runs no effects, so the spend-history fetch never fires and every history-backed figure must
// read gracefully as "—" or "no data" rather than crashing.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import type { OrgEntry } from "../orgs";
import type { Session, SpendHistory } from "../types";
import { OrgDashboard } from "./OrgDashboard";
import { OverviewView } from "./OverviewView";

const api = createMockApi();
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
  session({ id: "b", status: "merged", repo: "acme/api", issue: 7 }),
  session({ id: "c", status: "pr_opened", repo: "acme/webshop", issue: 9 }),
];

describe("OrgDashboard", () => {
  const render = (h: SpendHistory | null = history) =>
    renderToStaticMarkup(<OrgDashboard org={ACME} sessions={orgSessions()} history={h} range={30} compare onBack={noop} />);

  it("renders KPIs, the funnel and the repo drill-down from real data", () => {
    const html = render();
    expect(html).toContain("← overview");
    for (const kpi of ["LAUNCHED", "RETURNED", "MERGED", "SPEND", "COST / MERGED PR"]) expect(html).toContain(kpi);
    expect(html).toContain("$6.00"); // org rollup $6 ÷ 1 merged session
    for (const text of ["Colonies launched", "PR opened", "Merged", "acme/webshop", "acme/api", "100%", "OUTCOMES PER DAY", "SPEND PER DAY"]) {
      expect(html).toContain(text);
    }
  });

  it("renders gracefully with no history: dashes, not crashes", () => {
    const html = render(null);
    expect(html).toContain("← overview");
    expect(html).toContain("LAUNCHED");
    expect(html).toContain("no model spend in range");
    expect(html).toContain("acme/webshop");
  });

  it("renders gracefully with no colonies", () => {
    const html = renderToStaticMarkup(<OrgDashboard org={ACME} sessions={[]} history={null} range={30} compare={false} onBack={noop} />);
    expect(html).toContain("No colonies right now.");
  });
});

describe("OverviewView dashboard", () => {
  it("shows the range picker, the KPI strip, the charts and a dashboard entry per org card", () => {
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <OverviewView sessions={orgSessions()} orgs={[ACME]} cost={null} onOpenOrg={noop} onOpenColony={noop} onSelect={noop} />
      </ApiContext.Provider>,
    );
    expect(html).toContain('aria-label="Range"');
    for (const text of [">7d<", ">30d<", ">90d<", "Compare to previous 30d", "SPEND PER DAY", "WORKSPACES COMPARED", "dashboard →"]) {
      expect(html).toContain(text);
    }
    for (const kpi of ["LAUNCHED", "RETURNED", "MERGED", "SPEND", "NEEDS YOU"]) expect(html).toContain(kpi);
  });

  it("hides org cards with no colonies while the toggle is on (the default)", () => {
    const empty: OrgEntry = { ...ACME, org: "empty", live: 0, queued: 0, total: 0, spend: undefined };
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <OverviewView sessions={orgSessions()} orgs={[ACME, empty]} cost={null} onOpenOrg={noop} onOpenColony={noop} onSelect={noop} />
      </ApiContext.Provider>,
    );
    expect(html).toContain("webshop#42");
    expect(html).toContain("dashboard →");
    expect(html).not.toContain("empty");
  });
});
