// The ranked-host list (issue #1039): one row per connected provider — position, name, the rates,
// when the price was verified, and badges for a degraded, out-of-quota, limited or host-sensitive
// host — with "in use" on the host the current value names. Pure presentational, rendered to static
// markup (the test environment has no DOM).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { blendedPerMtok } from "../features/models/mock";
import type { ModelHost, ModelHostRanking } from "../features/models/types";
import { HostRankings } from "./HostRankings";

const agoDays = (days: number) => new Date(Date.now() - days * 86_400_000).toISOString();

const host = (over: Partial<ModelHost> & Pick<ModelHost, "provider_id" | "provider_name" | "model">): ModelHost => ({
  input_per_mtok: null,
  output_per_mtok: null,
  cache_read_per_mtok: null,
  blended_per_mtok: null,
  last_verified_at: null,
  source: null,
  healthy: true,
  degraded: false,
  out_of_quota: false,
  quota_reset_at: null,
  tools_limited: false,
  host_sensitive: false,
  ...over,
});

/**
 * The mock install's fixtures: a three-host open-weight model, and a host-sensitive one. The
 * blended rates come from `blendedPerMtok` — the mothership's own blend — so the numbers here are
 * the ones the ranking would be sorted by, not a hand-copied set that can drift from the formula.
 */
const RANKINGS: ModelHostRanking[] = [
  {
    model: "deepseek-v4-flash",
    eligible: true,
    hosts: [
      host({
        provider_id: "deepinfra",
        provider_name: "DeepInfra",
        model: "deepinfra/deepseek-v4-flash",
        input_per_mtok: 0.2,
        output_per_mtok: 0.8,
        cache_read_per_mtok: 0.05,
        blended_per_mtok: blendedPerMtok(0.2, 0.8, 0.05),
        last_verified_at: agoDays(3),
        source: "https://deepinfra.com/prices",
      }),
      host({
        provider_id: "deepseek",
        provider_name: "DeepSeek",
        model: "deepseek/deepseek-v4-flash",
        input_per_mtok: 0.27,
        output_per_mtok: 1.1,
        cache_read_per_mtok: 0.07,
        blended_per_mtok: blendedPerMtok(0.27, 1.1, 0.07),
      }),
      host({ provider_id: "novita", provider_name: "Novita", model: "novita/deepseek-v4-flash" }),
    ],
  },
  {
    model: "glm-5.3",
    eligible: false,
    hosts: [
      host({
        provider_id: "zai",
        provider_name: "Z.AI",
        model: "zai/glm-5.3",
        input_per_mtok: 0.48,
        output_per_mtok: 1.7,
        cache_read_per_mtok: 0.14,
        blended_per_mtok: blendedPerMtok(0.48, 1.7, 0.14),
        healthy: false,
        degraded: true,
        host_sensitive: true,
      }),
      host({
        provider_id: "deepinfra",
        provider_name: "DeepInfra",
        model: "deepinfra/glm-5.3",
        input_per_mtok: 0.55,
        output_per_mtok: 1.9,
        cache_read_per_mtok: 0.16,
        blended_per_mtok: blendedPerMtok(0.55, 1.9, 0.16),
        last_verified_at: agoDays(3),
      }),
    ],
  },
];

/**
 * The name the ranking is announced under, or null when there is none. It comes from a `section`
 * (a region, once it has an accessible name) labelled by the heading it renders — an `aria-label`
 * on a bare `div` is ignored, so the element and the labelling are both part of what is asserted:
 * a `div` wrapper, or one whose `aria-labelledby` names nothing, returns null.
 */
function announcedName(html: string): string | null {
  const section = /<section[^>]*aria-labelledby="([^"]+)"/.exec(html);
  if (!section) return null;
  return new RegExp(`id="${section[1]}"[^>]*>([^<]*)<`).exec(html)?.[1] ?? null;
}

describe("HostRankings", () => {
  it("shows the current value's ranking: hosts in price order, with rates, verification and the in-use host", () => {
    const html = renderToStaticMarkup(<HostRankings rankings={RANKINGS} currentModel="deepseek/deepseek-v4-flash" />);
    expect(html).toContain("Hosts for deepseek-v4-flash");
    expect(html.startsWith("<section"), "the list is a named region, not an unnamed div").toBe(true);
    expect(announcedName(html)).toBe("Hosts for deepseek-v4-flash");
    for (const name of ["DeepInfra", "DeepSeek", "Novita"]) expect(html).toContain(name);
    // Cheapest first, unpriced last: the order the list is given is the order it renders in.
    expect(html.indexOf("DeepInfra")).toBeLessThan(html.indexOf("DeepSeek"));
    expect(html.indexOf("DeepSeek")).toBeLessThan(html.indexOf("Novita"));
    expect(html).toContain("in $0.2");
    expect(html).toContain("out $0.8");
    expect(html).toContain("verified 3 days ago");
    expect(html).toContain("verified date unknown"); // a local price, no feed date
    expect(html).toContain("price unknown"); // the unpriced host
    expect(html).toContain("in use"); // deepseek/deepseek-v4-flash, the current value
  });

  it("renders nothing for a value with no slash or no ranking", () => {
    expect(renderToStaticMarkup(<HostRankings rankings={RANKINGS} currentModel="opus" />)).toBe("");
    expect(renderToStaticMarkup(<HostRankings rankings={RANKINGS} currentModel="lab/qwen3-coder" />)).toBe("");
    expect(renderToStaticMarkup(<HostRankings rankings={RANKINGS} />)).toBe("");
    expect(renderToStaticMarkup(<HostRankings rankings={[]} currentModel="deepseek/deepseek-v4-flash" />)).toBe("");
  });

  it("badges a degraded, out-of-quota, tools-limited or host-sensitive host", () => {
    const flagged = host({
      provider_id: "zai",
      provider_name: "Z.AI",
      model: "zai/glm-5.3",
      degraded: true,
      out_of_quota: true,
      quota_reset_at: agoDays(-1),
      tools_limited: true,
      host_sensitive: true,
    });
    const html = renderToStaticMarkup(
      <HostRankings rankings={[{ model: "glm-5.3", eligible: false, hosts: [flagged] }]} currentModel="zai/glm-5.3" />,
    );
    expect(html).toContain("degraded");
    expect(html).toContain("out of quota");
    expect(html).toContain("tools limited");
    expect(html).toContain("host-sensitive");
    expect(html).toContain("in use");
  });

  it("takes a custom heading, still as the list's accessible name", () => {
    const html = renderToStaticMarkup(
      <HostRankings rankings={RANKINGS} currentModel="deepinfra/glm-5.3" heading="Cheapest host wins" />,
    );
    expect(html).toContain("Cheapest host wins");
    expect(announcedName(html)).toBe("Cheapest host wins");
  });
});
