// Web push for the cockpit (issue #516): the web-cockpit half of the mobile story. The browser
// subscribes itself against the mothership's VAPID key and POSTs the subscription, so a phone that
// has never opened the tab can still be told a colony needs it; the decrypted payload and the
// notification it opens are the service worker's side (public/sw.js, pure halves in sw-routes.js).
//
// Everything that touches `navigator` or `Notification` stays in the lower half and untested, like
// notifications.ts; the pure decisions — the key's base64url decoding, the device-label guess, the
// deep link a push or the url bar may carry and the per-device prefs' defaults and quiet-hours
// arithmetic (issue #743) — are pinned by push.test.ts.
import { useEffect } from "react";
import { ApiError, type Api } from "./api";
import type { PushPrefs, PushSubscriptionSummary } from "./types";

// ---------------------------------------------------------------------------
// Pure decisions
// ---------------------------------------------------------------------------

/** The `?colony=<session id>` a push payload or the url bar carries, else null. */
export function colonyFromUrl(url: string): string | null {
  try {
    const id = new URL(url, "https://colonizer.invalid").searchParams.get("colony");
    return id || null;
  } catch {
    return null;
  }
}

/**
 * The VAPID `applicationServerKey` arrives base64url; `pushManager.subscribe` wants the bytes.
 * Standard base64 pads to a multiple of four and uses `+/`; base64url uses `-_` and no padding.
 */
export function urlBase64ToUint8Array(base64: string): Uint8Array<ArrayBuffer> {
  const padded = (base64 + "=".repeat((4 - (base64.length % 4)) % 4)).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(padded);
  const bytes = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
  return bytes;
}

/**
 * The label a device enrols under when the person does not type one: "iPhone · Safari",
 * "Android · Chrome", "Mac · Edge" — enough to tell the rows apart in Settings, deliberately not
 * a fingerprint. Checked device-first then browser-first, the way user agents nest.
 */
export function deviceLabel(ua: string): string {
  const device = /iPhone/.test(ua) ? "iPhone"
    : /iPad/.test(ua) ? "iPad"
    : /Android/.test(ua) ? "Android"
    : /Macintosh|Mac OS X/.test(ua) ? "Mac"
    : /Windows/.test(ua) ? "Windows"
    : /Linux/.test(ua) ? "Linux"
    : "Device";
  const browser = /Edg\//.test(ua) ? "Edge"
    : /OPR\//.test(ua) ? "Opera"
    : /Firefox\//.test(ua) ? "Firefox"
    : /Chrome\//.test(ua) ? "Chrome"
    : /Safari\//.test(ua) ? "Safari"
    : "";
  return browser ? `${device} · ${browser}` : device;
}

// ---------------------------------------------------------------------------
// Per-device prefs (issue #743): the defaults, quiet-hours minute arithmetic and
// the repo filter's entry shape — all pure, all pinned by push.test.ts.
// ---------------------------------------------------------------------------

/** The prefs a freshly enrolled device has: the loud events on, the two quiet kinds off. */
export function defaultPushPrefs(): PushPrefs {
  return {
    events: { question: true, pull_request: true, needs_rebase: true, failed: true, attention: true, provider_degraded: false, digest: false },
    question_sound: true,
    answer_actions: true,
    badge: true,
    scope: [],
    quiet: null,
    questions_break_quiet: false,
    tz: null,
    utc_offset: 0,
  };
}

/** A row's prefs with every gap filled from the defaults — a mothership may omit keys it adds later. */
export function mergePushPrefs(stored?: Partial<PushPrefs> | null): PushPrefs {
  const base = defaultPushPrefs();
  if (!stored) return base;
  return {
    events: { ...base.events, ...stored.events },
    question_sound: stored.question_sound ?? base.question_sound,
    answer_actions: stored.answer_actions ?? base.answer_actions,
    badge: stored.badge ?? base.badge,
    scope: stored.scope ?? base.scope,
    quiet: stored.quiet ?? base.quiet,
    questions_break_quiet: stored.questions_break_quiet ?? base.questions_break_quiet,
    tz: stored.tz ?? base.tz,
    utc_offset: stored.utc_offset ?? base.utc_offset,
  };
}

/** Minutes since local midnight as "HH:MM", the `<input type="time">` shape; out-of-range clamps. */
export function minutesToTime(minutes: number): string {
  const m = Math.min(Math.max(Math.round(minutes), 0), 1439);
  return `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
}

/** "HH:MM" back to minutes; null for anything else, an emptied time input included. */
export function timeToMinutes(time: string): number | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(time.trim());
  if (!m) return null;
  const [hours, minutes] = [Number(m[1]), Number(m[2])];
  return hours < 24 && minutes < 60 ? hours * 60 + minutes : null;
}

/**
 * One repo filter entry, "org" or "org/repo": each part 1–100 chars of [-._a-zA-Z0-9] and not "." or
 * "..", mirroring the mothership's util::valid_repo — the same shape the mock's rows already keep.
 */
export function validScopeEntry(entry: string): boolean {
  const parts = entry.split("/");
  if (parts.length > 2) return false;
  return parts.every((p) => !!p && p.length <= 100 && p !== "." && p !== ".." && /^[._a-zA-Z0-9-]+$/.test(p));
}

/** This device's IANA timezone and its offset east of UTC — `getTimezoneOffset()` counts west, so negated. */
export function deviceTz(): { tz: string; utc_offset: number } {
  return { tz: Intl.DateTimeFormat().resolvedOptions().timeZone, utc_offset: -new Date().getTimezoneOffset() };
}

/** A tab can show a notification itself only when it is visible and has focus; otherwise the push goes out. */
export function tabFocused(visibilityState: string, hasFocus: boolean): boolean {
  return visibilityState === "visible" && hasFocus;
}

// ---------------------------------------------------------------------------
// Thin browser layer — everything below touches `navigator` or `Notification` and
// is deliberately left to the untestable side of the suite.
// ---------------------------------------------------------------------------

/**
 * Whether the presence reporter below still has an enrolled endpoint to report for. A 404 from
 * POST /api/push/presence — this endpoint was revoked — clears it until the next subscribe, so a
 * device that left the push stops POSTing on every colony change. Module-level, not state:
 * Settings' subscribe happens in another component than App's reporter.
 */
let presenceArmed = true;

/** The live reporters, nudged by subscribeThisDevice so a re-arm resumes heartbeats at once. */
const presenceListeners = new Set<() => void>();

/**
 * Whether this browser can join the push at all: a service worker to receive, PushManager to
 * subscribe and Notification to ask permission. iOS Safari only grows all three from 16.4, and
 * only for a Home-Screen web app — Settings says so rather than a bare disabled button.
 */
export function pushSupported(): boolean {
  return (
    typeof window !== "undefined" &&
    "serviceWorker" in navigator &&
    typeof PushManager !== "undefined" &&
    typeof Notification !== "undefined"
  );
}

/**
 * Asks, subscribes and enrols, in that order — the permission prompt is legal only inside the
 * click that got us here, so the caller must not let anything await before it. The subscription's
 * own copy is kept if the mothership refuses the enrolment, so Settings can retry the POST without
 * re-prompting; the API error carries the reason.
 */
export async function subscribeThisDevice(api: Api, label: string): Promise<PushSubscriptionSummary> {
  if (!pushSupported()) throw new Error("this browser does not offer web push");
  const permission = await Notification.requestPermission();
  if (permission !== "granted") throw new Error("notification permission was not granted");
  const { public_key } = await api.pushKey();
  const ready = await navigator.serviceWorker.ready;
  const subscription = await ready.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: urlBase64ToUint8Array(public_key),
  });
  const json = subscription.toJSON() as { endpoint?: string; keys?: { p256dh?: string; auth?: string } };
  if (!json.endpoint || !json.keys?.p256dh || !json.keys?.auth) {
    await subscription.unsubscribe().catch(() => undefined);
    throw new Error("the browser returned an incomplete subscription");
  }
  const summary = await api.subscribePush({ label, endpoint: json.endpoint, keys: { p256dh: json.keys.p256dh, auth: json.keys.auth } });
  presenceArmed = true;
  // The enrolled endpoint changed: heartbeat again now, not on the next focus event.
  for (const listener of presenceListeners) listener();
  return summary;
}

/**
 * Revokes one row from Settings: the mothership record first, then the browser's own registration
 * when it is the one being revoked (another device's row must not unsubscribe this browser).
 */
export async function unsubscribeThisDevice(api: Api, id: string, endpointHost?: string): Promise<void> {
  await api.deletePushSubscription(id);
  if (!endpointHost) return;
  try {
    const subscription = await (await navigator.serviceWorker.ready).pushManager.getSubscription();
    // `.hostname`, not `.host`: the list's endpoint_host carries no port, and neither must the match.
    if (subscription && new URL(subscription.endpoint).hostname === endpointHost) await subscription.unsubscribe();
  } catch {
    /* the mothership record is gone either way; the stale registration expires on its next push */
  }
}

/** A device's own row in the list, matched on the endpoint the browser remembers, if any. */
export async function thisDeviceSubscriptions(api: Api): Promise<PushSubscriptionSummary[]> {
  if (!pushSupported()) return [];
  try {
    const subscription = await (await navigator.serviceWorker.ready).pushManager.getSubscription();
    if (!subscription) return [];
    const host = new URL(subscription.endpoint).host;
    return (await api.pushSubscriptions()).filter((row) => row.endpoint_host === host);
  } catch {
    return [];
  }
}

/**
 * The focused-tab report (issue #743): while this device is enrolled, tells the mothership which
 * colony this tab has open and whether it could show a notification itself, so a push for a colony
 * someone is already looking at can be held back. Reports at once when focus, visibility or the
 * shown colony changes, and every 30 s while focused — the mothership forgets a device it has not
 * heard from. Quiet where push cannot work and in mock mode; after a 404 said this endpoint is no
 * longer subscribed it idles — the listeners stay on, and subscribeThisDevice's re-arm nudges the
 * next report out at once.
 */
export function usePushPresence(api: Api, colony: string | null): void {
  useEffect(() => {
    if (!pushSupported() || api.mock) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const focused = () => tabFocused(document.visibilityState, document.hasFocus());

    const report = () => {
      // Disarmed (the endpoint was revoked): nothing to say until the next subscribe nudges us.
      if (stopped || !presenceArmed) return;
      clearTimeout(timer);
      void (async () => {
        try {
          const subscription = await (await navigator.serviceWorker.ready).pushManager.getSubscription();
          // Nothing enrolled here yet: say nothing, and let the next focus event or subscribe try again.
          if (!subscription) return;
          await api.pushPresence({ endpoint: subscription.endpoint, colony, focused: focused(), ...deviceTz() });
        } catch (error) {
          if (error instanceof ApiError && error.status === 404) presenceArmed = false;
          // Anything else — offline, the mothership mid-restart — just misses this one report.
        }
        if (!stopped && presenceArmed && focused()) timer = setTimeout(report, 30_000);
      })();
    };

    report();
    presenceListeners.add(report);
    document.addEventListener("visibilitychange", report);
    window.addEventListener("focus", report);
    window.addEventListener("blur", report);
    return () => {
      stopped = true;
      presenceListeners.delete(report);
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", report);
      window.removeEventListener("focus", report);
      window.removeEventListener("blur", report);
    };
  }, [api, colony]);
}

// Re-exported so callers (Settings) reach the whole push story from one module.
export type { PushSubscriptionSummary, PushSubscribeBody } from "./types";
