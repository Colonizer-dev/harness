// The cockpit's URLs (issue #1180): every view has a real path, so a view can be bookmarked,
// shared, and walked with the browser's back and forward buttons. Pure string work, no window, so
// the tests can pin the round trip: formatRoute(parseRoute(x)) is x for every path the cockpit makes.
//
//   /                          Overview          /history        History
//   /nest                      Nest              /queues         Queues
//   /chat                      Chat              /loops          Loops
//   /chat                      Chat              /memory         Memory
//   /code                      Code              /host           Host
//   /launch                    Launch            /secrets        Secrets (also /settings/secrets)
//   /inbox                     Inbox             /settings       Settings (the list on a phone)
//   /colonies/<id>[/<tab>]     One colony        /settings/<group>/<page>   One settings page
//   /orgs/<org>                Overview, scoped to one workspace
//
// The workspace selector rides on every other path as `?org=<org>`; other query parameters
// (`?mock=1`, the launch params) are never touched, and `?token=` is never written.
import type { CockpitView } from "./cockpit/NavRail";
import { sectionFromSettingsPath, settingsPath } from "./components/settings/nav";
import type { SectionId } from "./components/settings/ui";

export type ColonyTab = "chat" | "terminal";
export const COLONY_TABS: readonly ColonyTab[] = ["chat", "terminal"];

export interface Route {
  view: CockpitView;
  /** `colony` only: the colony's id. */
  colony?: string;
  colonyTab?: ColonyTab;
  /** `settings` only: the page, or null for the section list. */
  section?: SectionId | null;
  /** The workspace the URL names; undefined when it names none (every workspace). */
  org?: string;
}

/** The view each plain path belongs to. `/` is the Overview. Exported so a test can walk them all. */
export const VIEW_PATHS: Partial<Record<CockpitView, string>> = {
  overview: "/",
  home: "/nest",
  chat: "/chat",
  code: "/code",
  launch: "/launch",
  inbox: "/inbox",
  history: "/history",
  queues: "/queues",
  loops: "/loops",
  memory: "/memory",
  host: "/host",
  secrets: "/secrets",
  settings: "/settings",
};

const PATH_VIEWS = new Map<string, CockpitView>(Object.entries(VIEW_PATHS).map(([view, path]) => [path as string, view as CockpitView]));

/** The part of the path that is the app's own, with the deploy base (`/demo/`) and trailing slashes removed. */
function appPath(pathname: string, base: string): string {
  let path = pathname;
  const prefix = base.replace(/\/+$/, "");
  if (prefix && (path === prefix || path.startsWith(`${prefix}/`))) path = path.slice(prefix.length) || "/";
  return path.length > 1 ? path.replace(/\/+$/, "") : path;
}

function decode(segment: string): string | null {
  try {
    return decodeURIComponent(segment);
  } catch {
    return null;
  }
}

/**
 * The route a location names, or null for a path the cockpit has no view at (the caller keeps what
 * it had). `search` is the query string with or without its `?`.
 */
export function parseRoute(pathname: string, search = "", base = "/"): Route | null {
  const path = appPath(pathname, base);
  const org = new URLSearchParams(search).get("org") || undefined;
  const parts = path.split("/").filter(Boolean);

  if (parts[0] === "orgs" && parts.length === 2) {
    const name = decode(parts[1]);
    return name ? { view: "overview", org: name } : null;
  }
  if (parts[0] === "colonies" && parts.length >= 2 && parts.length <= 3) {
    const id = decode(parts[1]);
    if (!id) return null;
    const tab = parts[2] as ColonyTab | undefined;
    if (tab !== undefined && !COLONY_TABS.includes(tab)) return null;
    return { view: "colony", colony: id, colonyTab: tab, org };
  }
  if (parts[0] === "settings" && parts[1] === "secrets" && parts.length === 2) return { view: "secrets", org };
  if (parts[0] === "settings" && parts.length > 1) {
    if (parts.length !== 3) return null;
    const section = sectionFromSettingsPath(path);
    // Secrets is a view of its own, whichever group's address it is reached by.
    return section === "secrets" ? { view: "secrets", org } : { view: "settings", section, org };
  }
  const view = PATH_VIEWS.get(path);
  return view ? (view === "settings" ? { view, section: null, org } : { view, org }) : null;
}

/** The path (no query) of a route. A colony view with no colony, or any unknown view, reads as the Nest. */
export function routePath(route: Route): string {
  if (route.view === "colony") {
    if (!route.colony) return "/nest";
    return `/colonies/${encodeURIComponent(route.colony)}${route.colonyTab ? `/${route.colonyTab}` : ""}`;
  }
  if (route.view === "settings" && route.section) return settingsPath(route.section);
  if (route.view === "overview" && route.org) return `/orgs/${encodeURIComponent(route.org)}`;
  return VIEW_PATHS[route.view] ?? "/nest";
}

/**
 * The address for a route: its path plus the query, where `search` (the current one) keeps
 * everything but `org` and `token`, and `org` is written back for every view that is not the
 * org's own Overview, where the path says it.
 */
export function formatRoute(route: Route, search = "", base = "/"): string {
  const path = routePath(route);
  const params = new URLSearchParams(search);
  params.delete("org");
  params.delete("token");
  if (route.org && !(route.view === "overview" && path.startsWith("/orgs/"))) params.set("org", route.org);
  const query = params.toString();
  const prefix = base.replace(/\/+$/, "");
  return `${prefix}${path === "/" && prefix ? "/" : path}${query ? `?${query}` : ""}`;
}

/** Whether a route's path is the bare root: where a returning visitor's remembered view still wins. */
export function isRootPath(pathname: string, base = "/"): boolean {
  return appPath(pathname, base) === "/";
}
