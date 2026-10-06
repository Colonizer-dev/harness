// Plan usage in the model switcher: a bar per plan in use, built only from what the mothership
// knows — an exhausted plan reads red with its countdown, a probed balance with a limit draws used
// against limit, a balance alone says the total isn't reported, and a plan reporting nothing says
// so and shows its request count instead. Rendered to static markup: no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { PlanUsage } from "../types";
import { PlanList, agoWords, planView, sortPlans } from "./ModelPlans";

const RESET = Date.UTC(2026, 9, 5, 19, 51, 58) / 1000;
const NOW = (RESET - (2 * 3600 + 10 * 60)) * 1000;

const plan = (over: Partial<PlanUsage> = {}): PlanUsage => ({
  id: "byteplus",
  name: "BytePlus",
  kind: "provider",
  used_by: ["subagents", "background"],
  exhausted: false,
  reset_at: null,
  reset_unix: null,
  last_limit: null,
  requests: null,
  failures: null,
  last_request_at: null,
  since: null,
  balance: null,
  ...over,
});

describe("planView", () => {
  it("reads an exhausted plan as red, fully used, with the local reset and countdown", () => {
    const v = planView(plan({ exhausted: true, reset_unix: RESET, reset_at: "10-05 19:51:58" }), NOW, "UTC");
    expect(v.tone).toBe("err");
    expect(v.usedPct).toBe(100);
    expect(v.figure).toBe("Out · 2 h 10 min");
    expect(v.usedBy).toBe("Used by subagents and background");
    expect(v.details[0]).toBe("Limit reached — resets at 19:51 · in 2 h 10 min");
  });

  it("draws used against limit when the probe reports both", () => {
    const v = planView(
      plan({ balance: { remaining: 2_140_000, limit: 5_000_000, pct_left: 42.8, error: null, checked_at: new Date(NOW - 40_000).toISOString() } }),
      NOW,
    );
    expect(v.tone).toBe("ok");
    expect(v.figure).toBe("42.8% left");
    expect(v.usedPct).toBeCloseTo(57.2);
    expect(v.details[0]).toBe("2.1M of 5M left · checked 40 s ago");
    expect(planView(plan({ balance: { remaining: 200, limit: 1000, pct_left: 20, error: null, checked_at: null } }), NOW).tone).toBe("warn");
    expect(planView(plan({ balance: { remaining: 50, limit: 1000, pct_left: 5, error: null, checked_at: null } }), NOW).tone).toBe("err");
  });

  it("shows a balance alone without a bar, and labels the missing total", () => {
    const v = planView(plan({ balance: { remaining: 12_000, limit: null, pct_left: null, error: null, checked_at: null } }), NOW);
    expect(v.figure).toBe("12K left");
    expect(v.usedPct).toBeNull();
    expect(v.details[0]).toBe("Plan balance; the plan's total isn't reported");
  });

  it("says what is known when a provider reports no quota: requests, and the last limit hit", () => {
    const v = planView(
      plan({
        requests: 1240,
        since: "2026-03-12T09:00:00Z",
        last_limit: { at: new Date(NOW - 3 * 3600_000).toISOString(), reset_at: "7am (UTC)", reset_unix: null },
      }),
      NOW,
      "UTC",
    );
    expect(v.tone).toBe("unknown");
    expect(v.usedPct).toBeNull();
    expect(v.figure).toBe("no limit reported");
    expect(v.details).toEqual(["This provider reports no remaining quota", "1,240 requests since 12 Mar", "Last limit hit 3 h ago"]);
  });

  it("explains Claude's limits and a failed balance check honestly", () => {
    const claude = planView(plan({ id: "anthropic", name: "Claude", kind: "claude", used_by: ["orchestrator"] }), NOW);
    expect(claude.details[0]).toBe("Claude reports its session and weekly limits only once one is hit");
    const failed = planView(plan({ balance: { remaining: null, limit: null, pct_left: null, error: "quota endpoint answered HTTP 404", checked_at: null } }), NOW);
    expect(failed.details[0]).toBe("Balance check failed: quota endpoint answered HTTP 404");
    expect(failed.usedPct).toBeNull();
  });

  it("puts exhausted plans first, otherwise keeps the mothership's order", () => {
    const sorted = sortPlans([plan({ id: "a" }), plan({ id: "b", exhausted: true }), plan({ id: "c" })]);
    expect(sorted.map((p) => p.id)).toEqual(["b", "a", "c"]);
    expect(agoWords(new Date(NOW - 5 * 60_000).toISOString(), NOW)).toBe("5 min ago");
  });
});

describe("PlanList", () => {
  it("renders a meter per plan, red for the exhausted one, and a count of plans out", () => {
    const html = renderToStaticMarkup(
      <PlanList
        nowMs={NOW}
        plans={[
          plan({ id: "anthropic", name: "Claude", kind: "claude", used_by: ["orchestrator"] }),
          plan({ exhausted: true, reset_unix: RESET }),
        ]}
      />,
    );
    expect(html).toContain('aria-label="plan usage"');
    expect(html).toContain("1 out");
    expect(html.match(/role="meter"/g)?.length).toBe(2);
    expect(html).toContain('data-plan="byteplus" data-tone="err"');
    expect(html).toContain('aria-valuenow="100"');
    expect(html).toContain('aria-valuetext="limit not reported"');
    expect(html.indexOf("byteplus")).toBeLessThan(html.indexOf("anthropic"));
  });

  it("says when it is loading, failed or empty", () => {
    expect(renderToStaticMarkup(<PlanList plans={null} />)).toContain("Reading plan usage…");
    expect(renderToStaticMarkup(<PlanList plans={[]} />)).toContain("No plan in use reports anything yet.");
    expect(renderToStaticMarkup(<PlanList plans={null} error="403" />)).toContain("Plan usage unavailable: 403");
  });
});
