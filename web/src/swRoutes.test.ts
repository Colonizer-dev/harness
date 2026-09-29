import { describe, expect, it, vi } from "vitest";
import routesSource from "../public/sw-routes.js?raw";
import manifestSource from "../public/manifest.webmanifest?raw";
import workerSource from "../public/sw.js?raw";
import { BUILD_LINE, buildHash, withBuild } from "./swBuild";

// The service worker's routing script, run the way importScripts runs it: against a `self`.
const context: {
  self: {
    colonizerRoute?: (url: URL, method: string, mode: string, origin: string) => string;
    colonizerSafeUrl?: (url: unknown) => string;
    colonizerPushPayload?: (raw: unknown) => { title: string; body: string; url: string; tag: string };
  };
} = { self: {} };
new Function("self", routesSource)(context.self);
const route = (path: string, method = "GET", mode = "cors", origin = "http://127.0.0.1:7878") =>
  context.self.colonizerRoute!(new URL(path, "http://127.0.0.1:7878"), method, mode, origin);
const safeUrl = (url: unknown) => context.self.colonizerSafeUrl!(url);
const payload = (raw: unknown) => context.self.colonizerPushPayload!(raw);

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
    expect(workerSource).toMatch(/const VERSION = "v4"/);
  });
});

// --- The update path, run the way a browser runs the worker: a `self` with captured listeners, a
// stub importScripts, a cache stack over Maps and a fetch stub. This is the build-swap story: the
// build fills in BUILD, install precaches it without taking over, activate keeps one build back,
// and an old build's chunk is answered from its cache after the mothership stopped serving it.

interface FakeCache {
  match(request: RequestInfo | URL): Promise<Response | undefined>;
  put(request: RequestInfo | URL, response: Response): Promise<void>;
  addAll(urls: readonly string[]): Promise<void>;
  keys(): Promise<string[]>;
  delete(request: RequestInfo | URL): Promise<boolean>;
}

const ORIGIN = "http://127.0.0.1:7878";
/** Cache keys are absolute urls, as the real Cache API stores them. */
const urlOf = (request: RequestInfo | URL) => new URL(typeof request === "string" ? request : request instanceof URL ? request.href : request.url, ORIGIN).href;

/** Caches over Maps, seeded with name → url → body. */
function fakeCaches(seed: Record<string, Record<string, string>> = {}) {
  const store = new Map<string, Map<string, Response>>(
    Object.entries(seed).map(([name, entries]) => [name, new Map(Object.entries(entries).map(([url, body]) => [urlOf(url), new Response(body)]))]),
  );
  const deleted: string[] = [];
  const cache = (entries: Map<string, Response>): FakeCache => ({
    match: async (request) => {
      const hit = entries.get(urlOf(request));
      return hit ? hit.clone() : undefined;
    },
    put: async (request, response) => void entries.set(urlOf(request), response.clone()),
    addAll: async (urls) => {
      for (const url of urls) entries.set(urlOf(url), new Response(`cached ${url}`));
    },
    keys: async () => [...entries.keys()],
    delete: async (request) => entries.delete(urlOf(request)),
  });
  return {
    store,
    deleted,
    caches: {
      keys: async () => [...store.keys()],
      delete: async (name: string) => {
        deleted.push(name);
        return store.delete(name);
      },
      match: async (request: RequestInfo | URL) => {
        for (const entries of store.values()) {
          const hit = await cache(entries).match(request);
          if (hit) return hit;
        }
        return undefined;
      },
      open: async (name: string) => {
        let entries = store.get(name);
        if (!entries) store.set(name, (entries = new Map()));
        return cache(entries);
      },
    },
  };
}

/** Runs one worker source against `caches`, with spies; dispatch resolves the waitUntil promises. */
function runWorker(source: string, caches: ReturnType<typeof fakeCaches>["caches"], fetchImpl: (input: RequestInfo) => Promise<Response> = async () => new Response(null, { status: 404 })) {
  type WorkerEvent = Record<string, unknown> & { waitUntil: (p: Promise<unknown>) => void };
  const listeners: Record<string, Array<(event: WorkerEvent) => void>> = {};
  const skipWaiting = vi.fn(async () => undefined);
  const claimed = vi.fn(async () => undefined);
  const self: Record<string, unknown> = {
    addEventListener: (type: unknown, handler: (event: WorkerEvent) => void) => void (listeners[type as string] ??= []).push(handler),
    location: { origin: ORIGIN },
    registration: { skipWaiting },
    // skipWaiting lives on the worker global itself, and activate claims the clients.
    skipWaiting,
    clients: { claim: claimed },
  };
  new Function("self", "importScripts", "caches", "fetch", source)(self, () => new Function("self", routesSource)(self), caches, fetchImpl);
  return {
    skipWaiting,
    claimed,
    dispatch: async (type: string, event: Record<string, unknown> = {}) => {
      const pending: Promise<unknown>[] = [];
      for (const handler of listeners[type] ?? []) handler({ waitUntil: (p) => void pending.push(p), ...event });
      await Promise.all(pending);
    },
  };
}

describe("service worker update across a build swap", () => {
  const buildCache = (hash: string) => `colonizer-build-${hash}`;
  /** The worker as the build would have rewritten it: BUILD set to one hash and its asset list. */
  const workerFor = (hash: string, assets: string[]) => withBuild(workerSource, hash, assets);
  /** Dispatches one fetch and returns the worker's answer. */
  const respond = async (worker: ReturnType<typeof runWorker>, url: string) => {
    let answer!: Promise<Response>;
    await worker.dispatch("fetch", { request: new Request(url), respondWith: (p: Promise<Response>) => void (answer = p) });
    return answer;
  };

  it("carries the BUILD placeholder exactly once, and withBuild fills it in", () => {
    expect(workerSource.split(BUILD_LINE)).toHaveLength(2);
    const filled = withBuild(workerSource, "bee2f00d", ["/assets/index-a1.js", "/assets/ChatView-b2.js"]);
    expect(filled).toContain('const BUILD = { hash: "bee2f00d", assets: ["/assets/index-a1.js","/assets/ChatView-b2.js"] };');
    expect(filled).not.toContain(BUILD_LINE);
  });

  it("withBuild fails the build on a drifted worker: placeholder missing or doubled", () => {
    expect(() => withBuild("const BUILD = null;", "h", [])).toThrow(/exactly once/);
    expect(() => withBuild(`${BUILD_LINE}\n//\n${BUILD_LINE}`, "h", [])).toThrow(/exactly once/);
  });

  it("buildHash is stable for a build and moves when its files move", () => {
    const build = ["/assets/ChatView-b2.js", "/assets/index-a1.js"];
    expect(buildHash(build)).toMatch(/^[0-9a-f]{8}$/);
    expect(buildHash(build)).toBe(buildHash([...build]));
    expect(buildHash(build)).not.toBe(buildHash(["/assets/ChatView-c3.js", "/assets/index-a1.js"]));
  });

  it("install precaches this build's assets into its own cache and does not take over", async () => {
    const fake = fakeCaches();
    const fetchImpl = vi.fn(async (input: RequestInfo) => new Response(`cached ${input}`));
    const worker = runWorker(workerFor("bee2f00d", ["/assets/a.js", "/assets/b.js"]), fake.caches, fetchImpl);
    await worker.dispatch("install");
    expect([...(fake.store.get(buildCache("bee2f00d"))?.keys() ?? [])].sort()).toEqual([`${ORIGIN}/assets/a.js`, `${ORIGIN}/assets/b.js`]);
    expect(worker.skipWaiting).not.toHaveBeenCalled();
  });

  it("one build asset failing to fetch neither fails the install nor blocks the others", async () => {
    const fake = fakeCaches();
    const fetchImpl = vi.fn(async (input: RequestInfo) =>
      String(input).endsWith("bad.js") ? new Response(null, { status: 404 }) : new Response("chunk"),
    );
    const worker = runWorker(workerFor("bee2f00d", ["/assets/bad.js", "/assets/good.js"]), fake.caches, fetchImpl);
    await worker.dispatch("install");
    expect(await (await fake.caches.open(buildCache("bee2f00d"))).match("/assets/good.js").then((hit) => hit?.text())).toBe("chunk");
    expect(await (await fake.caches.open(buildCache("bee2f00d"))).match("/assets/bad.js")).toBeUndefined();
  });

  it("the waiting worker takes over only when a tab sends colonizer:skip-waiting", async () => {
    const worker = runWorker(workerFor("bee2f00d", []), fakeCaches().caches);
    await worker.dispatch("message", { data: { type: "colonizer:open" } });
    await worker.dispatch("message", {});
    expect(worker.skipWaiting).not.toHaveBeenCalled();
    await worker.dispatch("message", { data: { type: "colonizer:skip-waiting" } });
    expect(worker.skipWaiting).toHaveBeenCalled();
  });

  it("activate keeps this and the previous build's caches and drops the rest, and records itself", async () => {
    const fake = fakeCaches({
      "colonizer-shell-v4": { "/__colonizer-build": "aaa" },
      "colonizer-build-aaa": { "/assets/a.js": "previous build" },
      "colonizer-build-ccc": { "/assets/c.js": "two builds ago" },
      "colonizer-assets-v3": { "/assets/x.js": "a cache from the old VERSION" },
    });
    const worker = runWorker(workerFor("bbb", ["/assets/b.js"]), fake.caches);
    await worker.dispatch("install");
    await worker.dispatch("activate");
    expect([...fake.store.keys()].sort()).toEqual(["colonizer-build-aaa", "colonizer-build-bbb", "colonizer-shell-v4"].sort());
    expect(fake.deleted).toEqual(expect.arrayContaining(["colonizer-build-ccc", "colonizer-assets-v3"]));
    expect(await (await fake.caches.open("colonizer-shell-v4")).match("/__colonizer-build").then((hit) => hit?.text())).toBe("bbb");
    expect(worker.claimed).toHaveBeenCalled();
  });

  it("with no record — the first run after a VERSION bump — every build cache is kept", async () => {
    const fake = fakeCaches({ "colonizer-shell-v4": {}, "colonizer-build-aaa": { "/assets/a.js": "previous" } });
    const worker = runWorker(workerFor("bbb", ["/assets/b.js"]), fake.caches);
    await worker.dispatch("install");
    await worker.dispatch("activate");
    expect([...fake.store.keys()].sort()).toEqual(["colonizer-build-aaa", "colonizer-build-bbb", "colonizer-shell-v4"].sort());
  });

  it("an old build's chunk is served from its cache after the server stopped serving it", async () => {
    const fake = fakeCaches({ "colonizer-build-aaa": { "/assets/old.js": "old chunk" } });
    const fetchImpl = vi.fn(async () => new Response(null, { status: 404 }));
    const worker = runWorker(workerFor("bbb", ["/assets/new.js"]), fake.caches, fetchImpl);
    const hit = await respond(worker, "http://127.0.0.1:7878/assets/old.js");
    expect(hit.status).toBe(200);
    expect(await hit.text()).toBe("old chunk");
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("prefers this build's cache and fills a miss into it from the network", async () => {
    const fake = fakeCaches({
      "colonizer-build-aaa": { "/assets/a.js": "old build's copy" },
      "colonizer-build-bbb": { "/assets/b.js": "this build's copy" },
    });
    const fetchImpl = vi.fn(async () => new Response("fetched", { status: 200 }));
    const worker = runWorker(workerFor("bbb", ["/assets/b.js"]), fake.caches, fetchImpl);
    expect(await (await respond(worker, "http://127.0.0.1:7878/assets/b.js")).text()).toBe("this build's copy");
    const missed = await respond(worker, "http://127.0.0.1:7878/assets/late.js");
    expect(await missed.text()).toBe("fetched");
    expect(await (await fake.caches.open(buildCache("bbb"))).match("/assets/late.js").then((hit) => hit?.text())).toBe("fetched");
    // An older build's chunk still answers: the miss never displaced it.
    expect(await (await respond(worker, "http://127.0.0.1:7878/assets/a.js")).text()).toBe("old build's copy");
  });
});

describe("push notifications", () => {
  it("only a same-origin relative path may be opened, else the cockpit root", () => {
    expect(safeUrl("/?colony=demo1234")).toBe("/?colony=demo1234");
    expect(safeUrl("/")).toBe("/");
    expect(safeUrl("//evil.test/x")).toBe("/");
    expect(safeUrl("/\\evil.test")).toBe("/");
    expect(safeUrl("https://evil.test/")).toBe("/");
    expect(safeUrl("colony")).toBe("/");
    expect(safeUrl(undefined)).toBe("/");
    expect(safeUrl(null)).toBe("/");
  });

  it("shows the mothership's payload as it arrived, with its deep link kept", () => {
    expect(payload(JSON.stringify({ title: "acme/webshop #42", body: "needs an answer", url: "/?colony=demo1234", tag: "colonizer:demo1234" }))).toEqual({
      title: "acme/webshop #42",
      body: "needs an answer",
      url: "/?colony=demo1234",
      tag: "colonizer:demo1234",
    });
  });

  it("never throws on a malformed, empty or absent payload — a generic notification instead", () => {
    const generic = { title: "Colonizer", body: "A colony needs you.", url: "/", tag: "" };
    expect(payload("")).toEqual(generic);
    expect(payload("not json{")).toEqual(generic);
    expect(payload(null)).toEqual(generic);
    expect(payload(undefined)).toEqual(generic);
    expect(payload("[1,2]")).toEqual(generic);
    expect(payload(JSON.stringify({ title: "  ", body: 42, url: "https://evil.test/" }))).toEqual({ title: "Colonizer", body: "A colony needs you.", url: "/", tag: "" });
    // A payload with only a title still degrades field by field, and its url is sanitized.
    expect(payload(JSON.stringify({ title: "acme/webshop #42", url: "//evil.test" }))).toEqual({ title: "acme/webshop #42", body: "A colony needs you.", url: "/", tag: "" });
  });

  it("the worker shows and opens notifications, and always waits on them", () => {
    expect(workerSource).toMatch(/addEventListener\("push"/);
    expect(workerSource).toMatch(/addEventListener\("notificationclick"/);
    expect(workerSource).toMatch(/showNotification\(/);
    expect(workerSource).toMatch(/colonizer:open/);
    expect(workerSource).toMatch(/clients\.openWindow\(/);
  });
});

describe("manifest", () => {
  // The image files the manifest names live in public/icons: the server serves /icons/* before
  // sign-in (is_public_app_file, server.rs), which is why every asset lives there rather than
  // beside the manifest. import.meta.glob lists them off disk without importing a byte — the
  // suite's tsconfig keeps node's types out (`types: ["vite/client"]`), so there is no fs here.
  const iconsOnDisk = new Set(Object.keys(import.meta.glob("../public/icons/*")).map((file) => `/icons/${file.split("/").pop()}`));
  const manifest = JSON.parse(manifestSource);

  it("installs as a standalone app scoped to the cockpit, with maskable icons", () => {
    expect(manifest).toMatchObject({ name: "Colonizer", start_url: "/", scope: "/", display: "standalone" });
    const sizes = manifest.icons.map((i: { sizes: string; purpose: string }) => `${i.sizes}:${i.purpose}`);
    expect(sizes).toEqual(expect.arrayContaining(["192x192:any", "512x512:any", "192x192:maskable", "512x512:maskable"]));
    for (const icon of manifest.icons) expect(iconsOnDisk).toContain(icon.src);
  });

  it("offers the three shortcuts, each naming a real view within scope", () => {
    expect(manifest.shortcuts.map((s: { url: string }) => s.url)).toEqual(["/?view=inbox", "/?view=launch", "/?view=home"]);
    for (const shortcut of manifest.shortcuts) {
      expect(new URL(shortcut.url, "https://cockpit.test").toString()).toMatch(/^https:\/\/cockpit\.test\/\?/);
      expect(shortcut.name).toBeTruthy();
      expect(shortcut.short_name).toBeTruthy();
      expect(shortcut.description).toBeTruthy();
      const shortcutIcons = shortcut.icons as { src: string }[];
      expect(shortcutIcons.length).toBeGreaterThan(0);
      for (const icon of shortcutIcons) expect(iconsOnDisk).toContain(icon.src);
      const view = new URL(shortcut.url, "https://cockpit.test").searchParams.get("view");
      expect(["inbox", "launch", "home"]).toContain(view);
    }
  });

  it("receives shared links as distinct query params, on GET at the scope root", () => {
    expect(manifest.share_target).toMatchObject({
      action: "/",
      method: "GET",
      params: { title: "share_title", text: "share_text", url: "share_url" },
    });
  });

  it("carries a wide and a narrow screenshot, each labelled and declared at a pixel size", () => {
    const shots = manifest.screenshots as { src: string; sizes: string; type: string; form_factor: string; label: string }[];
    expect(shots.filter((s) => s.form_factor === "wide").length).toBeGreaterThan(0);
    expect(shots.filter((s) => s.form_factor === "narrow").length).toBeGreaterThan(0);
    for (const shot of shots) {
      expect(shot.label).toBeTruthy();
      expect(shot.sizes).toMatch(/^\d+x\d+$/);
      expect(iconsOnDisk).toContain(shot.src);
    }
  });
});
