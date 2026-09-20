// The shared cost helpers (issue #209): "unmeasured" and "measured as zero" must never render the
// same way — null is "—", a real zero is "$0.00" — and every sum goes through one rule.
import { describe, expect, it } from "vitest";

import { formatCost, formatTokens, modelMix, orgCost, sessionCost, sumCosts } from "./spend";

describe("sumCosts", () => {
  it("is null only when every member is unmeasured", () => {
    expect(sumCosts([])).toBeNull();
    expect(sumCosts([null, null])).toBeNull();
  });

  it("adds the measured members, treating null as $0", () => {
    expect(sumCosts([null, 1])).toBe(1);
    expect(sumCosts([null, 1, 2])).toBe(3);
  });

  it("stays 0 for a measured zero", () => {
    expect(sumCosts([null, 0])).toBe(0);
  });
});

describe("sessionCost", () => {
  it("is null when nothing has ever been measured", () => {
    expect(sessionCost({ cost_usd: null, routed_cost_usd: null })).toBeNull();
    expect(sessionCost({})).toBeNull();
    expect(sessionCost({ cost_usd: undefined, routed_cost_usd: undefined })).toBeNull();
  });

  it("sums the two reports, treating an absent one as zero", () => {
    expect(sessionCost({ cost_usd: 1.2, routed_cost_usd: null })).toBe(1.2);
    expect(sessionCost({ cost_usd: null, routed_cost_usd: 0.3 })).toBe(0.3);
    expect(sessionCost({ cost_usd: 1.2, routed_cost_usd: 0.3 })).toBe(1.5);
  });
});

describe("orgCost", () => {
  const pair = { cost_usd: 1.2, routed_cost_usd: 0.3 };

  it("is null for an absent rollup or one that never measured", () => {
    expect(orgCost(undefined)).toBeNull();
    expect(orgCost(null)).toBeNull();
    expect(orgCost({ cost_usd: null, routed_cost_usd: null })).toBeNull();
  });

  it("sums the rollup's two fields under the same rule", () => {
    expect(orgCost(pair)).toBe(1.5);
    expect(orgCost({ cost_usd: null, routed_cost_usd: 0.3 })).toBe(0.3);
  });
});

describe("formatCost", () => {
  it("renders an unmeasured cost as an em dash, never $0.00", () => {
    expect(formatCost(null)).toBe("—");
  });

  it("a measured zero IS $0.00", () => {
    expect(formatCost(0)).toBe("$0.00");
  });

  it("always shows two decimals", () => {
    expect(formatCost(12.345)).toBe("$12.35");
    expect(formatCost(0.5)).toBe("$0.50");
  });
});

describe("formatTokens", () => {
  it("keeps small counts plain", () => {
    expect(formatTokens(0)).toBe("0");
    expect(formatTokens(999)).toBe("999");
  });

  it("compacts thousands, millions and billions to one decimal with a trimmed tail zero", () => {
    expect(formatTokens(1000)).toBe("1k");
    expect(formatTokens(1200)).toBe("1.2k");
    expect(formatTokens(12_300)).toBe("12.3k");
    expect(formatTokens(1_200_000)).toBe("1.2M");
    expect(formatTokens(1_234_567_890)).toBe("1.2B");
  });

  it("rolls a prefix that rounds up to 1000 into the next unit", () => {
    expect(formatTokens(999_950)).toBe("1M");
    expect(formatTokens(999_999)).toBe("1M");
    expect(formatTokens(999_999_999)).toBe("1B");
  });
});

describe("modelMix", () => {
  it("keeps the top N by tokens and counts what remains", () => {
    const models = [5, 4, 3, 2, 1].map((n) => ({ model: `m${n}`, tokens: n * 1000 }));
    const { shown, more } = modelMix(models, 3);
    expect(shown.map((s) => s.model)).toEqual(["m5", "m4", "m3"]);
    expect(more).toBe(2);
  });

  it("shows everything when there is less than the cap, and nothing when absent", () => {
    const few = [{ model: "a", tokens: 10 }, { model: "b", tokens: 5 }];
    expect(modelMix(few).shown.map((s) => s.model)).toEqual(["a", "b"]);
    expect(modelMix(few).more).toBe(0);
    expect(modelMix(undefined).shown).toEqual([]);
    expect(modelMix(null).more).toBe(0);
  });

  it("sorts defensively, even if the server already orders by tokens", () => {
    const { shown } = modelMix([{ model: "z", tokens: 10 }, { model: "a", tokens: 99 }], 1);
    expect(shown.map((s) => s.model)).toEqual(["a"]);
  });
});