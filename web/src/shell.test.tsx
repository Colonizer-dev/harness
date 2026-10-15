// Every cockpit route renders inside the same shell (issue #1203). The app used to keep a
// sidebar-drawer layout for one window band — 640–899px, a half-width tiled pane — and never
// mounted the cockpit there at all, so /secrets, /loops and every /settings/… page fell back to the
// legacy colonies/memory panes with no rail and no Loops or Secrets to click. These walk the whole
// route table at three widths and pin the shell: the left rail from `sm` up and the mobile tab bar
// below it, both present, with the route's own item marked as the current page.
//
// Rendered at App level, not at Cockpit level, because the bug lived in App's own branch. The whole
// App is rendered to static markup — the test environment has no DOM and no effects run, which is
// fine here: the shell and the active item are decided by the first render.
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiContext } from "./context";
import { createMockApi } from "./mock";
import { App } from "./App";
import { navTabs } from "./cockpit/NavRail";
import { FIXED_PAGES, settingsPath } from "./components/settings/nav";
import { parseRoute, routePath, VIEW_PATHS, type Route } from "./routes";

/** The rail's own label for a view: its own tabs plus the Settings row in the foot. */
const RAIL_LABEL = new Map<string, string>(
  [...navTabs({ needCount: 0, liveCount: 0, pendingMemory: 0 }), { view: "settings", label: "settings" }].map((tab) => [tab.view, tab.label]),
);

/** Every path the route table can produce: the plain views, a colony, an org, and every settings page. */
function everyRoute(): { path: string; view: string }[] {
  const plain = Object.entries(VIEW_PATHS).map(([view, path]) => ({ path: path as string, view }));
  const pages = FIXED_PAGES.map((page) => {
    const route = parseRoute(settingsPath(page.id)) as Route;
    return { path: routePath(route), view: route.view };
  });
  return [
    ...plain,
    ...pages,
    { path: "/colonies/demo-repo", view: "colony" },
    { path: "/orgs/acme", view: "overview" },
  ];
}

/**
 * Just enough window for one render: the address the cockpit reads at boot, and a matchMedia that
 * answers by reading the width out of the query, so `(min-width: 640px) and (max-width: 899px)`
 * matches at 760 and not at 1440 or 390.
 */
function stubWindow(path: string, width: number) {
  const store = new Map<string, string>();
  vi.stubGlobal("window", {
    localStorage: {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    },
    matchMedia: (query: string) => {
      const bound = (name: string) => {
        const at = new RegExp(`\\(${name}:\\s*(\\d+)px\\)`).exec(query);
        return at ? Number(at[1]) : null;
      };
      const min = bound("min-width");
      const max = bound("max-width");
      return {
        matches: (min === null || width >= min) && (max === null || width <= max),
        addEventListener() {},
        removeEventListener() {},
      };
    },
    location: { href: `http://cockpit.test${path}`, pathname: path, search: "", hash: "", origin: "http://cockpit.test" },
    history: { pushState() {}, replaceState() {} },
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent() {},
  });
}

/** The markup of one route's app at one window width. */
function renderAt(path: string, width: number): string {
  stubWindow(path, width);
  return renderToStaticMarkup(
    <ApiContext.Provider value={createMockApi()}>
      <App />
    </ApiContext.Provider>,
  );
}

/**
 * The two `<nav aria-label="cockpit">` elements apart: the left rail (`v3-rail`, `sm:flex`) and the
 * phone tab bar (fixed to the foot, `sm:hidden`). Both are in the markup at every width — which is
 * the whole point, since each one hides itself at the other's widths.
 */
function shells(html: string): { rail?: string; tabBar?: string } {
  const navs = html.split('<nav aria-label="cockpit"').slice(1);
  return {
    rail: navs.find((nav) => nav.includes("v3-rail")),
    tabBar: navs.find((nav) => nav.includes("fixed inset-x-0 bottom-0")),
  };
}

/** The `aria-label`s the rail marks as the current page. */
function currentItems(rail: string): string[] {
  return [...rail.matchAll(/aria-label="([^"]+)"[^>]*aria-current="page"/g)].map((m) => m[1]);
}

const WIDTHS = [
  { width: 1440, what: "a wide desktop" },
  { width: 760, what: "a half-width tiled window" },
  { width: 390, what: "a phone" },
];

afterEach(() => vi.unstubAllGlobals());

describe("the shell every route renders inside", () => {
  it.each(WIDTHS)("has the rail and the tab bar on every route, at $what", ({ width }) => {
    for (const { path } of everyRoute()) {
      const html = renderAt(path, width);
      const { rail, tabBar } = shells(html);
      expect(rail, `no left rail at ${width}px on ${path}`).toBeDefined();
      expect(tabBar, `no mobile tab bar at ${width}px on ${path}`).toBeDefined();
      // Neither is a phone-only or a desktop-only shell: each hides itself at the other's widths,
      // but both are mounted so the navigation exists whatever the window is.
      expect(rail).toContain("sm:flex");
      expect(tabBar).toContain("sm:hidden");
    }
  });

  it.each(WIDTHS)("marks the route's own item as the current page, at $what", ({ width }) => {
    for (const { path, view } of everyRoute()) {
      const rail = shells(renderAt(path, width)).rail ?? "";
      // The bare root is the Overview in the route table, but a bookmark of `/` opens wherever the
      // visitor last was — the Nest when nothing is remembered, which is what an empty store means.
      const shown = path === "/" ? "home" : view;
      const label = RAIL_LABEL.get(shown);
      // Launch is the rail's Colonize button and an open colony is shown in the nest's own column,
      // so neither lights an item of its own; both still get the shell above.
      if (!label) continue;
      expect(currentItems(rail), `${path} at ${width}px`).toEqual([label]);
    }
  });

  it("keeps the shell on the plain views and the settings pages alike", () => {
    // The two the report named, plus the paths that used to fall back to the legacy panes.
    for (const path of ["/secrets", "/loops", "/settings", "/settings/connections/api-tokens", "/settings/devices/phone"]) {
      const { rail } = shells(renderAt(path, 760));
      expect(rail, `no left rail on ${path}`).toBeDefined();
    }
  });
});