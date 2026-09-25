import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import { DEMO_MAP } from "../mock";
import type { RepoMap } from "../types";
import { SURFACE_Y } from "./nest";
import { MIN_PLOT_H, NestMapView, colonyPlaces, mapRepos, plotHeightFor } from "./NestMapView";
import { DENSE_MAP } from "./mapFixtures";
import {
  LABEL_MAX_W,
  antRoute,
  boundaryBox,
  clampView,
  componentForPath,
  componentsForFiles,
  entryComponent,
  fitView,
  labelCollisions,
  layoutMap,
  normalizeMap,
  routeBetween,
  titleCollisions,
  tunnelPaths,
  zoomAt,
} from "./nestMap";
import { session } from "./testFixtures";

const box = { width: 1000, height: 640 };

describe("componentForPath", () => {
  const comps = DEMO_MAP.components;

  it("puts a file in the component that names it, or whose source shares its directory", () => {
    expect(componentForPath("services/checkout/src/checkout.ts", comps)).toBe("checkout");
    expect(componentForPath("services/checkout/src/guest.ts", comps)).toBe("checkout");
    expect(componentForPath("./services/email/templates/order-dark.mjml", comps)).toBe("email");
  });

  it("prefers the deepest directory, and claims nothing outside every component", () => {
    const nested = [
      { id: "outer", type: "backend", label: "Outer", pos: [0, 0] as [number, number], size: [10, 10] as [number, number], sources: [{ path: "src/index.ts" }] },
      { id: "inner", type: "backend", label: "Inner", pos: [0, 0] as [number, number], size: [10, 10] as [number, number], sources: [{ path: "src/inner/mod.ts" }] },
      { id: "dir", type: "backend", label: "Dir", pos: [0, 0] as [number, number], size: [10, 10] as [number, number], sources: [{ path: "lib/" }] },
    ];
    expect(componentForPath("src/inner/deep/x.ts", nested)).toBe("inner");
    expect(componentForPath("src/other/x.ts", nested)).toBe("outer");
    expect(componentForPath("src/inner/x.ts", nested)).toBe("inner");
    expect(componentForPath("lib/a/b.ts", nested)).toBe("dir");
    expect(componentForPath("README.md", nested)).toBeNull();
  });

  it("groups a colony's files by chamber, busiest first", () => {
    const hits = componentsForFiles(["services/checkout/src/a.ts", "services/checkout/src/b.ts", "services/email/templates/x.mjml", "nowhere.txt"], comps);
    expect(hits.map((h) => [h.id, h.files.length])).toEqual([
      ["checkout", 2],
      ["email", 1],
    ]);
  });
});

describe("layoutMap", () => {
  const layout = layoutMap(DEMO_MAP, box);

  it("keeps every chamber under the surface and inside the plot, in archify's arrangement", () => {
    for (const c of layout.chambers) {
      expect(c.y - c.r).toBeGreaterThan(SURFACE_Y);
      expect(c.x - c.r).toBeGreaterThanOrEqual(0);
      expect(c.x + c.r).toBeLessThanOrEqual(layout.width);
      expect(c.y + c.r).toBeLessThanOrEqual(layout.height);
    }
    const at = (id: string) => layout.byId.get(id)!;
    expect(at("web").x).toBeLessThan(at("api").x);
    expect(at("web").y).toBeLessThan(at("checkout").y);
  });

  it("draws one tunnel per connected pair, and a mound around a boundary's chambers", () => {
    expect(tunnelPaths(DEMO_MAP, layout)).toHaveLength(DEMO_MAP.connections.length);
    const mound = boundaryBox(layout, ["api", "checkout"])!;
    const api = layout.byId.get("api")!;
    expect(mound.x).toBeLessThan(api.x - api.r);
    expect(mound.x + mound.w).toBeGreaterThan(api.x + api.r);
    expect(boundaryBox(layout, ["ghost"])).toBeNull();
  });

  it("walks ants from the entry along the connections", () => {
    expect(entryComponent(DEMO_MAP, layout)).toBe("web");
    expect(routeBetween(DEMO_MAP, "web", "webhooks")).toEqual(["web", "api", "checkout", "webhooks"]);
    expect(routeBetween(DEMO_MAP, "stripe", "web")).toEqual(["stripe", "webhooks", "checkout", "api", "web"]);
    const route = antRoute(DEMO_MAP, layout, "webhooks");
    const w = layout.byId.get("webhooks")!;
    expect(route.startsWith(`M${layout.mouth.x} ${layout.mouth.y}`)).toBe(true);
    expect(route.endsWith(`${w.x} ${w.y}`)).toBe(true);
  });
});

describe("layoutMap on a dense map", () => {
  // The screenshot's shape: twenty components, four boundaries (one nested), one squeezed row.
  const sizes = [
    { width: 360, height: 480 },
    { width: 1024, height: 560 },
    { width: 1700, height: 790 },
    { width: 1700, height: 420 },
  ];

  it.each(sizes)("keeps every chamber and name apart at $width×$height", (view) => {
    const layout = layoutMap(DENSE_MAP, view);
    expect(labelCollisions(layout)).toEqual([]);
    expect(layout.chambers.filter((c) => c.compact)).toEqual([]);
    for (const c of layout.chambers) expect(c.label.w).toBeLessThanOrEqual(LABEL_MAX_W);
  });

  it.each(sizes)("gives every mound a title clear of other titles, chambers and names at $width×$height", (view) => {
    const layout = layoutMap(DENSE_MAP, view);
    expect(layout.mounds.map((m) => m.label)).toEqual(DENSE_MAP.boundaries.map((b) => b.label));
    expect(titleCollisions(layout)).toEqual([]);
    for (const m of layout.mounds) {
      expect(m.box.y).toBeGreaterThan(SURFACE_Y);
      expect(m.title!.x).toBeGreaterThanOrEqual(m.box.x);
      expect(m.title!.x + m.title!.w).toBeLessThanOrEqual(m.box.x + m.box.w);
    }
  });

  it.each(sizes)("keeps everything on the plot, which is at least the viewport, at $width×$height", (view) => {
    const layout = layoutMap(DENSE_MAP, view);
    expect(layout.width).toBeGreaterThanOrEqual(view.width);
    expect(layout.height).toBeGreaterThanOrEqual(view.height);
    for (const r of [...layout.chambers.map((c) => c.label), ...layout.mounds.map((m) => m.box)]) {
      expect(r.x).toBeGreaterThanOrEqual(0);
      expect(r.x + r.w).toBeLessThanOrEqual(layout.width);
      expect(r.y + r.h).toBeLessThanOrEqual(layout.height);
    }
  });

  it("uses a wide screen's width instead of a narrow cluster", () => {
    const layout = layoutMap(DENSE_MAP, { width: 1700, height: 790 });
    const xs = layout.chambers.map((c) => c.x);
    expect(Math.max(...xs) - Math.min(...xs)).toBeGreaterThan(1700 * 0.6);
    expect(fitView(layout, { width: 1700, height: 790 }).k).toBeGreaterThan(0.8);
  });

  it("spreads a map a phone cannot fit onto a larger plot, zoomed out to fit", () => {
    const view = { width: 360, height: 480 };
    const layout = layoutMap(DENSE_MAP, view);
    const fit = fitView(layout, view);
    expect(layout.width).toBeGreaterThan(view.width);
    expect(layout.width * fit.k).toBeLessThanOrEqual(view.width + 0.5);
    expect(layout.height * fit.k).toBeLessThanOrEqual(view.height + 0.5);
  });

  it("zooms about a point and never lets the plot leave the viewport", () => {
    const view = { width: 360, height: 480 };
    const layout = layoutMap(DENSE_MAP, view);
    const fit = fitView(layout, view);
    const inside = zoomAt(fit, 3, 100, 200, layout, view);
    expect(inside.k).toBeCloseTo(fit.k * 3);
    // The plot point under (100, 200) stays under it.
    expect((100 - inside.x) / inside.k).toBeCloseTo((100 - fit.x) / fit.k);
    const flung = clampView({ ...inside, x: 5000, y: -99999 }, layout, view);
    expect(flung.x).toBe(0);
    expect(flung.y).toBe(view.height - layout.height * flung.k);
    expect(clampView({ ...fit, k: 50 }, layout, view).k).toBe(2.5);
  });
});

describe("filling the viewport", () => {
  it.each([
    { width: 1700, height: 900 },
    { width: 2400, height: 1300 },
    { width: 1280, height: 1100 },
  ])("stretches a map with room to spare to the edges of $width×$height, at full size", (view) => {
    for (const m of [DEMO_MAP, DENSE_MAP]) {
      const layout = layoutMap(m, view);
      const fit = fitView(layout, view);
      const xs = layout.chambers.flatMap((c) => [c.x - c.r, c.label.x, c.label.x + c.label.w, c.x + c.r]);
      const ys = layout.chambers.flatMap((c) => [c.y - c.r, c.label.y + c.label.h]);
      // The map meets the viewport's edges (bar a margin) on both axes, at the zoom it is shown at.
      expect((Math.max(...xs) - Math.min(...xs)) * fit.k).toBeGreaterThan(view.width * 0.8);
      expect((Math.max(...ys) - Math.min(...ys)) * fit.k).toBeGreaterThan((view.height - SURFACE_Y) * 0.6);
      expect(labelCollisions(layout)).toEqual([]);
      if (m === DEMO_MAP) expect(fit.k).toBe(1);
    }
  });

  it("takes the rest of the pane below the plot's top, with a floor", () => {
    expect(plotHeightFor(300, 1100)).toBe(800);
    expect(plotHeightFor(900, 1100)).toBe(MIN_PLOT_H);
    expect(plotHeightFor(Number.NaN, 1100)).toBeNull();
    expect(plotHeightFor(100, 0)).toBeNull();
  });
});

describe("maps that could break the view", () => {
  it("lays out for a sensible size when the plot measures 0×0", () => {
    const layout = layoutMap(DENSE_MAP, { width: 0, height: 0 });
    expect(layout.width).toBeGreaterThan(0);
    for (const c of layout.chambers) expect(Number.isFinite(c.x) && Number.isFinite(c.y)).toBe(true);
    const fit = fitView(layout, { width: 0, height: 0 });
    expect(Number.isFinite(fit.k) && fit.k > 0).toBe(true);
    const z = zoomAt(fit, 2, 0, 0, layout, { width: 0, height: 0 });
    expect(Number.isFinite(z.k) && Number.isFinite(z.x) && Number.isFinite(z.y)).toBe(true);
  });

  it("draws an empty map and a half-written one without throwing", () => {
    expect(layoutMap({ title: "x", components: [], connections: [], boundaries: [] }, box).chambers).toEqual([]);
    const junk = {
      title: "x",
      components: [{ id: "a", label: "A" }, { id: "b", pos: [Number.NaN, 3], size: null }, null],
      boundaries: [{ label: "g" }],
    } as unknown as Parameters<typeof normalizeMap>[0];
    const map = normalizeMap(junk);
    expect(map.components.map((c) => c.id)).toEqual(["a", "b"]);
    expect(map.components[1].label).toBe("b");
    const layout = layoutMap(map, box);
    expect(layout.chambers).toHaveLength(2);
    for (const c of layout.chambers) expect(Number.isFinite(c.x) && Number.isFinite(c.y)).toBe(true);
    expect(tunnelPaths(map, layout)).toEqual([]);
    expect(componentForPath("src/x.ts", map.components)).toBeNull();
  });
});

describe("colony placement", () => {
  const live = session({ id: "a", repo: "acme/webshop", status: "running" });
  const idle = session({ id: "b", repo: "acme/webshop", status: "running" });
  const done = session({ id: "c", repo: "acme/webshop", status: "merged" });

  it("puts live colonies in the chambers their files are in, and the rest by the mouth", () => {
    const places = colonyPlaces([live, idle, done], { a: ["services/checkout/src/x.ts"], c: ["services/email/templates/y.mjml"] }, DEMO_MAP);
    expect([...places.byChamber.keys()]).toEqual(["checkout"]);
    expect(places.waiting.map((s) => s.id)).toEqual(["b"]);
  });

  it("places a colony that has only read files in the chambers it reads, as a reader", () => {
    const places = colonyPlaces([live, idle], { a: ["services/checkout/src/x.ts"] }, DEMO_MAP, {
      a: ["services/email/templates/y.mjml"],
      b: ["services/email/templates/y.mjml"],
    });
    expect(places.byChamber.get("checkout")?.map((p) => [p.session.id, p.mode])).toEqual([["a", "changing"]]);
    expect(places.byChamber.get("email")?.map((p) => [p.session.id, p.mode])).toEqual([["b", "reading"]]);
    expect(places.waiting).toEqual([]);
  });

  it("marks a colony waiting on a question or idle as blocked", () => {
    const asking = session({ id: "q", repo: "acme/webshop", status: "waiting_for_answer" });
    const places = colonyPlaces([asking], {}, DEMO_MAP, { q: ["services/checkout/src/x.ts"] });
    expect(places.byChamber.get("checkout")?.[0]).toMatchObject({ mode: "reading", blocked: true });
  });

  it("offers the repositories the nest's colonies work in, busiest first", () => {
    expect(mapRepos([done, session({ id: "d", repo: "acme/api", status: "running" }), live])).toEqual(["acme/webshop", "acme/api"]);
    expect(mapRepos([done, session({ id: "d", repo: "acme/api", status: "running" })])).toEqual(["acme/api", "acme/webshop"]);
  });
});

describe("NestMapView", () => {
  const colonies = [session({ id: "a", repo: "acme/webshop", status: "running", issue_title: "Guest checkout" })];
  const render = (initialMap: RepoMap | null, touched: Record<string, string[]> = {}) =>
    renderToStaticMarkup(
      <ApiContext.Provider value={{} as Api}>
        <NestMapView sessions={colonies} selectedId={null} onSelect={() => {}} onOpen={() => {}} initialMap={initialMap} initialTouched={touched} />
      </ApiContext.Provider>,
    );

  it("renders with no repository and no map", () => {
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={{} as Api}>
        <NestMapView sessions={[]} selectedId={null} onSelect={() => {}} onOpen={() => {}} />
      </ApiContext.Provider>,
    );
    expect(html).toContain("No colonies in this workspace yet");
  });

  it("renders a stored map that is missing its lists", () => {
    const broken = { title: "acme/webshop" } as unknown as typeof DEMO_MAP;
    const html = render({ repo: "acme/webshop", map: { repo: "acme/webshop", revision: null, generated_at: "2026-09-24T00:00:00Z", session: "m0", map: broken }, mapping: null });
    expect(html).toContain("map-plot");
  });

  it("offers to draw a repository that has no map", () => {
    const html = render({ repo: "acme/webshop", map: null, mapping: null });
    expect(html).toContain("No map for acme/webshop yet");
    expect(html).toContain("Map this repo");
  });

  it("says a map is being drawn while its colony runs", () => {
    const html = render({ repo: "acme/webshop", map: null, mapping: { id: "m1", status: "running", created_at: "2026-09-24T00:00:00Z" } });
    expect(html).toContain("Drawing acme/webshop");
    expect(html).toContain("Watch it work");
  });

  it("keeps saying it is drawing while the colony publishes, and says so when one ended without a map", () => {
    const publishing = render({ repo: "acme/webshop", map: null, mapping: { id: "m1", status: "publishing", created_at: "2026-09-24T00:00:00Z" } });
    expect(publishing).toContain("Drawing acme/webshop");
    const failed = render({ repo: "acme/webshop", map: null, mapping: { id: "m1", status: "failed", created_at: "2026-09-24T00:00:00Z" } });
    expect(failed).toContain("ended (failed) without a map");
    expect(failed).toContain("Map this repo");
  });

  it("draws chambers, mounds and tunnels, with the colony's ant in the chamber it is changing", () => {
    const html = render(
      { repo: "acme/webshop", map: { repo: "acme/webshop", revision: "4f2c9e1abc", generated_at: "2026-09-24T00:00:00Z", session: "m0", map: DEMO_MAP }, mapping: null },
      { a: ["services/checkout/src/guest.ts"] },
    );
    for (const c of DEMO_MAP.components) expect(html).toContain(c.label);
    expect(html).toContain("map-mound");
    expect(html).toContain('aria-label="Checkout · 1 colony inside"');
    expect(html).toContain('data-active="true"');
    expect(html).toContain("colony Guest checkout changing files here in checkout");
    expect(html).toContain("at 4f2c9e1");
  });
});
