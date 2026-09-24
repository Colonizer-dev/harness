import { describe, expect, it } from "vitest";
import routesSource from "../public/sw-routes.js?raw";
import manifestSource from "../public/manifest.webmanifest?raw";
import workerSource from "../public/sw.js?raw";

// The service worker's routing script, run the way importScripts runs it: against a `self`.
const context: { self: { colonizerRoute?: (url: URL, method: string, mode: string, origin: string) => string } } = { self: {} };
new Function("self", routesSource)(context.self);
const route = (path: string, method = "GET", mode = "cors", origin = "http://127.0.0.1:7878") =>
  context.self.colonizerRoute!(new URL(path, "http://127.0.0.1:7878"), method, mode, origin);

describe("service worker routing", () => {
  it("never touches the API, writes, the sign-in link or other origins", () => {
    expect(route("/api/sessions")).toBe("network");
    expect(route("/api/status")).toBe("network");
    expect(route("/api/settings")).toBe("network");
    expect(route("/api/repos")).toBe("network");
    expect(route("/api/repos/acme/web/meta", "POST")).toBe("network");
    expect(route("/api/repos/acme/web/edits")).toBe("network");
    expect(route("/api/repos/acme/web/blob?path=a")).toBe("network");
    expect(route("/api/repos/acme/web/meta?token=abc")).toBe("network");
    expect(route("/api/repos/acme/web/meta", "GET", "cors", "http://evil.test")).toBe("network");
    expect(route("/api")).toBe("network");
    expect(route("/api/stream", "GET", "navigate")).toBe("network");
    expect(route("/assets/index-abc.js", "POST")).toBe("network");
    expect(route("/?token=abc", "GET", "navigate")).toBe("network");
    expect(route("/assets/x.js", "GET", "cors", "http://evil.test")).toBe("network");
  });

  it("caches hashed assets and sends pages to the network with an offline fallback", () => {
    expect(route("/assets/index-CHBkpbe7.js")).toBe("asset");
    expect(route("/", "GET", "navigate")).toBe("page");
    expect(route("/icons/icon-192.png")).toBe("network");
    expect(route("/manifest.webmanifest")).toBe("network");
  });
});

describe("service worker caching of read-only views", () => {
  it("serves proxied avatars from the cache first", () => {
    expect(route("/api/img?u=https%3A%2F%2Fgithub.com%2Focto.png")).toBe("image");
    expect(route("/api/img")).toBe("network");
    expect(route("/api/img?u=x", "POST")).toBe("network");
  });

  it("revalidates only the allowlisted read-only JSON views", () => {
    for (const path of [
      "/api/repos/acme/web/meta",
      "/api/repos/acme/web/loc",
      "/api/repos/acme/web/packages",
      "/api/repos/acme/web/supply-chain",
      "/api/orgs/acme/packages/published",
      "/api/orgs/acme/packages/dependencies",
      "/api/orgs/acme/packages/supply-chain",
    ]) {
      expect(route(path)).toBe("swr");
    }
    expect(route("/api/orgs/acme/packages/published?refresh=1")).toBe("network");
    expect(route("/api/repos/acme/web/meta/extra")).toBe("network");
    expect(route("/api/orgs/acme/settings")).toBe("network");
  });

  it("names a new cache version, so the activate step drops the old caches", () => {
    expect(workerSource).toMatch(/const VERSION = "v2"/);
  });
});

describe("manifest", () => {
  it("installs as a standalone app scoped to the cockpit, with maskable icons", () => {
    const manifest = JSON.parse(manifestSource);
    expect(manifest).toMatchObject({ name: "Colonizer", start_url: "/", scope: "/", display: "standalone" });
    const sizes = manifest.icons.map((i: { sizes: string; purpose: string }) => `${i.sizes}:${i.purpose}`);
    expect(sizes).toEqual(expect.arrayContaining(["192x192:any", "512x512:any", "512x512:maskable"]));
  });
});
