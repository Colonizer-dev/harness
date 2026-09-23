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
  Sparkline,
  StatusChip,
} from "./DashChart";
import { chartColor, chartRuns, chartY, deltaTone, joinedPoints, modelColorFor, monotonePath, orgColorFor, orgHue } from "./dash";

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
  it("paints the sparkline area under the smoothed line with a glowing stroke", () => {
    const html = renderToStaticMarkup(<KpiTile label="LAUNCHED" value="3" spark="0,28 50,10 100,20" hint="launched" />);
    expect(html).toContain("linearGradient");
    expect(html).toContain("<path");
    expect(html).toContain("dash-draw");
    // The historic 100×28 spark box is kept, so the org dashboard's viewBox assertion still holds.
    expect(html).toContain('viewBox="0 0 100 28"');
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
  it("renders a caller icon in place of the colour dot when one is given", () => {
    const html = renderToStaticMarkup(
      <DashLegend items={[{ label: "acme", color: "red", icon: <span>AVATAR</span> }, { label: "beta", color: "blue" }]} />,
    );
    expect(html).toContain("AVATAR");
    expect(html).toContain("acme");
    // Only beta keeps the dot.
    expect(html.match(/rounded-\[2px\]/g)).toHaveLength(1);
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
  it("rounds only each column's top segment and gradients every segment", () => {
    const html = renderToStaticMarkup(<DashBars series={series} labels={["a", "b", "c"]} format={(v) => String(v)} />);
    // 4 non-zero segments, 3 of them column tops.
    expect(html.match(/rounded-t-\[4px\]/g)).toHaveLength(3);
    expect(html.match(/linear-gradient/g)?.length).toBeGreaterThanOrEqual(4);
  });
  it("draws the dashed ghost line when compare is on, and skips it otherwise", () => {
    const ghost = [1, null, 2];
    const withGhost = renderToStaticMarkup(<DashBars series={series} labels={["a", "b", "c"]} ghost={ghost} format={(v) => String(v)} />);
    expect(withGhost).toContain("previous period daily total");
    expect(withGhost).toContain("stroke-dasharray");
    const without = renderToStaticMarkup(<DashBars series={series} labels={["a", "b", "c"]} format={(v) => String(v)} />);
    expect(without).not.toContain("previous period daily total");
  });
  it("joins the ghost across gaps so sparse previous periods still draw a line", () => {
    // Two measured days with a gap between them: one joined segment, not two
    // invisible single-point subpaths.
    const html = renderToStaticMarkup(<DashBars series={series} labels={["a", "b", "c", "d"]} ghost={[5, null, null, 3]} format={(v) => String(v)} />);
    expect(html).toContain("previous period daily total");
    expect(html).toMatch(/d="M[\d., ]+[CL]/);
  });
  it("dots a one-point ghost instead of vanishing it", () => {
    const html = renderToStaticMarkup(<DashBars series={series} labels={["a", "b", "c"]} ghost={[null, 4, null]} format={(v) => String(v)} />);
    expect(html).toContain("previous period daily total");
    expect(html).toContain("background:var(--faint)");
  });
  it("makes columns keyboard-focusable with the tooltip on focus as well as hover", () => {
    const html = renderToStaticMarkup(<DashBars series={series} labels={["a", "b", "c"]} format={(v) => String(v)} />);
    expect(html).toContain('tabindex="0"');
    expect(html).toContain("group-focus-within:opacity-100");
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
  it("draws smoothed gradient lines with pulsing last dots and hover tooltips", () => {
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
    expect(html).toContain("linearGradient");
    expect(html).toContain("dash-draw");
    // One pulsing last-point dot per solid series, as HTML so it stays circular.
    expect(html.match(/dash-pulse/g)?.length).toBeGreaterThanOrEqual(2);
    // Tooltip text names the day and every measured value — the accessible reading.
    expect(html).toContain("p50:");
    expect(html).toContain("p95:");
    expect(html).toContain("1.1s");
  });
  it("dots one-point runs so a lone measured day stays visible", () => {
    const html = renderToStaticMarkup(
      <DashLine series={[{ label: "s", color: "var(--chart-1)", values: [1, null, 2] }]} labels={["a", "b", "c"]} format={(v) => String(v)} />,
    );
    // The trailing lone point gets the pulsing end dot; the leading one a small static dot.
    expect(html).toContain("dash-pulse");
    expect(html).toContain("h-1.5 w-1.5 -translate-x-1/2");
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
  it("splits the strip by value with hover titles and a top-light gradient", () => {
    const html = renderToStaticMarkup(
      <ShareBar segments={[{ label: "a", color: "red", value: 1 }, { label: "b", color: "blue", value: 3 }]} format={(v) => String(v)} label="mix" />,
    );
    expect(html).toContain("25%");
    expect(html).toContain("75%");
    expect(html).toContain("a: 1");
    expect(html).toContain("linear-gradient");
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

describe("chartColor", () => {
  it("hands out ramp slots by series index and wraps after five", () => {
    expect(chartColor(0)).toBe("var(--chart-1)");
    expect(chartColor(4)).toBe("var(--chart-5)");
    expect(chartColor(5)).toBe("var(--chart-1)");
    expect(chartColor(7)).toBe("var(--chart-3)");
  });
});

describe("chartRuns / monotonePath", () => {
  it("maps values through the shared chartY helper", () => {
    expect(chartY(5, 10)).toBeCloseTo(51);
    expect(chartY(10, 10)).toBeCloseTo(2);
    expect(chartY(0, 10)).toBeCloseTo(100);
    // Negative values clamp to the frame instead of escaping it; no max collapses to the baseline.
    expect(chartY(-3, 10)).toBeLessThanOrEqual(100);
    expect(chartY(5, 0)).toBe(100);
  });
  it("joins gaps for the ghost instead of splitting them", () => {
    expect(joinedPoints([1, null, 2], 2)).toHaveLength(2);
    expect(joinedPoints([null, null], 2)).toHaveLength(0);
  });
  it("splits gaps into runs instead of zeroing through them", () => {
    const runs = chartRuns([1, null, 2, 3], 3);
    expect(runs).toHaveLength(2);
    expect(runs[0]).toHaveLength(1);
    expect(runs[1]).toHaveLength(2);
    // Lone points emit a bare moveto — the caller dots them.
    expect(monotonePath(runs[0])).toMatch(/^M[\d.]+,[\d.]+$/);
  });
  it("passes through every point: starts at the first, ends at the last", () => {
    const runs = chartRuns([1, 2, 5, 9], 9);
    expect(runs).toHaveLength(1);
    const d = monotonePath(runs[0]);
    expect(d.startsWith("M12.5,89.1")).toBe(true);
    expect(d.endsWith("87.5,2")).toBe(true);
  });
  it("never overshoots monotone data", () => {
    const runs = chartRuns([1, 2, 5, 9], 9);
    const nums = monotonePath(runs[0]).match(/-?\d+(\.\d+)?/g)?.map(Number) ?? [];
    const ys = nums.filter((_, i) => i % 2 === 1);
    expect(ys.length).toBeGreaterThan(2);
    // ViewBox y runs 89.1 (value 1) down to 2 (value 9): no control point may leave that band.
    for (const y of ys) {
      expect(y).toBeLessThanOrEqual(89.1);
      expect(y).toBeGreaterThanOrEqual(2);
    }
  });
  it("draws pairs straight and nothing for empty runs", () => {
    expect(monotonePath([])).toBe("");
    expect(monotonePath([{ x: 10, y: 20 }, { x: 30, y: 40 }])).toBe("M10,20L30,40");
  });
});

describe("Sparkline", () => {
  it("renders the gradient area, the glowing line and a pulsing last dot", () => {
    const html = renderToStaticMarkup(<Sparkline points="0,28 50,10 100,20" color="var(--accent)" />);
    expect(html).toContain("linearGradient");
    expect(html).toContain("dash-draw");
    expect(html).toContain("dash-pulse");
  });
  it("renders nothing for an empty spark", () => {
    expect(renderToStaticMarkup(<Sparkline points="" color="var(--accent)" />)).toBe("");
  });
});

describe("chart heights", () => {
  const series = [{ label: "a", color: "var(--chart-1)", values: [1, 2, 1] }];
  it("main charts stand 170px tall on narrow screens and 240px at desktop widths", () => {
    for (const html of [
      renderToStaticMarkup(<DashBars series={series} labels={["a", "b", "c"]} format={(v) => String(v)} />),
      renderToStaticMarkup(<DashLine series={[{ ...series[0], values: [1, 2, 1] }]} labels={["a", "b", "c"]} format={(v) => String(v)} />),
    ]) {
      expect(html).toContain("h-[170px]");
      expect(html).toContain("md:h-[240px]");
      expect(html).toContain('role="img"');
    }
  });
  it("KPI sparklines stay compact", () => {
    expect(renderToStaticMarkup(<Sparkline points="0,28 50,10 100,20" color="red" />)).toContain("h-7");
  });
});
