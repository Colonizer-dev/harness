// How the cockpit's service worker treats a request. Kept in its own file so the tests can load it.
//
//  - "network": left alone. Every other /api call, every write, the ?token= sign-in, a ?refresh=
//               the operator asked for, anything cross-origin.
//  - "asset":   /assets/* — Vite's content-hashed files, which never change under one name — served
//               from the cache first and cached on the way in.
//  - "image":   /api/img — GitHub avatars through the mothership's own week-long cache — served from
//               the cache first.
//  - "swr":     a short allowlist of read-only GET JSON (repository meta, packages, lines of code):
//               the last answer at once, revalidated behind it. Nothing that signs in, changes
//               state or streams is ever on it.
//  - "page":    a navigation. Always from the network (the cockpit is live data; a cached page would
//               be a stale build), falling back to the offline page when the mothership is down.
self.colonizerSwr = [
  /^\/api\/repos\/[^/]+\/[^/]+\/(meta|loc|packages|published|dependencies|supply-chain)$/,
  /^\/api\/orgs\/[^/]+\/packages\/(published|dependencies|supply-chain)$/,
];

self.colonizerRoute = function colonizerRoute(url, method, mode, origin) {
  if (method !== "GET") return "network";
  if (url.origin !== origin) return "network";
  if (url.searchParams.has("token")) return "network";
  if (url.pathname === "/api/img") return url.searchParams.has("u") ? "image" : "network";
  if (url.pathname === "/api" || url.pathname.startsWith("/api/")) {
    if (mode === "navigate" || url.searchParams.has("refresh")) return "network";
    return self.colonizerSwr.some((re) => re.test(url.pathname)) ? "swr" : "network";
  }
  if (url.pathname.startsWith("/assets/")) return "asset";
  if (mode === "navigate") return "page";
  return "network";
};
