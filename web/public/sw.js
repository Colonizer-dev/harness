// The cockpit's service worker: just enough to make the cockpit an installable app and quick to
// reopen. It caches Vite's hashed /assets and the proxied avatars, answers a few read-only views
// from their last answer while it revalidates them, shows an offline page when the mothership is
// not running, and never touches writes, sign-in or any other /api call (see sw-routes.js).
importScripts("/sw-routes.js");

// Bumped whenever the routing or the caches change: the activate step drops every other cache.
const VERSION = "v2";
const ASSETS = `colonizer-assets-${VERSION}`;
const SHELL = `colonizer-shell-${VERSION}`;
const IMAGES = `colonizer-img-${VERSION}`;
const API = `colonizer-api-${VERSION}`;
const LIMITS = { [ASSETS]: 150, [IMAGES]: 300, [API]: 200 };

self.addEventListener("install", (event) => {
  // A new worker takes over at once: a stale one must not linger after an update.
  event.waitUntil(caches.open(SHELL).then((c) => c.addAll(["/offline.html", "/icons/mark.svg"])).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  const keep = [ASSETS, SHELL, IMAGES, API];
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => !keep.includes(k)).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

async function trim(name, cache) {
  const keys = await cache.keys();
  for (const key of keys.slice(0, Math.max(0, keys.length - LIMITS[name]))) await cache.delete(key);
}

/** From the cache when there, else from the network, cached on the way in when it succeeded. */
function cacheFirst(name, request) {
  return caches.open(name).then(async (cache) => {
    const hit = await cache.match(request);
    if (hit) return hit;
    const response = await fetch(request);
    if (response.ok) {
      await cache.put(request, response.clone());
      trim(name, cache);
    }
    return response;
  });
}

/** The cached answer at once when there is one, refreshed from the network behind it; only a
 *  successful JSON answer is kept, so an error or a sign-in page never is. */
function staleWhileRevalidate(event, request) {
  return caches.open(API).then(async (cache) => {
    const hit = await cache.match(request);
    const refresh = fetch(request).then(async (response) => {
      const json = (response.headers.get("content-type") || "").includes("application/json");
      if (response.ok && json) {
        await cache.put(request, response.clone());
        trim(API, cache);
      }
      return response;
    });
    if (hit) {
      event.waitUntil(refresh.catch(() => undefined));
      return hit;
    }
    return refresh;
  });
}

self.addEventListener("fetch", (event) => {
  const request = event.request;
  const route = self.colonizerRoute(new URL(request.url), request.method, request.mode, self.location.origin);
  if (route === "asset") {
    event.respondWith(cacheFirst(ASSETS, request));
  } else if (route === "image") {
    event.respondWith(cacheFirst(IMAGES, request));
  } else if (route === "swr") {
    event.respondWith(staleWhileRevalidate(event, request));
  } else if (route === "page") {
    event.respondWith(fetch(request).catch(() => caches.match("/offline.html")));
  }
  // "network": not handled, so the browser does exactly what it would without a worker.
});
