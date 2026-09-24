// How the cockpit's service worker treats a request. Kept in its own file so the tests can load it.
//
//  - "network": left alone. Every /api call, every write, the ?token= sign-in, anything cross-origin.
//  - "asset":   /assets/* — Vite's content-hashed files, which never change under one name — served
//               from the cache first and cached on the way in.
//  - "page":    a navigation. Always from the network (the cockpit is live data; a cached page would
//               be a stale build), falling back to the offline page when the mothership is down.
self.colonizerRoute = function colonizerRoute(url, method, mode, origin) {
  if (method !== "GET") return "network";
  if (url.origin !== origin) return "network";
  if (url.pathname === "/api" || url.pathname.startsWith("/api/")) return "network";
  if (url.searchParams.has("token")) return "network";
  if (url.pathname.startsWith("/assets/")) return "asset";
  if (mode === "navigate") return "page";
  return "network";
};
