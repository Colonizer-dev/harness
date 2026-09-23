import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import { DEMO_MAP } from "../mock";
import type { RepoMap } from "../types";
import { SURFACE_Y, normalizeBox } from "./nest";
import { NestMapView, colonyPlaces, mapRepos } from "./NestMapView";
import { antRoute, boundaryBox, componentForPath, componentsForFiles, entryComponent, layoutMap, routeBetween, tunnelPaths } from "./nestMap";
import { session } from "./testFixtures";

const box = normalizeBox(1000, 640);

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
      expect(c.x + c.r).toBeLessThanOrEqual(box.width);
      expect(c.y + c.r).toBeLessThanOrEqual(box.height);
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

describe("colony placement", () => {
  const live = session({ id: "a", repo: "acme/webshop", status: "running" });
  const idle = session({ id: "b", repo: "acme/webshop", status: "running" });
  const done = session({ id: "c", repo: "acme/webshop", status: "merged" });

  it("puts live colonies in the chambers their files are in, and the rest by the mouth", () => {
    const places = colonyPlaces([live, idle, done], { a: ["services/checkout/src/x.ts"], c: ["services/email/templates/y.mjml"] }, DEMO_MAP);
    expect([...places.byChamber.keys()]).toEqual(["checkout"]);
    expect(places.waiting.map((s) => s.id)).toEqual(["b"]);
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
    expect(html).toContain("colony Guest checkout in checkout");
    expect(html).toContain("at 4f2c9e1");
  });
});
