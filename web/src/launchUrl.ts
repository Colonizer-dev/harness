// Launch urls: what a manifest shortcut, a share target or an iOS browser can hand the cockpit when
// it opens. Three things can ride the query string —
//
// - `?view=inbox|launch|home|…`, the manifest shortcuts' target: which cockpit view to show once,
//   over the persisted one;
// - `?share_title=/share_text=/share_url=`, what Android's share sheet puts there for the
//   manifest's share_target: somewhere in them, a GitHub issue or pull request url;
// - `?welcome=phone`, what a freshly paired phone lands on (issue #746): the cockpit offers its
//   install-and-notify sheet once;
// - `?mock=1` and `?colony=`, which other code owns (the in-browser mock and the push deep link).
//
// Everything here is pure string work so the tests can pin it without a browser; Cockpit applies it
// once at boot and then strips the params with history.replaceState, so a reload or a re-share does
// not pin the cockpit to that view or issue forever.
import { heldByFor } from "./api";
import type { Session } from "./types";
import type { CockpitView } from "./cockpit/NavRail";

/** Every view the rail can route to; the same list Cockpit routes on. */
export const COCKPIT_VIEWS: readonly CockpitView[] = [
  "overview",
  "home",
  "colony",
  "launch",
  "inbox",
  "history",
  "loops",
  "settings",
  "memory",
  "host",
  "secrets",
  "code",
  "chat",
];

/**
 * The `?view=` a launch url carries, when it names a view the cockpit actually has. Anything else —
 * a stale view name from an older build, a value from another site — falls back to null and the
 * cockpit keeps whatever it had persisted.
 */
export function viewFromUrl(url: string): CockpitView | null {
  try {
    const view = new URL(url, "https://colonizer.invalid").searchParams.get("view");
    return view && (COCKPIT_VIEWS as readonly string[]).includes(view) ? (view as CockpitView) : null;
  } catch {
    return null;
  }
}

/** A GitHub issue or pull request a share target handed over: where it lives and its number. */
export interface SharedIssue {
  repo: string;
  number: number;
  /** `pull` for a /pull/ url, `issue` for an /issues/ one. */
  kind: "issue" | "pull";
}

/**
 * The first GitHub issue or pull request url in a shared title, text or url. Android's share sheet
 * often squeezes the link into `text` ("Look at this bug https://github.com/o/r/issues/42"), and the
 * `title` can carry it too, so all three are searched — the explicit `share_url` first, the more
 * deliberate the param the sooner it wins. Only a real `…/(issues|pull)/<number>` matches: an issue
 * list (`/issues`), a query (`/issues?q=…`) or a repo root is not one issue, and whatever follows the
 * number (a slug fragment, `#issuecomment-…`, `/files`) is ignored.
 */
export function sharedIssueFromUrl(url: string): SharedIssue | null {
  const params = (() => {
    try {
      return new URL(url, "https://colonizer.invalid").searchParams;
    } catch {
      return null;
    }
  })();
  if (!params) return null;
  // Owner: letters, digits, hyphens (GitHub's own rule); repo: that plus dot and underscore. The
  // number is 1-12 digits and must not run on into a longer one, so `pull/612` is never read as 61.
  const pattern = /https:\/\/github\.com\/([\w-]+)\/([\w.-]+)\/(issues|pull)\/(\d{1,12})(?![.\d])/;
  for (const name of ["share_url", "share_text", "share_title"]) {
    const raw = params.get(name);
    const found = raw?.match(pattern);
    if (!raw || !found) continue;
    return { repo: `${found[1]}/${found[2].replace(/\.git$/i, "")}`, number: Number(found[4]), kind: found[3] === "pull" ? "pull" : "issue" };
  }
  return null;
}

/** Whether a session's `pr_url` names this repository and pull request number (`…/pull/612` never matches 61). */
export function sessionWithPull(sessions: Session[], repo: string, number: number): Session | null {
  return (
    sessions.find((s) => {
      const url = s.pr_url;
      if (typeof url !== "string") return false;
      const marker = `/${repo}/pull/${number}`;
      const at = url.indexOf(marker);
      if (at === -1) return false;
      const after = url[at + marker.length];
      return after === undefined || !/\d/.test(after);
    }) ?? null
  );
}

/**
 * The colony a shared issue or pull request belongs to, if the mothership has one. An issue is a
 * colony's own key: the mirror of the mothership's 409 guard (`heldByFor`). A pull request is not —
 * the colony holds the issue it works on — so a PR only matches a session that records that PR url,
 * and otherwise falls through to the launch form rather than risking the wrong colony.
 */
export function holdingSession(sessions: Session[], shared: SharedIssue): Session | null {
  if (shared.kind === "pull") return sessionWithPull(sessions, shared.repo, shared.number);
  return heldByFor(sessions, shared.repo, shared.number);
}

/**
 * The `?welcome=phone` a freshly paired phone lands on (issue #746): the phone is signed in and the
 * cockpit shows its install-and-notify sheet once. Anything else — no param, or a value naming a
 * welcome this build does not know — is null, and nothing is offered.
 */
export function welcomeFromUrl(url: string): "phone" | null {
  try {
    return new URL(url, "https://colonizer.invalid").searchParams.get("welcome") === "phone" ? "phone" : null;
  } catch {
    return null;
  }
}

/** The query parameters this module reads, which Cockpit strips once it has applied them. */
export const LAUNCH_PARAMS: readonly string[] = ["view", "share_title", "share_text", "share_url", "welcome"];

/** The url with the launch params dropped, the rest (`?mock=1`, `?colony=`) kept. */
export function stripLaunchParams(url: string): string {
  try {
    const parsed = new URL(url, "https://colonizer.invalid");
    for (const name of LAUNCH_PARAMS) parsed.searchParams.delete(name);
    return parsed.pathname + parsed.search + parsed.hash;
  } catch {
    return url;
  }
}

/**
 * Whether this is iOS Safari — or an iOS browser built on Safari's WebKit, which on iOS is every
 * browser. The Home-Screen sheet is about where iOS puts "Add to Home Screen" (the share sheet),
 * which since iOS 16.4 Chrome, Firefox and Edge on iOS offer too, and since 16.4 they can also hold
 * a push-subscribed Home-Screen web app; so the check is deliberately about the *device* (iPhone,
 * iPod, iPad, and iPadOS 13+ posing as Macintosh with a touch screen) and not the engine, and does
 * not exclude CriOS/FxiOS/EdgiOS. Guidance must show only on iOS, and this shows it only there — a
 * desktop Mac Safari has one touch point at most, and Safari on macOS installs through its own menu.
 */
export function isIosSafari(userAgent: string, maxTouchPoints = 0): boolean {
  if (/iPhone|iPod/.test(userAgent)) return true;
  if (/iPad/.test(userAgent)) return true;
  // iPadOS reports the desktop Macintosh agent; only the touch points give it away.
  return /Macintosh/.test(userAgent) && maxTouchPoints > 1;
}
