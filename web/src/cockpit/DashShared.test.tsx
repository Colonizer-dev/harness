// Shared foundation (issue #398, v3 pass): pure colour/delta/geometry helpers and static markup
// for the shared primitives — renderToStaticMarkup runs no effects, exactly like the existing
// dashboard tests.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Session } from "../types";
import {
  AreaChart,
  ChartSection,
  ColonyRow,
  DashLegend,
  KpiStrip,
  KpiTile,
  niceStep,
  RangePicker,
  SegTabs,
  ShareBar,
  Sparkline,
  StatusChip,
  stackAreas,
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

describe("KpiTile / KpiStrip", () => {
  it("reads a measured tile: label, value, coloured delta and a glowing sparkline", () => {
    const bad = renderToStaticMarkup(<KpiTile label="Change failure rate" value="9.0%" delta="+1.2 pts" deltaTone="bad" spark="0,20 50,10 100,4" hint="h" />);
    expect(bad).toContain("Change failure rate");
    expect(bad).toContain("text-err");
    expect(bad).toContain("drop-shadow");
    expect(bad).toContain('viewBox="0 0 100 28"');
    const good = renderToStaticMarkup(<KpiTile label="Merged PRs" value="12" delta="+20%" deltaTone="good" hint="h" />);
    expect(good).toContain("text-ok");
  });
  it("shows — with the reason when a measured KPI has nothing to read", () => {
    const html = renderToStaticMarkup(<KpiTile label="Cost per merged PR" value="—" emptyNote="nothing merged yet" hint="h" />);
    expect(html).toContain("—");
    expect(html).toContain("nothing merged yet");
    expect(html).not.toContain("<path");
  });
  it("names unmeasured KPIs once in the footnote instead of drawing empty tiles", () => {
    const html = renderToStaticMarkup(
      <KpiStrip
        items={[
          { label: "Merged PRs", value: "3", hint: "h" },
          { label: "Lead time", value: "—", unmeasured: true, hint: "h" },
          { label: "CI pass rate", value: "—", unmeasured: true, hint: "h" },
        ]}
        note="API error rate 1.00%"
      />,
    );
    expect(html).toContain("Merged PRs");
    expect(html).toContain("Lead time and CI pass rate are not measured yet — no data source.");
    expect(html).toContain("API error rate 1.00%");
    expect(html.match(/text-\[28px\]/g)).toHaveLength(1);
  });
});

describe("DashLegend", () => {
  it("renders a square per series, a caller icon in its place, and a dashed ghost entry", () => {
    const html = renderToStaticMarkup(
      <DashLegend items={[{ label: "acme", color: "red" }, { label: "beta", color: "blue", icon: <i>AVATAR</i> }, { label: "prev", color: "transparent", dashed: true }]} />,
    );
    expect(html).toContain("acme");
    expect(html).toContain("AVATAR");
    expect(html).toContain("border-dashed");
    expect(html.match(/rounded-\[2px\]/g)).toHaveLength(1);
  });
});

describe("niceStep / stackAreas", () => {
  it("rounds a step up to 1, 2, 2.5 or 5 × 10ⁿ", () => {
    expect(niceStep(0.3)).toBe(0.5);
    expect(niceStep(3)).toBe(5);
    expect(niceStep(2.2)).toBe(2.5);
    expect(niceStep(12)).toBe(20);
    expect(niceStep(0)).toBe(1);
  });
  it("stacks each series on the one before and closes each band", () => {
    const geo = stackAreas(
      [
        { label: "a", color: "red", values: [1, 2] },
        { label: "b", color: "blue", values: [1, 2] },
      ],
      2,
      4,
    );
    expect(geo[0].tops.map((p) => p.y)).toEqual([75, 50]);
    expect(geo[1].tops.map((p) => p.y)).toEqual([50, 0]);
    expect(geo[1].area.endsWith("Z")).toBe(true);
    expect(geo[1].area).toContain(" L");
  });
});

describe("AreaChart", () => {
  const series = [
    { label: "acme", color: "var(--chart-1)", values: [1, 0, 3] },
    { label: "beta", color: "var(--chart-2)", values: [0, 2, 1] },
  ];
  it("draws gradient bands, lines, the now dot, axis ticks and the read-out totals", () => {
    const html = renderToStaticMarkup(<AreaChart series={series} labels={["Sep 1", "Sep 2", "Sep 3"]} format={(v) => String(v)} readTitle="Last 3 days" />);
    expect(html).toContain("linearGradient");
    expect(html).toContain("v3-reveal");
    expect(html).toContain("v3-now-dot");
    expect(html).toContain("Last 3 days");
    expect(html).toContain("acme <span");
    expect(html).toContain("Sep 1");
    expect(html).toContain("h-[200px]");
  });
  it("draws the dashed ghost only when given", () => {
    const withGhost = renderToStaticMarkup(<AreaChart series={series} labels={["a", "b", "c"]} ghost={[2, null, 1]} format={(v) => String(v)} readTitle="r" />);
    expect(withGhost).toContain("previous period daily total");
    expect(withGhost).toContain("stroke-dasharray");
    const without = renderToStaticMarkup(<AreaChart series={series} labels={["a", "b", "c"]} format={(v) => String(v)} readTitle="r" />);
    expect(without).not.toContain("previous period daily total");
  });
  it("makes columns keyboard-focusable with a spoken summary", () => {
    const html = renderToStaticMarkup(<AreaChart series={series} labels={["a", "b", "c"]} format={(v) => String(v)} readTitle="r" />);
    expect(html.match(/tabindex="0"/g)).toHaveLength(3);
    expect(html).toContain('aria-label="c: acme 3, beta 1"');
  });
  it("keeps an empty state", () => {
    expect(renderToStaticMarkup(<AreaChart series={[]} labels={[]} format={String} readTitle="r" />)).toContain("no data in range");
    expect(renderToStaticMarkup(<AreaChart series={[{ label: "a", color: "red", values: [0, 0] }]} labels={["a", "b"]} format={String} readTitle="r" emptyNote="nothing here" />)).toContain(
      "nothing here",
    );
  });
});

describe("ChartSection", () => {
  it("puts the side column beside the chart, with clickable rows when asked", () => {
    const html = renderToStaticMarkup(
      <ChartSection
        title="Merged PRs per day"
        chart={<div>CHART</div>}
        foot="3 merged"
        sideTitle="Share by workspace"
        side={[
          { label: "acme", value: 2, note: "67%", share: 67, color: "red", onClick: () => {} },
          { label: "beta", value: 1, note: "33%", share: 33, color: "blue" },
        ]}
        sideFoot="All workspaces shown"
      />,
    );
    expect(html).toContain("Merged PRs per day");
    expect(html).toContain("CHART");
    expect(html).toContain("Share by workspace");
    expect(html).toContain("width:67%");
    expect(html.match(/<button/g)).toHaveLength(1);
    expect(html).toContain("All workspaces shown");
  });
});

describe("ShareBar", () => {
  it("splits the strip by value with hover titles", () => {
    const html = renderToStaticMarkup(
      <ShareBar segments={[{ label: "a", color: "red", value: 3 }, { label: "b", color: "blue", value: 1 }]} format={(v) => `${v} tok`} label="mix" />,
    );
    expect(html).toContain("width:75%");
    expect(html).toContain("a: 3 tok");
  });
  it("reads empty instead of dividing by zero", () => {
    expect(renderToStaticMarkup(<ShareBar segments={[{ label: "a", color: "red", value: 0 }]} format={String} label="mix" />)).toContain("no data in range");
  });
});

describe("StatusChip / SegTabs / RangePicker", () => {
  it("renders tone text and pressed segments with warn counts", () => {
    expect(renderToStaticMarkup(<StatusChip tone="warn">2 need you</StatusChip>)).toContain("var(--warn)");
    const tabs = renderToStaticMarkup(
      <SegTabs
        label="f"
        items={[
          { key: "all", label: "all", count: 4, active: true, onClick: () => {} },
          { key: "need", label: "need you", count: 2, active: false, urgent: true, onClick: () => {} },
        ]}
      />,
    );
    expect(tabs).toMatch(/aria-pressed="true"[^>]*>all/);
    expect(tabs).toContain("text-warn");
  });
  it("renders the range segments and the compare switch", () => {
    const html = renderToStaticMarkup(<RangePicker range={30} onRange={() => {}} compare onCompare={() => {}} />);
    for (const r of ["7d", "30d", "90d"]) expect(html).toContain(r);
    expect(html).toContain('role="switch"');
    expect(html).toContain('aria-checked="true"');
    expect(html).toContain("Compare");
  });
});

describe("Sparkline", () => {
  it("draws one glowing smoothed line", () => {
    const html = renderToStaticMarkup(<Sparkline points="0,20 50,10 100,4" color="red" />);
    // Revealed by a clip: a dash-offset draw-in breaks a non-scaling stroke into segments.
    expect(html).toContain("spark-reveal");
    expect(html).not.toContain("stroke-dasharray");
    expect(html).toContain("drop-shadow(0 0 2px red)");
  });
  it("keeps its height but draws nothing for fewer than two points", () => {
    expect(renderToStaticMarkup(<Sparkline points="" color="red" />)).not.toContain("<svg");
  });
});

describe("ColonyRow", () => {
  const session = {
    id: "s1",
    repo: "acme/webshop",
    org: "acme",
    issue: 7,
    issue_title: "Fix checkout",
    status: "running",
    cost_usd: 0.5,
    routed_cost_usd: null,
  } as unknown as Session;
  it("pulses a working colony and flashes/highlights on request", () => {
    const html = renderToStaticMarkup(<ColonyRow session={session} age="2m" flashed bumped />);
    expect(html).toContain("v3-live-dot");
    expect(html).toContain("v3-flash");
    expect(html).toContain("text-accent");
    expect(html).toContain("webshop#7");
    expect(html).toContain("Working");
  });
  it("stays quiet when nothing moved", () => {
    const html = renderToStaticMarkup(<ColonyRow session={{ ...session, status: "merged" } as Session} age="1d" flashed={false} bumped={false} />);
    expect(html).not.toContain("v3-flash");
    expect(html).not.toContain("v3-live-dot");
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

