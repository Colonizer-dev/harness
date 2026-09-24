import { describe, expect, it } from "vitest";
import routesSource from "../public/sw-routes.js?raw";
import manifestSource from "../public/manifest.webmanifest?raw";

// The service worker's routing script, run the way importScripts runs it: against a `self`.
const context: { self: { colonizerRoute?: (url: URL, method: string, mode: string, origin: string) => string } } = { self: {} };
new Function("self", routesSource)(context.self);
const route = (path: string, method = "GET", mode = "cors", origin = "http://127.0.0.1:7878") =>
  context.self.colonizerRoute!(new URL(path, "http://127.0.0.1:7878"), method, mode, origin);

describe("service worker routing", () => {
  it("never touches the API, writes, the sign-in link or other origins", () => {
    expect(route("/api/sessions")).toBe("network");
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

describe("manifest", () => {
  it("installs as a standalone app scoped to the cockpit, with maskable icons", () => {
    const manifest = JSON.parse(manifestSource);
    expect(manifest).toMatchObject({ name: "Colonizer", start_url: "/", scope: "/", display: "standalone" });
    const sizes = manifest.icons.map((i: { sizes: string; purpose: string }) => `${i.sizes}:${i.purpose}`);
    expect(sizes).toEqual(expect.arrayContaining(["192x192:any", "512x512:any", "512x512:maskable"]));
  });
});
