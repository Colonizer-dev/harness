// The cockpit's service worker: just enough to make the cockpit an installable app. It caches
// Vite's hashed /assets, shows an offline page when the mothership is not running, and never
// touches /api, writes or the sign-in link (see sw-routes.js).
importScripts("/sw-routes.js");

const ASSETS = "colonizer-assets-v1";
const SHELL = "colonizer-shell-v1";
const MAX_ASSETS = 150;

self.addEventListener("install", (event) => {
  // A new worker takes over at once: a stale one must not linger after an update.
  event.waitUntil(caches.open(SHELL).then((c) => c.addAll(["/offline.html", "/icons/mark.svg"])).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== ASSETS && k !== SHELL).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

async function trim(cache) {
  const keys = await cache.keys();
  for (const key of keys.slice(0, Math.max(0, keys.length - MAX_ASSETS))) await cache.delete(key);
}

self.addEventListener("fetch", (event) => {
  const request = event.request;
  const route = self.colonizerRoute(new URL(request.url), request.method, request.mode, self.location.origin);
  if (route === "asset") {
    event.respondWith(
      caches.open(ASSETS).then(async (cache) => {
        const hit = await cache.match(request);
        if (hit) return hit;
        const response = await fetch(request);
        if (response.ok) {
          await cache.put(request, response.clone());
          trim(cache);
        }
        return response;
      }),
    );
  } else if (route === "page") {
    event.respondWith(fetch(request).catch(() => caches.match("/offline.html")));
  }
  // "network": not handled, so the browser does exactly what it would without a worker.
});
