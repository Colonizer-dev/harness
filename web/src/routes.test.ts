// The cockpit's URLs (issue #1180): every view has one, and parsing then formatting gives it back.
import { describe, expect, it } from "vitest";

import { COCKPIT_VIEWS } from "./launchUrl";
import { formatRoute, isRootPath, parseRoute, routePath, type Route } from "./routes";
import { FIXED_PAGES, sectionFromSettingsPath, settingsPath } from "./components/settings/nav";

describe("parseRoute", () => {
  it("reads every plain view's path", () => {
    const table: [string, Route["view"]][] = [
      ["/", "overview"],
      ["/nest", "home"],
      ["/chat", "chat"],
      ["/code", "code"],
      ["/launch", "launch"],
      ["/inbox", "inbox"],
      ["/history", "history"],
      ["/loops", "loops"],
      ["/memory", "memory"],
      ["/host", "host"],
      ["/secrets", "secrets"],
      ["/settings", "settings"],
    ];
    for (const [path, view] of table) expect(parseRoute(path)?.view, path).toBe(view);
  });

  it("reads a colony and its tab", () => {
    expect(parseRoute("/colonies/abc123")).toEqual({ view: "colony", colony: "abc123", colonyTab: undefined, org: undefined });
    expect(parseRoute("/colonies/abc123/terminal")?.colonyTab).toBe("terminal");
    expect(parseRoute("/colonies/a%2Fb")?.colony).toBe("a/b");
    expect(parseRoute("/colonies/abc/nonsense")).toBeNull();
    expect(parseRoute("/colonies")).toBeNull();
  });

  it("reads a workspace from its own path and from ?org=", () => {
    expect(parseRoute("/orgs/Colonizer-dev")).toEqual({ view: "overview", org: "Colonizer-dev" });
    expect(parseRoute("/nest", "?org=acme")?.org).toBe("acme");
    expect(parseRoute("/nest", "?mock=1")?.org).toBeUndefined();
  });

  it("reads a settings page, and Secrets under /settings as the Secrets view", () => {
    expect(parseRoute("/settings/models/providers")).toEqual({ view: "settings", section: "providers", org: undefined });
    expect(parseRoute("/settings/runtime/module-source")?.section).toBe("module:source");
    expect(parseRoute("/settings/workspaces/org-acme")?.section).toBe("org:acme");
    expect(parseRoute("/settings/models/not-a-page")).toEqual({ view: "settings", section: null, org: undefined });
    expect(parseRoute("/settings/secrets")?.view).toBe("secrets");
    expect(parseRoute("/settings/connections/secrets")?.view).toBe("secrets");
    expect(parseRoute("/settings/a/b/c")).toBeNull();
  });

  it("ignores a trailing slash and the deploy base, and refuses a path with no view", () => {
    expect(parseRoute("/nest/")?.view).toBe("home");
    expect(parseRoute("/demo/settings/models/providers", "", "/demo/")?.section).toBe("providers");
    expect(parseRoute("/demo/", "", "/demo/")?.view).toBe("overview");
    expect(parseRoute("/api/status")).toBeNull();
    expect(parseRoute("/assets/index.js")).toBeNull();
  });
});

describe("formatRoute", () => {
  const paths = [
    "/",
    "/nest",
    "/chat",
    "/code",
    "/launch",
    "/inbox",
    "/history",
    "/loops",
    "/memory",
    "/host",
    "/secrets",
    "/settings",
    "/colonies/abc123",
    "/colonies/abc123/terminal",
    "/orgs/acme",
    "/settings/general/cockpit",
    "/settings/models/providers",
    "/settings/runtime/module-source",
    "/settings/workspaces/org-acme",
  ];

  it("round-trips every path the cockpit makes", () => {
    for (const path of paths) {
      const route = parseRoute(path);
      expect(route, path).not.toBeNull();
      expect(formatRoute(route as Route), path).toBe(path);
    }
  });

  it("round-trips the workspace on a path that is not its Overview, and keeps other query parameters", () => {
    expect(formatRoute({ view: "home", org: "acme" })).toBe("/nest?org=acme");
    expect(formatRoute({ view: "overview", org: "acme" })).toBe("/orgs/acme");
    expect(formatRoute({ view: "home" }, "?mock=1")).toBe("/nest?mock=1");
    expect(formatRoute({ view: "home", org: "acme" }, "?mock=1&org=old")).toBe("/nest?mock=1&org=acme");
    expect(parseRoute("/nest", "?org=acme")).toEqual({ view: "home", org: "acme" });
  });

  it("never writes a sign-in token into the address", () => {
    expect(formatRoute({ view: "home" }, "?token=secret&mock=1")).toBe("/nest?mock=1");
    expect(formatRoute({ view: "settings", section: "providers" }, "?token=secret")).toBe("/settings/models/providers");
  });

  it("writes the deploy base back", () => {
    expect(formatRoute({ view: "home" }, "", "/demo/")).toBe("/demo/nest");
    expect(formatRoute({ view: "overview" }, "", "/demo/")).toBe("/demo/");
  });

  it("sends a colony view with no colony to the Nest, and names a path for every view", () => {
    expect(routePath({ view: "colony" })).toBe("/nest");
    for (const view of COCKPIT_VIEWS) expect(routePath({ view, colony: "x" }), view).toMatch(/^\//);
  });

  it("is quiet about the root: only the bare root lets a remembered view win", () => {
    expect(isRootPath("/")).toBe(true);
    expect(isRootPath("/nest")).toBe(false);
    expect(isRootPath("/demo/", "/demo/")).toBe(true);
  });
});

describe("settings paths", () => {
  it("round-trips every fixed page, and every module and workspace page", () => {
    for (const page of FIXED_PAGES) {
      const path = settingsPath(page.id);
      if (page.view) expect(path).toBe(`/${page.view}`);
      else expect(sectionFromSettingsPath(path), path).toBe(page.id);
    }
    for (const id of ["module:source", "module:burn_down", "org:Colonizer-dev", "org:a b"] as const) {
      expect(sectionFromSettingsPath(settingsPath(id)), id).toBe(id);
    }
  });

  it("gives each page its own address", () => {
    const paths = FIXED_PAGES.map((p) => settingsPath(p.id));
    expect(new Set(paths).size).toBe(paths.length);
  });
});
