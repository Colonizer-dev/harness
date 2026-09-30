// How the cockpit's service worker treats a request. Kept in its own file so the tests can load it.
//
//  - "network": left alone. Every other /api call, every write, the ?token= sign-in, a ?refresh=
//               the operator asked for, anything cross-origin.
//  - "asset":   /assets/* — Vite's content-hashed files, which never change under one name — served
//               from whichever build's cache has them (this build's first) and cached on the way in.
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

// --- Push notifications -----------------------------------------------------------------------
//
// The mothership encrypts one small JSON payload per push: {title, body, url, tag, silent}, plus
// `answer: {token, choices}` on a question the notification itself may answer. These run inside the
// worker but are kept here, pure, so the tests can load them exactly like colonizerRoute.

/**
 * The only urls a notification may open: a same-origin relative path — starting with "/" and not
 * "//" (a protocol-relative url names another origin) and never "/\" (which some url parsers read
 * back as "//"). Everything else, absolute urls included, becomes "/". A malformed push payload is
 * untrusted input, so the notification never gets a url it did not earn.
 */
self.colonizerSafeUrl = function colonizerSafeUrl(url) {
  if (typeof url !== "string") return "/";
  if (!url.startsWith("/") || url.startsWith("//") || url.startsWith("/\\")) return "/";
  return url;
};

/**
 * The push payload to show. Anything malformed — empty, truncated by the push service, not JSON,
 * an array — degrades to a generic "Colonizer" notification instead of throwing: a silent drop
 * would look exactly like a missed colony. Missing fields fall back one at a time.
 *
 * A question push also carries `answer: {token, choices}` — the labels the notification itself may
 * answer with. That block is parsed just as defensively: a non-object drops entirely, a bad field
 * drops one at a time, and a question that cannot be answered from here (no usable token, no
 * choices, more than 3 — answering with a cut-down subset would mislead the colony) degrades to
 * `{token: "", choices: []}`, which the actions below turn into a plain "Open to answer".
 */
self.colonizerPushPayload = function colonizerPushPayload(raw) {
  const fallback = { title: "Colonizer", body: "A colony needs you.", url: "/", tag: "", silent: true };
  let data = null;
  try {
    data = JSON.parse(raw);
  } catch {
    return fallback;
  }
  if (!data || typeof data !== "object" || Array.isArray(data)) return fallback;
  const text = (value) => (typeof value === "string" ? value.trim() : "");
  const answerOf = (value) => {
    if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
    const token = typeof value.token === "string" && /^[0-9a-f]{8,128}$/i.test(value.token) ? value.token : "";
    const choices = (Array.isArray(value.choices) ? value.choices : [])
      .filter((choice) => typeof choice === "string" && choice.trim())
      .map((choice) => choice.trim().slice(0, 80));
    if (!token || choices.length < 1 || choices.length > 3) return { token: "", choices: [] };
    return { token, choices };
  };
  return {
    title: text(data.title) || fallback.title,
    body: text(data.body) || fallback.body,
    url: self.colonizerSafeUrl(data.url),
    tag: text(data.tag),
    // The mothership may let a question's push sound (issue #743); anything it does not say is silent.
    silent: typeof data.silent === "boolean" ? data.silent : true,
    answer: answerOf(data.answer),
  };
};

/**
 * The buttons a question notification offers, within what the platform shows (maxActions is 0 or
 * undefined on iOS, Safari and Firefox, which show none — there the tap itself opens the cockpit).
 * One button per choice, then free text when the platform can deliver it, then a plain Open, each
 * only while a slot is left. A question that cannot be answered inline gets a single "Open to
 * answer" — never a subset of its choices.
 */
self.colonizerNotificationActions = function colonizerNotificationActions(answer, maxActions, supportsReply) {
  const room = Math.max(0, Math.floor(Number(maxActions)) || 0);
  if (!answer || !Array.isArray(answer.choices)) return [];
  if (!answer.token || answer.choices.length < 1 || answer.choices.length > room) return room >= 1 ? [{ action: "open", title: "Open to answer" }] : [];
  const actions = answer.choices.map((title, index) => ({ action: `choice:${index}`, title }));
  if (supportsReply && actions.length < room) actions.push({ action: "other", type: "text", title: "Other…", placeholder: "Your answer" });
  if (actions.length < room) actions.push({ action: "open", title: "Open" });
  return actions;
};

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
