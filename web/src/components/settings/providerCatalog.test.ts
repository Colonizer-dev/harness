// Per-model prices and the price feed (issue #1038): the rows the editor holds, the record they would
// save, and the feed price's wording — the pure helpers, pinned like the model map's.
import { describe, expect, it } from "vitest";

import {
  modelPricingChanged,
  modelPricingRowsOf,
  parseModelPricing,
  pricingRatesOf,
  pricingSummaryOf,
  sourceLinkOf,
  verifiedTextOf,
  type ModelPricingRow,
} from "./providerCatalog";

const NOW = Date.parse("2026-10-07T12:00:00Z");
const daysAgo = (n: number) => new Date(NOW - n * 86_400_000).toISOString();

const row = (model: string, rates: Partial<Record<string, string>> = {}): ModelPricingRow => ({
  model,
  pricing: { input_per_mtok: "", output_per_mtok: "", cache_read_per_mtok: "", cache_write_per_mtok: "", thinking_per_mtok: "", ...rates },
});

describe("modelPricingRowsOf", () => {
  it("sorts the saved prices by model id and drafts every rate; none saved is no rows", () => {
    expect(modelPricingRowsOf(undefined)).toEqual([]);
    expect(modelPricingRowsOf({ "b-model": { input_per_mtok: 1 }, "a-model": { output_per_mtok: 2 } }).map((r) => r.model)).toEqual([
      "a-model",
      "b-model",
    ]);
    expect(modelPricingRowsOf({ "a-model": { input_per_mtok: 1.5 } })[0].pricing.input_per_mtok).toBe("1.5");
  });
});

describe("parseModelPricing", () => {
  it("saves the rates that are set, dropping blank ids and rate-less rows like the model map's blanks", () => {
    expect(parseModelPricing([row(" flash ", { input_per_mtok: " 0.5 ", output_per_mtok: "2" }), row("  "), row("vintage")]).value).toEqual({
      flash: { input_per_mtok: 0.5, output_per_mtok: 2 },
    });
    expect(parseModelPricing([row("flash")]).value).toEqual({});
  });

  it("refuses a rate that is not a dollar amount, 0 or more, naming the model when there is one", () => {
    expect(parseModelPricing([row("flash", { input_per_mtok: "-1" })]).error).toBe("flash: A dollar amount, 0 or more");
    expect(parseModelPricing([row("", { input_per_mtok: "-1" })]).error).toBe("A dollar amount, 0 or more");
    expect(parseModelPricing([row("flash", { cache_write_per_mtok: "later" })]).error).toBe("flash: A dollar amount, 0 or more");
    expect(parseModelPricing([row("flash", { input_per_mtok: "0.5" })]).error).toBeNull();
  });

  it("refuses a model id named twice, which a saved map would collapse last-wins", () => {
    expect(parseModelPricing([row("flash", { input_per_mtok: "1" }), row(" flash ", { output_per_mtok: "2" })]).error).toBe(
      "Duplicate model id: flash",
    );
    // Two rate-less rows for one model both drop, so they are not a clash.
    expect(parseModelPricing([row("flash"), row("flash")]).error).toBeNull();
  });
});

describe("modelPricingChanged", () => {
  it("is false when the rows amount to the saved prices, in any order", () => {
    expect(modelPricingChanged({}, undefined)).toBe(false);
    expect(modelPricingChanged({ a: { input_per_mtok: 1 } }, { a: { input_per_mtok: 1 } })).toBe(false);
    expect(
      modelPricingChanged({ b: { input_per_mtok: 2 }, a: { input_per_mtok: 1 } }, { a: { input_per_mtok: 1 }, b: { input_per_mtok: 2 } }),
    ).toBe(false);
  });

  it("is true when a rate, a model or the whole map changed", () => {
    expect(modelPricingChanged({ a: { input_per_mtok: 2 } }, { a: { input_per_mtok: 1 } })).toBe(true);
    expect(modelPricingChanged({}, { a: { input_per_mtok: 1 } })).toBe(true);
    expect(modelPricingChanged({ a: { input_per_mtok: 1 } }, undefined)).toBe(true);
  });
});

describe("verifiedTextOf", () => {
  it("counts the days back from the verification, with words for today and an unknown date", () => {
    expect(verifiedTextOf(null, NOW)).toBe("verified date unknown");
    expect(verifiedTextOf("not a date", NOW)).toBe("verified date unknown");
    expect(verifiedTextOf(daysAgo(0.2), NOW)).toBe("verified today");
    expect(verifiedTextOf(daysAgo(1), NOW)).toBe("verified 1 day ago");
    expect(verifiedTextOf(daysAgo(20), NOW)).toBe("verified 20 days ago");
  });
});

describe("sourceLinkOf", () => {
  it("accepts only http(s) URLs, trimmed, and nothing else", () => {
    expect(sourceLinkOf("https://example.com/prices")).toBe("https://example.com/prices");
    expect(sourceLinkOf("  http://example.com/prices ")).toBe("http://example.com/prices");
    expect(sourceLinkOf("javascript:alert(1)")).toBeNull();
    expect(sourceLinkOf("api-docs.deepseek.com/pricing")).toBeNull();
    expect(sourceLinkOf(null)).toBeNull();
  });
});

describe("pricingRatesOf", () => {
  it("feeds pricingSummaryOf from a saved price, unset rates left out", () => {
    expect(pricingSummaryOf(pricingRatesOf({ input_per_mtok: 0.27, output_per_mtok: 1.1 }))).toEqual(["input $0.27", "output $1.1"]);
    expect(pricingSummaryOf(pricingRatesOf(null))).toEqual([]);
  });
});
