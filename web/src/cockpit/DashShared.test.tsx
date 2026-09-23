// Shared foundation (issue #398): pure colour/delta helpers and static markup for the shared
// primitives — renderToStaticMarkup runs no effects, exactly like the existing dashboard tests.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import {
  DashBars,
  DashLegend,
  DashLine,
  DashPanel,
  Eyebrow,
  FilterChip,
  KpiTile,
  RangePicker,
  ShareBar,
  StatusChip,
} from "./DashChart";
import { deltaTone, modelColorFor, orgColorFor, orgHue } from "./dash";

describe("orgHue / orgColorFor", () => {
  it("is deterministic and fits the design's oklch(0.72 0.17 <hue>) form", () => {
    expect(orgColorFor("acme")).toBe(orgColorFor("acme"));
    expect(orgColorFor("acme")).toMatch(/^oklch\(0\.72 0\.17 \d+\)$/);
    expect(orgHue("acme")).toBeGreaterThanOrEqual(0);
    expect(orgHue("acme")).toBeLessThan(360);
    expect(Number.isInteger(orgHue("acme"))).toBe(true);
  });
  it("separates distinct orgs", () => {
    expect(orgHue("acme")).not.toBe(orgHue("beta"));
    expect(orgColorFor("acme")).not.toBe(orgColorFor("beta"));
  });
});

describe("modelColorFor", () => {
  it("pins the design's three model hues", () => {
    expect(modelColorFor("bailian/deepseek-v4-flash-0731")).toBe("var(--model-deepseek)");
    expect(modelColorFor("claude-opus-5-5")).toBe("var(--model-opus)");
    expect(modelColorFor("meta/muse-spark-1.3-contributor")).toBe("var(--model-spark)");
  });
  it("hashes unknown models deterministically", () => {
    expect(modelColorFor("future-model-9")).toBe(modelColorFor("future-model-9"));
    expect(modelColorFor("future-model-9")).toMatch(/^oklch\(/);
  });
});

describe("deltaTone", () => {
  it("reads flat with no figure or a negligible one", () => {
    expect(deltaTone(null)).toBe("flat");
    expect(deltaTone(0.001)).toBe("flat");
    expect(deltaTone(-0.001)).toBe("flat");
  });
  it("treats up as good by default and down as good when asked", () => {
    expect(deltaTone(0.2)).toBe("good");
    expect(deltaTone(-0.2)).toBe("bad");
    expect(deltaTone(0.2, "down")).toBe("bad");
    expect(deltaTone(-0.2, "down")).toBe("good");
  });
});

describe("KpiTile", () => {
  it("renders the honest empty state in the same card shape", () => {
    const html = renderToStaticMarkup(<KpiTile label="LEAD TIME" value="—" emptyNote="no data source yet" hint="lead time" />);
    expect(html).toContain("LEAD TIME");
    expect(html).toContain("—");
    expect(html).toContain("no data source yet");
    expect(html).not.toContain("<svg");
    expect(html).toContain("rounded-[14px]");
  });
  it("colours the delta by tone and prefixes the direction glyph", () => {
    const bad = renderToStaticMarkup(<KpiTile label="SPEND" value="$5" delta="6%" deltaTone="bad" deltaDir="up" hint="spend" />);
    expect(bad).toContain("text-err");
    expect(bad).toContain("▲");
    const good = renderToStaticMarkup(<KpiTile label="SPEND" value="$5" delta="6%" deltaTone="good" deltaDir="down" hint="spend" />);
    expect(good).toContain("text-ok");
    expect(good).toContain("▼");
  });
  it("paints the sparkline area under the line", () => {
    const html = renderToStaticMarkup(<KpiTile label="LAUNCHED" value="3" spark="0,28 50,10 100,20" hint="launched" />);
    expect(html).toContain("<polygon");
    expect(html).toContain("<polyline");
  });
});

describe("DashPanel / Eyebrow / DashLegend", () => {
  it("renders the title, subtitle and legend in one card", () => {
    const html = renderToStaticMarkup(
      <DashPanel title="SPEND PER DAY" sub="$10 over 7d" legend={<DashLegend items={[{ label: "acme", color: "red" }]} />}>
        <div>body</div>
      </DashPanel>,
    );
    expect(html).toContain("SPEND PER DAY");
    expect(html).toContain("$10 over 7d");
    expect(html).toContain("acme");
    expect(html).toContain("body");
    expect(html).toContain("rounded-2xl");
  });
  it("renders the mono eyebrow", () => {
    expect(renderToStaticMarkup(<Eyebrow>HELLO</Eyebrow>)).toContain("tracking-[0.12em]");
  });
});

describe("DashBars", () => {
  const series = [
    { label: "launched", color: "var(--info)", values: [2, 0, 3] },
    { label: "returned", color: "var(--ok)", values: [1, 1, 0] },
  ];
  it("rounds only each column's top segment", () => {
    const html = renderToStaticMarkup(<DashBars series={series} labels={["a", "b", "c"]} format={(v) => String(v)} />);
    // 4 non-zero segments, 3 of them column tops.
    expect(html.match(/rx="/g)).toHaveLength(4);
    expect(html).toContain('rx="0"');
  });
  it("draws the dashed ghost line when compare is on, and skips it otherwise", () => {
    const ghost = [1, null, 2];
    const withGhost = renderToStaticMarkup(<DashBars series={series} labels={["a", "b", "c"]} ghost={ghost} format={(v) => String(v)} />);
    expect(withGhost).toContain("previous period daily total");
    expect(withGhost).toContain("stroke-dasharray");
    const without = renderToStaticMarkup(<DashBars series={series} labels={["a", "b", "c"]} format={(v) => String(v)} />);
    expect(without).not.toContain("previous period daily total");
  });
  it("renders the y gutter and sparse x labels when asked", () => {
    const html = renderToStaticMarkup(
      <DashBars series={series} labels={["a", "b", "c"]} format={(v) => String(v)} formatY={(v) => `$${v}`} xLabels={["Aug 24", "Aug 25", "Aug 26"]} />,
    );
    expect(html).toContain("$3");
    expect(html).toContain("Aug 24");
    expect(html).toContain("Aug 26");
  });
  it("keeps the empty state", () => {
    expect(renderToStaticMarkup(<DashBars series={[]} labels={[]} format={(v) => String(v)} />)).toContain("no data in range");
  });
});

describe("DashLine", () => {
  it("draws filled lines with end dots and hover titles", () => {
    const html = renderToStaticMarkup(
      <DashLine
        series={[
          { label: "p50", color: "var(--lat-p50)", values: [1.2, 1.4, null, 1.1], fill: true },
          { label: "p95", color: "var(--lat-p95)", values: [3.1, null, 3.4, 3.0] },
        ]}
        labels={["a", "b", "c", "d"]}
        format={(v) => `${v.toFixed(1)}s`}
      />,
    );
    expect(html).toContain("<polygon");
    expect(html).toContain("<circle");
    expect(html).toContain("p50:");
    expect(html).toContain("p95:");
  });
  it("draws a dashed reference series and the same empty state", () => {
    const html = renderToStaticMarkup(
      <DashLine series={[{ label: "prev", color: "var(--faint)", values: [2, 2], dashed: true }]} labels={["a", "b"]} format={(v) => String(v)} />,
    );
    expect(html).toContain("stroke-dasharray");
    expect(renderToStaticMarkup(<DashLine series={[{ label: "p50", color: "red", values: [null, null] }]} labels={["a", "b"]} format={(v) => String(v)} />)).toContain(
      "no data in range",
    );
  });
});

describe("ShareBar", () => {
  it("splits the strip by value with hover titles", () => {
    const html = renderToStaticMarkup(
      <ShareBar segments={[{ label: "a", color: "red", value: 1 }, { label: "b", color: "blue", value: 3 }]} format={(v) => String(v)} label="mix" />,
    );
    expect(html).toContain("25%");
    expect(html).toContain("75%");
    expect(html).toContain("a: 1");
  });
  it("reads empty instead of dividing by zero", () => {
    expect(renderToStaticMarkup(<ShareBar segments={[]} format={(v) => String(v)} label="mix" />)).toContain("no data in range");
  });
});

describe("StatusChip / FilterChip / RangePicker", () => {
  it("renders chips with tone and pressed state", () => {
    expect(renderToStaticMarkup(<StatusChip tone="warn">1 need you</StatusChip>)).toContain("1 need you");
    const on = renderToStaticMarkup(<FilterChip active count={4} label="need you" onClick={() => {}} />);
    expect(on).toContain('aria-pressed="true"');
    expect(on).toContain("bg-accent-soft");
    const off = renderToStaticMarkup(<FilterChip active={false} count={4} label="need you" onClick={() => {}} />);
    expect(off).toContain('aria-pressed="false"');
  });
  it("renders the range switch and the compare toggle", () => {
    const html = renderToStaticMarkup(<RangePicker range={30} onRange={() => {}} compare onCompare={() => {}} />);
    expect(html).toContain('aria-label="Range"');
    for (const text of [">7d<", ">30d<", ">90d<", "Compare to previous 30d"]) expect(html).toContain(text);
  });
});
