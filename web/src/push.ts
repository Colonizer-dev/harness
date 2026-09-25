// Web push for the cockpit (issue #516): the web-cockpit half of the mobile story. The browser
// subscribes itself against the mothership's VAPID key and POSTs the subscription, so a phone that
// has never opened the tab can still be told a colony needs it; the decrypted payload and the
// notification it opens are the service worker's side (public/sw.js, pure halves in sw-routes.js).
//
// Everything that touches `navigator` or `Notification` stays in the lower half and untested, like
// notifications.ts; the pure decisions — the key's base64url decoding, the device-label guess and
// the deep link a push or the url bar may carry — are pinned by push.test.ts.
import type { Api } from "./api";
import type { PushSubscriptionSummary } from "./types";

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
// Thin browser layer — everything below touches `navigator` or `Notification` and
// is deliberately left to the untestable side of the suite.
// ---------------------------------------------------------------------------

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
  return api.subscribePush({ label, endpoint: json.endpoint, keys: { p256dh: json.keys.p256dh, auth: json.keys.auth } });
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

// Re-exported so callers (Settings) reach the whole push story from one module.
export type { PushSubscriptionSummary, PushSubscribeBody } from "./types";
