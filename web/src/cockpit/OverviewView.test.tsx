// The overview page's relationship between the header total and the org cards (issue #209): when a
// mothership reports per-org spend, the header is the sum of the rows (never a session-derived
// figure that could disagree), and an org whose spend was never measured reads "—", never "$0.00".
// renderToStaticMarkup runs no effects, so the sparkline fetch never fires and the org cards simply
// have no history — which is exactly the older-mothership path.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import type { OrgEntry } from "../orgs";
import { OverviewView } from "./OverviewView";

const api = createMockApi();

const entry = (org: string, spend: OrgEntry["spend"]): OrgEntry => ({
  org,
  live: 0,
  queued: 0,
  total: 0,
  pending: 0,
  avatar: null,
  spend,
});

const render = (orgs: OrgEntry[], cost: number | null) =>
  renderToStaticMarkup(
    <ApiContext.Provider value={api}>
      <OverviewView sessions={[]} orgs={orgs} cost={cost} onOpenOrg={() => {}} onOpenColony={() => {}} />
    </ApiContext.Provider>,
  );

const measured = (costUsd: number, routedCostUsd: number): OrgEntry["spend"] => ({
  cost_usd: costUsd,
  routed_cost_usd: routedCostUsd,
  tokens: { input: 1, output: 1, cache_read: 0, cache_write: 0 },
  models: [],
});

const unmeasured: OrgEntry["spend"] = {
  cost_usd: null,
  routed_cost_usd: null,
  tokens: { input: 1, output: 1, cache_read: 0, cache_write: 0 },
  models: [],
};

describe("OverviewView spend", () => {
  it("derives the header from the org rows, matching their sum and ignoring the session-based cost prop", () => {
    const html = render([entry("acme", measured(12.5, 0.5)), entry("globex", measured(2, 0))], 999);
    // 12.5 + 0.5 + 2 = 15. The `cost` prop of 999 must not win.
    expect(html).toContain("$15.00 spent");
    expect(html).not.toContain("$999.00");
  });

  it("shows — on each org, and no $0.00, when every org is a never-measured subscription", () => {
    const html = render([entry("acme", unmeasured)], null);
    expect(html).toContain("—");
    expect(html).not.toContain("$0.00");
  });

  it("falls back to the sessions-derived cost when no org carries server spend", () => {
    const html = render([entry("acme", undefined)], 4.5);
    expect(html).toContain("$4.50 spent");
    expect(html).toContain("acme");
  });

  it("falls back to the sessions-derived cost unless every org carries server spend", () => {
    const html = render([entry("acme", measured(12.5, 0.5)), entry("globex", undefined)], 7.25);
    // 12.5 + 0.5 = 13 from the row with spend, but a sibling without spend means the server is not
    // reporting a complete rollup, so the sessions-derived `cost` of 7.25 must win.
    expect(html).toContain("$7.25 spent");
    expect(html).not.toContain("$13.00 spent");
  });
});