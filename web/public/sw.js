// The cockpit's service worker: just enough to make the cockpit an installable app and quick to
// reopen. It precaches each build's hashed /assets into a cache named for that build and answers
// asset requests from whichever build's cache has them, caches the proxied avatars, answers a few
// read-only views from their last answer while it revalidates them, shows an offline page when the
// mothership is not running, shows the mothership's web pushes and opens the colony they name when
// tapped, and never touches writes, sign-in or any other /api call (see sw-routes.js).
importScripts("/sw-routes.js");

// Bumped whenever the routing or the caches change: the activate step drops every other cache.
const VERSION = "v4";
const SHELL = `colonizer-shell-${VERSION}`;
const IMAGES = `colonizer-img-${VERSION}`;
const API = `colonizer-api-${VERSION}`;
const LIMITS = { [IMAGES]: 300, [API]: 200 };

// One cache per build, named for the build's hash. Install precaches this build's /assets into it;
// the previous build's cache is kept one build longer, because a tab that has not reloaded yet
// still asks for the old build's chunks and the mothership serves only the current build — after a
// swap it 404s the old ones (the lazy-chunk 404 that used to break open tabs).
const BUILD_PREFIX = "colonizer-build-";
const buildCache = (hash) => BUILD_PREFIX + hash;
// The line the build rewrites (vite.config.ts, src/swBuild.ts): "dev" and no assets only outside a
// real build, where the worker is never registered.
const BUILD = { hash: "dev", assets: [] };
// The record of which build was current before this one, kept as a cache-only entry in the shell
// cache: activate reads it to know which older build cache to keep. Never fetched.
const HISTORY = "/__colonizer-build";

/** The build's assets, fail-soft: one missing or failing file must not fail the whole install — the
 *  asset route below falls back to the network for whatever did not land. Only ok answers are kept. */
async function precacheBuild(cache, urls) {
  await Promise.allSettled(
    urls.map(async (url) => {
      const response = await fetch(url);
      if (response.ok) await cache.put(url, response);
    }),
  );
}

self.addEventListener("install", (event) => {
  // No skipWaiting: an update waits until a tab asks for it (the Reload of the update prompt,
  // installApp.ts) or every tab of the old build has closed, so no open tab is ever swapped. A
  // first install — nothing controlled yet — activates at once without asking.
  event.waitUntil(
    (async () => {
      // The shell files are few and load-bearing (the offline page), so they stay all-or-nothing;
      // the build's ~hundred hashed chunks are not.
      await (await caches.open(SHELL)).addAll(["/offline.html", "/icons/mark.svg"]);
      await precacheBuild(await caches.open(buildCache(BUILD.hash)), BUILD.assets);
    })(),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      const shell = await caches.open(SHELL);
      // The build before this one stays cached, for the tabs still running it (see buildCache).
      // With no record — the first run after a VERSION bump — every build cache is kept: there are
      // at most two, and this build's next activate prunes them.
      const known = await shell.match(HISTORY);
      const keepBuilds = known ? new Set([buildCache(BUILD.hash), buildCache((await known.text()).trim())]) : null;
      const drop = (name) => (name.startsWith(BUILD_PREFIX) ? keepBuilds !== null && !keepBuilds.has(name) : true);
      const keys = await caches.keys();
      await Promise.all(keys.filter((k) => ![SHELL, IMAGES, API].includes(k) && drop(k)).map((k) => caches.delete(k)));
      await shell.put(HISTORY, new Response(BUILD.hash));
      // Take over every tab: their asset requests now reach this worker, which answers from the
      // previous build's cache too (asset below), so the takeover is not itself the break.
      await self.clients.claim();
    })(),
  );
});

// The update prompt's Reload: the waiting worker takes over at once, activate's clients.claim
// brings the asking tab under it, and that tab reloads itself on the controllerchange.
self.addEventListener("message", (event) => {
  if (event.data && event.data.type === "colonizer:skip-waiting") event.waitUntil(self.skipWaiting());
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

/** An asset from whichever build cache has it — this build's first, then the older ones a tab that
 *  has not reloaded yet may still be running. Not in any of them: from the network, cached into
 *  this build's cache on the way in. */
async function asset(request) {
  const current = buildCache(BUILD.hash);
  const names = (await caches.keys()).filter((k) => k.startsWith(BUILD_PREFIX));
  names.sort((a, b) => Number(b === current) - Number(a === current));
  for (const name of names) {
    const hit = await (await caches.open(name)).match(request);
    if (hit) return hit;
  }
  const response = await fetch(request);
  if (response.ok) await (await caches.open(current)).put(request, response.clone());
  return response;
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
    event.respondWith(asset(request));
  } else if (route === "image") {
    event.respondWith(cacheFirst(IMAGES, request));
  } else if (route === "swr") {
    event.respondWith(staleWhileRevalidate(event, request));
  } else if (route === "page") {
    event.respondWith(fetch(request).catch(() => caches.match("/offline.html")));
  }
  // "network": not handled, so the browser does exactly what it would without a worker.
});

// --- Web push (issue #516, #744) ---------------------------------------------------------------

/** Closes every notification carrying one tag, where the browser offers the filtered lookup. */
async function closeTagged(tag) {
  if (!self.registration.getNotifications) return;
  for (const notification of await self.registration.getNotifications({ tag })) notification.close();
}

/** The app badge to the attention count the mothership pushes, cleared at zero. Absent on a browser
 *  without the Badging API, and any refusal is swallowed — the notifications are the record. */
async function applyBadge(count) {
  if (count == null) return;
  try {
    if (count > 0) {
      if (self.navigator.setAppBadge) await self.navigator.setAppBadge(count);
    } else if (self.navigator.clearAppBadge) {
      await self.navigator.clearAppBadge();
    }
  } catch {
    /* no badge here; the colony notifications still show */
  }
}

/** The one summary standing in for several colony notifications, replaced in place and closed again
 *  once a single colony notification says it all. */
async function applySummary(count) {
  if (count == null) return;
  try {
    const summary = self.colonizerSummary(count);
    if (summary) {
      await self.registration.showNotification(summary.title, {
        tag: summary.tag,
        data: { url: summary.url },
        icon: "/icons/icon-192.png",
        badge: "/icons/mark.svg",
        silent: true,
      });
    } else {
      await closeTagged("summary");
    }
  } catch {
    /* the summary is decoration; the colony notifications carry the news */
  }
}

/** The silent "seen elsewhere" push (issue #744): drop that colony's notification here too, then
 *  follow the badge and the summary down. */
async function resolveColony(colony, badge) {
  if (colony) await closeTagged(`colony-${colony}`);
  await applyBadge(badge);
  await applySummary(badge);
}

/** Shows one push, whatever arrived: a payload the mothership malformed still shows generically. */
async function showPush(raw) {
  const payload = self.colonizerPushPayload(raw);
  if (payload.resolved) return resolveColony(payload.colony, payload.badge);
  // A question push answers from the notification itself (issue #742): one button per choice where
  // the platform shows buttons at all — Notification.maxActions is 0 or undefined on iOS, Safari
  // and Firefox, so there the tap opens the cockpit — and free text only where the platform can
  // deliver it, which NotificationEvent.reply is the one honest signal for.
  const maxActions = (self.Notification && self.Notification.maxActions) || 0;
  const supportsReply = typeof NotificationEvent === "function" && "reply" in NotificationEvent.prototype;
  await self.registration.showNotification(payload.title, {
    body: payload.body,
    tag: payload.tag || undefined,
    actions: self.colonizerNotificationActions(payload.answer, maxActions, supportsReply),
    data: { url: payload.url, colony: payload.colony, answer: payload.answer || undefined },
    icon: "/icons/icon-192.png",
    badge: "/icons/mark.svg",
    // Silent unless the mothership said otherwise — a question may sound (issue #743); the in-app
    // channel stays the only other thing that beeps (notifications.ts).
    silent: payload.silent,
  });
  await applyBadge(payload.badge);
  await applySummary(payload.badge);
}

self.addEventListener("push", (event) => {
  let raw = "";
  try {
    raw = event.data ? event.data.text() : "";
  } catch {
    raw = "";
  }
  event.waitUntil(showPush(raw).catch(() => undefined));
});

/** The tapped notification opens the cockpit on the payload's url — already-running if there is one. */
async function openFromNotification(url) {
  const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
  const target = windows.find((client) => {
    try {
      return new URL(client.url).origin === self.location.origin;
    } catch {
      return false;
    }
  });
  if (target) {
    await target.focus();
    target.postMessage({ type: "colonizer:open", url });
    return;
  }
  await self.clients.openWindow(url);
}

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = self.colonizerSafeUrl(event.notification.data && event.notification.data.url);
  event.waitUntil(
    (async () => {
      // A question's buttons answer straight from the notification: the endpoint authenticates by
      // the one-time token in the notification's data alone, so no stored credentials leave the
      // worker. Anything the button cannot do — no token, a stale or used one, a network miss —
      // opens the colony, and every step below, clients.openWindow included, stays inside this
      // click's waitUntil.
      const answer = event.notification.data && event.notification.data.answer;
      const action = event.action || "";
      const raw = action.startsWith("choice:") ? action.slice("choice:".length) : "";
      const choice = /^\d+$/.test(raw) ? (answer && answer.choices ? answer.choices[Number(raw)] : "") : "";
      const other = action === "other" && typeof event.reply === "string" ? event.reply.trim() : "";
      if (answer && answer.token && (choice || other)) {
        try {
          const response = await fetch("/api/push/answer", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(choice ? { token: answer.token, choice } : { token: answer.token, other }),
          });
          if (!response.ok) throw new Error(`status ${response.status}`);
          await self.registration.showNotification(`Answered: ${(choice || other).slice(0, 80)}`, {
            tag: event.notification.tag || undefined,
            data: { url },
            icon: "/icons/icon-192.png",
            badge: "/icons/mark.svg",
            silent: true,
          });
          return;
        } catch {
          // The cockpit is the reliable way to answer: fall through and open it.
        }
      }
      await openFromNotification(url).catch(() => undefined);
    })(),
  );
});
