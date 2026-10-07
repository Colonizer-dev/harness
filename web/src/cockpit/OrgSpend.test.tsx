// The org-card spend block (issue #209): "unmeasured" renders as "—" — never "$0.00" — tokens are
// compact, the model mix caps at three with the rest counted, and the sparkline keeps a zero-height
// slot for gap days rather than a bar. Rendered to static markup, as the cockpit's tests do.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { OrgSpend as OrgSpendData } from "../types";
import { OrgSpend, type OrgSpendHistoryDay } from "./OrgSpend";

const spend = (overrides: Partial<OrgSpendData> = {}): OrgSpendData => ({
  cost_usd: 12.5,
  routed_cost_usd: 0.5,
  tokens: { input: 1_150_000, output: 240_000, cache_read: 3_200_000, cache_write: 60_000 },
  models: [
    { model: "claude-opus-5", tokens: 4_000_000, cost_usd: 8 },
    { model: "deepseek/deepseek-flash", tokens: 1_200_000, cost_usd: 0.3 },
    { model: "strix/ds4-flash", tokens: 500_000, cost_usd: null },
    { model: "claude-haiku-4-5", tokens: 120_000, cost_usd: 0.02 },
    { model: "qwen3-coder", tokens: 40_000, cost_usd: null },
  ],
  ...overrides,
});

const orgDay = (day: string, costUsd: number | null): OrgSpendHistoryDay => ({
  day,
  org: {
    org: "acme",
    cost_usd: costUsd,
    routed_cost_usd: costUsd == null ? null : 0,
    tokens: { input: 1, output: 1, cache_read: 0, cache_write: 0 },
    models: [],
    launched: 0,
    returned: 0,
  },
});

/** A day in the return window this org stayed out of — an empty sparkline slot. */
const gapDay = (day: string): OrgSpendHistoryDay => ({ day, org: undefined });

const rects = (html: string) => html.match(/<rect/g)?.length ?? 0;

describe("OrgSpend", () => {
  it("renders nothing when an older mothership has neither an org rollup nor history", () => {
    expect(renderToStaticMarkup(<OrgSpend spend={undefined} />)).toBe("");
  });

  it("renders —, never $0.00, for unmeasured spend", () => {
    const html = renderToStaticMarkup(<OrgSpend spend={spend({ cost_usd: null, routed_cost_usd: null })} />);
    expect(html).toContain("—");
    expect(html).not.toContain("$0.00");
  });

  it("shows compact token tallies", () => {
    const html = renderToStaticMarkup(<OrgSpend spend={spend()} />);
    // Total tokens 1.15M + 0.24M + 3.2M + 0.06M = 4.65M → "4.7M"; deepseek's slice is 1.2M.
    expect(html).toContain("4.7M tokens");
    expect(html).toContain(">1.2M<");
  });

  it("shows the top three models by tokens and counts the rest", () => {
    const html = renderToStaticMarkup(<OrgSpend spend={spend()} />);
    expect(html).toContain("claude-opus-5");
    expect(html).toContain("deepseek/deepseek-flash");
    expect(html).toContain("strix/ds4-flash");
    expect(html).toContain("and 2 more");
    expect(html).not.toContain("claude-haiku-4-5");
    expect(html).not.toContain("qwen3-coder");
  });

  it("draws one bar per day, leaving a zero-height slot where the org was away", () => {
    const days = [orgDay("2026-09-10", 2), gapDay("2026-09-11"), orgDay("2026-09-12", 4)];
    const html = renderToStaticMarkup(<OrgSpend spend={spend()} history={days} />);
    expect(rects(html)).toBe(3);
    expect(html).toContain('height="0"');
    expect(html).toContain('aria-label="Spend, last 30 days"');
    expect(html).toContain('role="img"');
    // The hover title pairs the day with its measured cost.
    expect(html).toContain("2026-09-10: $2.00");
  });

  it("renders — with no bars when every day is unmeasured, never an implied $0.00", () => {
    const days = [orgDay("2026-09-10", null), orgDay("2026-09-11", null)];
    const html = renderToStaticMarkup(
      <OrgSpend spend={spend({ cost_usd: null, routed_cost_usd: null })} history={days} />,
    );
    expect(rects(html)).toBe(0);
    expect(html).toContain("—");
    expect(html).not.toContain("$0.00");
  });
});