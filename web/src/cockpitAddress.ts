// Where this cockpit can be reached, in the plain words a bookmark wants (issue #867): on this
// computer, on your network or tailnet, anywhere. Everything here is pure string work over an
// address, the mothership's origin list and the remote-access view, so the tests can pin the
// ordering and — the point of the feature — that what is shown, copied and drawn into a QR code is
// a stable address with no query string, no hash and nothing token-like in it. The sign-in link in
// the terminal (?token=…, one use) and the pairing code (#…) must never end up in a bookmark.
import { runningStandalone } from "./installApp";
import { isBraveBrowser, isIosDevice, isIosSafari } from "./launchUrl";
import { chosenOrigin } from "./phoneOrigins";
import type { PhoneOrigin, RemoteStatus } from "./types";
import { store, stored } from "./components/ui";

/** One address the card offers, already reduced to a bookmarkable url. */
export interface CockpitAddress {
  kind: "local" | "network" | "anywhere";
  label: string;
  url: string;
  /** A caveat to show under the address — a plain-http network origin the phone cannot install from. */
  note?: string;
}

/**
 * The url reduced to scheme + host + port + base path, with no query string and no hash — what the
 * card shows, copies and encodes. A sign-in token (`?token=…`) or a pairing code (`#…`) can never
 * ride along, so a bookmark made here stays a plain address that needs a real sign-in like any other.
 */
export function bookmarkable(url: string): string {
  try {
    const parsed = new URL(url);
    return `${parsed.protocol}//${parsed.host}${parsed.pathname.replace(/\/+$/, "")}`;
  } catch {
    // Not a parseable absolute url: still strip anything after the first ? or # rather than risk it.
    return url.split(/[?#]/)[0];
  }
}

/** Whether a url points at this computer alone (its loopback), which no other device can open. */
export function isLoopback(url: string): boolean {
  try {
    const { protocol, hostname } = new URL(url);
    if (protocol !== "http:" && protocol !== "https:") return false;
    const host = hostname.replace(/^\[|\]$/g, "").toLowerCase();
    return host === "localhost" || host === "127.0.0.1" || host === "::1";
  } catch {
    return false;
  }
}

/** This page's own address, reduced to a bookmark; "" when there is no window (a static render). */
export function currentAddress(): string {
  try {
    return bookmarkable(window.location.href);
  } catch {
    return "";
  }
}

export interface CockpitAddressesInput {
  /** Where this page is open, `window.location.href`. */
  here: string;
  /**
   * The mothership's preference-ordered origins, when something can supply them. GET /api/phone
   * answers with them (bare origins, no invite), so the card usually has them; with none to hand
   * the network address falls back to `here` — see cockpitAddresses.
   */
  origins?: readonly PhoneOrigin[];
  remote?: Pick<RemoteStatus, "enabled" | "host"> | null;
}

/**
 * The addresses to offer, best-known first: this computer, your network/tailnet, anywhere. The
 * network pick is the first `reachable`, non-relay origin (the relay link is the "anywhere" one):
 * an unreachable origin is not an address another device can actually open, so it is left out and
 * networkGap explains why rather than offering a dead link. With no reachable origin to hand, an
 * address this page is already open on is offered as the network one — you reached this machine
 * through it, and the rest of its network can too — except when it is the loopback (only this
 * computer) or the remote link (listed once, as anywhere). Nothing is ever listed twice.
 */
export function cockpitAddresses({ here, origins = [], remote = null }: CockpitAddressesInput): CockpitAddress[] {
  const local = isLoopback(here) ? bookmarkable(here) : null;
  const anywhere = remote?.enabled && remote.host ? bookmarkable(`https://${remote.host}`) : null;

  const origin = chosenOrigin(origins.filter((o) => o.kind !== "relay" && o.reachable));
  const fromHere = !isLoopback(here) && here ? bookmarkable(here) : null;
  const network =
    origin && origin.url
      ? {
          label: origin.kind === "tailnet" ? "On your tailnet" : "On your network",
          url: bookmarkable(origin.url),
          // A reachable but plain-http origin still pairs, but the phone cannot install the app or
          // get notifications over it; the mothership's own note says so, with our wording as backup.
          note: origin.secure ? undefined : (origin.note ?? PLAIN_HTTP),
        }
      : fromHere && fromHere !== anywhere
        ? { label: "On your network", url: fromHere }
        : null;

  const list: CockpitAddress[] = [];
  if (local) list.push({ kind: "local", label: "On this computer", url: local });
  if (network && network.url !== local) list.push({ kind: "network", ...network });
  if (anywhere && !list.some((a) => a.url === anywhere)) list.push({ kind: "anywhere", label: "Anywhere", url: anywhere });
  return list;
}

/** What a plain-http network address warns, when the mothership sent no note of its own. */
const PLAIN_HTTP =
  "Plain http: it only works on this network, and a phone cannot install the app or get notifications over it. Prefer the relay link.";

/**
 * The line to show when the list holds no address another device could open (only the loopback):
 * this cockpit is not reachable from outside. Any note the mothership attached to the origins says
 * what to change (usually the bind), and the remote-access link is the way in; null once there is a
 * network or an anywhere address.
 */
export function networkGap(addresses: readonly CockpitAddress[], origins: readonly PhoneOrigin[] = []): string | null {
  if (addresses.some((a) => a.kind !== "local")) return null;
  const note = origins.map((o) => o.note).find((n): n is string => Boolean(n));
  return `Not reachable from other devices — ${note ?? "the mothership only listens on this computer"}. Turn on Remote access (Settings → Remote access) for an https link that works from anywhere.`;
}

// ---------------------------------------------------------------------------
// The bookmark prompt, remembered per device
// ---------------------------------------------------------------------------

/** How this device answered the bookmark prompt, or null while it is still worth offering. */
export type BookmarkState = "dismissed" | "installed";

const BOOKMARK_KEY = "colonizer.bookmarkPrompt";

type ReadStore = (key: string) => string | null;
type WriteStore = (key: string, value: string | null) => void;

/** The remembered answer; anything else (missing, foreign) reads as "not answered". */
export function bookmarkState(read: ReadStore = stored): BookmarkState | null {
  const value = read(BOOKMARK_KEY);
  return value === "dismissed" || value === "installed" ? value : null;
}

export function dismissBookmark(write: WriteStore = store): void {
  write(BOOKMARK_KEY, "dismissed");
}

export function markBookmarkInstalled(write: WriteStore = store): void {
  write(BOOKMARK_KEY, "installed");
}

/** Forgets the answer, so the card's "Add to this device" can offer the prompt again. */
export function clearBookmark(write: WriteStore = store): void {
  write(BOOKMARK_KEY, null);
}

/** Whether the prompt is still worth showing: not dismissed, not installed, and not already the app. */
export function shouldOfferBookmark(options: { state?: BookmarkState | null; standalone?: boolean } = {}): boolean {
  const state = options.state === undefined ? bookmarkState() : options.state;
  return state === null && !(options.standalone ?? runningStandalone());
}

/** The browser's own bookmark shortcut, as plain text for the prompt: ⌘D on macOS, Ctrl+D elsewhere. */
export function bookmarkShortcut(userAgent: string = browserUserAgent()): string {
  return /Macintosh|Mac OS X/.test(userAgent) ? "⌘D" : "Ctrl+D";
}

// ---------------------------------------------------------------------------
// Which install guidance this device should get
// ---------------------------------------------------------------------------

export type InstallPlatform = "installed" | "ios-safari" | "ios-other" | "android-chrome" | "android-other" | "desktop";

/** Android browsers that are not Chrome: install is still offered, but by a menu, not a prompt. */
const ANDROID_NOT_CHROME = /EdgA|OPR|SamsungBrowser|FxiOS|CriOS|DuckDuckGo|GSA/;

/**
 * Which install story to tell: already the app, iOS Safari's share sheet, another iOS browser, an
 * Android Chrome (the one with a native prompt), another Android browser, or a desktop. iOS is
 * detected through launchUrl's `isIosDevice`, so an iPad reporting itself as a Macintosh with a
 * touch screen is still iOS, and Safari through `isIosSafari`, so Brave (Safari's user agent, plus
 * `navigator.brave`), Chrome, Firefox, Edge, Opera and in-app webviews all get the Safari hand-off.
 */
export function installPlatform(
  userAgent: string = browserUserAgent(),
  standalone: boolean = runningStandalone(),
  maxTouchPoints: number = typeof navigator === "undefined" ? 0 : (navigator.maxTouchPoints ?? 0),
  brave: boolean = isBraveBrowser(),
): InstallPlatform {
  if (standalone) return "installed";
  if (isIosDevice(userAgent, maxTouchPoints)) {
    return isIosSafari(userAgent, maxTouchPoints, brave) ? "ios-safari" : "ios-other";
  }
  if (/Android/.test(userAgent)) {
    return /Chrome\//.test(userAgent) && !ANDROID_NOT_CHROME.test(userAgent) ? "android-chrome" : "android-other";
  }
  return "desktop";
}

function browserUserAgent(): string {
  return typeof navigator === "undefined" ? "" : navigator.userAgent;
}
