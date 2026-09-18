// What tells a person a colony needs them, on three channels of descending intrusiveness: the tab
// itself (title count, favicon dot, a strip in the sidebar), a short sound, and browser notifications
// for when the tab is behind another window or on another desk. The decisions — what "needs you"
// means, what changed since the last look, what a notification may say, what the switches were —
// live here as pure functions so the tests can pin them in the plain node environment; everything
// that touches `window` or `document` is a thin wrapper in the lower half and stays untested.
//
// Two rules shape the file. A notification is never the only record: the sidebar stays the source of
// truth, so a blocked or missed notification must never hide a waiting colony. And they are
// edge-triggered with no backlog: the first session list only seeds the snapshot, so a colony that
// was already waiting when the page loaded is the sidebar's job, not an interruption.
import type { AttentionReason, Session, SessionStatus } from "./types";
import { orgOf, sameOrg } from "./components/ui";

// ---------------------------------------------------------------------------
// Pure decisions
// ---------------------------------------------------------------------------

/**
 * The one definition of "needs a person": flagged by the watchdog or autopilot, or sat on an open
 * question. The sidebar's rank-0 group, the tab-title count, the favicon dot and the strip all read
 * this, so none of them can disagree about who is waiting.
 */
export function needsYou(session: Session): boolean {
  return Boolean(session.attention) || session.status === "waiting_for_answer";
}

/** `(2) Colonizer` while colonies wait; at zero, exactly the title index.html ships with. */
export function tabTitle(count: number): string {
  return count > 0 ? `(${count}) Colonizer` : "Colonizer";
}

// The favicon lives in index.html as an inline SVG data URL — the outpost hexagon in the brand
// accent on the term-background navy — and there is no web/public/ to put files in, so both
// variants are built here. Keeping the quiet variant byte-identical to the shipped href is what
// makes "everything off" restore today's tab exactly.
const FAVICON_ACCENT = "%23FF6B35";
const FAVICON_BG = "%230b0e14";

/** The favicon data URL; `waiting` adds a dot in the corner, over a dark halo so it survives overlapping the hexagon's stroke. */
export function faviconHref(waiting: boolean): string {
  const dot = waiting ? `<circle cx='25' cy='7' r='5.5' fill='${FAVICON_BG}'/><circle cx='25' cy='7' r='3.5' fill='${FAVICON_ACCENT}'/>` : "";
  return (
    `data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'>` +
    `<rect width='32' height='32' rx='8' fill='${FAVICON_BG}'/>` +
    `<path d='M16 5.5 25 10.75v10.5L16 26.5 7 21.25v-10.5z' fill='none' stroke='${FAVICON_ACCENT}' stroke-width='2.6' stroke-linejoin='round'/>` +
    `<circle cx='16' cy='16' r='3' fill='${FAVICON_ACCENT}'/>${dot}</svg>`
  );
}

/** The strip's heading: "1 colony needs you", "2 colonies need you". */
export function needsYouLabel(count: number): string {
  return count === 1 ? "1 colony needs you" : `${count} colonies need you`;
}

/** `acme/webshop #42`; a colony launched without an issue is just the repository. */
export function colonyLabel(repo: string, issue: number | null): string {
  return issue != null ? `${repo} #${issue}` : repo;
}

/**
 * The org filter to use when something outside the colony list — the strip, which counts every
 * waiting colony whatever the filter shows — opens a colony. The filter survives when the list
 * already shows the target under it, and is cleared only when it would hide the target: landing
 * on a colony the list beneath does not contain is a dead end, and a missed notification must
 * never leave one stranded. App applies the result through its org selection, so the stored
 * filter stays consistent. A colony whose `org` an older mothership omits resolves through
 * `orgOf`'s repository-owner fallback — the same value the list filters on, so the answer here
 * always matches what the list actually shows.
 */
export function orgFilterForTarget(selectedOrg: string | null, target: Pick<Session, "org" | "repo">): string | null {
  if (!selectedOrg) return selectedOrg;
  return sameOrg(orgOf(target), selectedOrg) ? selectedOrg : null;
}

/** What one colony looked like last time the list was seen; just enough to tell "changed" from "unchanged". */
export interface SessionSnapshot {
  status: SessionStatus;
  attention: AttentionReason | null;
}

/**
 * The last-seen state per colony. Rebuilt wholesale on every pass, which is also how entries for
 * colonies that disappeared get pruned: a deleted colony must not survive as a stale comparison.
 */
export function snapshotOf(sessions: Session[]): Record<string, SessionSnapshot> {
  const snapshot: Record<string, SessionSnapshot> = {};
  for (const session of sessions) {
    snapshot[session.id] = { status: session.status, attention: session.attention?.reason ?? null };
  }
  return snapshot;
}

export type NotificationEventKind = "question" | "attention" | "failed" | "pull_request";

/** One switch per event kind; each gates its own kind and nothing else. */
export interface EventSwitches {
  question: boolean;
  attention: boolean;
  failed: boolean;
  pull_request: boolean;
}

export interface ColonyEvent {
  /** The colony's session id, so a notification click can open its chat. */
  id: string;
  repo: string;
  issue: number | null;
  kind: NotificationEventKind;
  /** For `attention`, the reason that just appeared — only ever "stalled" or "nudges_exhausted". */
  reason: "stalled" | "nudges_exhausted" | null;
}

/**
 * The transitions worth interrupting someone for, given what each colony looked like before. Every
 * kind is a strict transition from the snapshot, not a state: a colony already `waiting_for_answer`
 * on first sight stays quiet, and it must leave and re-enter a state before the same event repeats,
 * so one event fires at most once per (colony, kind) per change. There are exactly four kinds on
 * purpose — anything more and the notifications become noise nobody reads. `waiting_for_answer`
 * attention is excluded because the question event already covers it, and `autopilot_held` because
 * nobody has to act on it.
 */
export function diffEvents(previous: Record<string, SessionSnapshot>, next: Session[], enabled: EventSwitches): ColonyEvent[] {
  const events: ColonyEvent[] = [];
  for (const session of next) {
    const before = previous[session.id];
    if (!before) continue; // first sight of this colony while the page is open: seeding, not news
    const reason = session.attention?.reason ?? null;
    const push = (kind: NotificationEventKind, withReason: "stalled" | "nudges_exhausted" | null = null) =>
      events.push({ id: session.id, repo: session.repo, issue: session.issue, kind, reason: withReason });
    if (enabled.question && session.status === "waiting_for_answer" && before.status !== "waiting_for_answer") push("question");
    if (enabled.attention && (reason === "stalled" || reason === "nudges_exhausted") && reason !== before.attention) push("attention", reason);
    if (enabled.failed && session.status === "failed" && before.status !== "failed") push("failed");
    if (enabled.pull_request && session.status === "pr_opened" && before.status !== "pr_opened") push("pull_request");
  }
  return events;
}

/**
 * What a notification says: `acme/webshop #42 needs an answer`. Short and dull on purpose — these
 * land on screens other people can see. `ColonyEvent` carries no issue title, question text or
 * error, so none can leak in; the repository and issue number are the address, not the content.
 */
export function eventText(event: ColonyEvent): string {
  const subject = colonyLabel(event.repo, event.issue);
  switch (event.kind) {
    case "question":
      return `${subject} needs an answer`;
    case "attention":
      return event.reason === "nudges_exhausted" ? `${subject} is out of nudges` : `${subject} has stalled`;
    case "failed":
      return `${subject} failed`;
    case "pull_request":
      return `${subject} opened a pull request`;
  }
}

export interface NotificationPrefs {
  /** The in-tab layer: title count, favicon dot and the sidebar strip. On by default — it is the one channel that cannot miss, and the quietest. */
  inTab: boolean;
  /** A short blip when a question opens. Off by default: a sound is a surprise to everyone in the room. */
  sound: boolean;
  /** Browser notifications while the tab is not in front. Off until the browser's own prompt is answered from a click. */
  browser: boolean;
  /** Which of the four events may use the sound and the browser notification. The in-tab layer ignores these: it always shows everything that needs you. */
  events: EventSwitches;
}

export const NOTIFICATIONS_KEY = "colonizer.notifications";

/** The defaults are decided: the in-tab layer on, everything that makes noise or leaves the tab off. */
export function defaultNotificationPrefs(): NotificationPrefs {
  return { inTab: true, sound: false, browser: false, events: { question: true, attention: true, failed: true, pull_request: true } };
}

const boolOr = (value: unknown, fallback: boolean): boolean => (typeof value === "boolean" ? value : fallback);

/**
 * Parses the stored blob, filling every gap and rejecting every wrong-typed field with its default,
 * so a blob from an older or newer build — or plain garbage — degrades to the defaults instead of
 * breaking the settings pane or the notifier.
 */
export function parseNotificationPrefs(raw: string | null): NotificationPrefs {
  const prefs = defaultNotificationPrefs();
  if (!raw) return prefs;
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return prefs;
  }
  if (!data || typeof data !== "object" || Array.isArray(data)) return prefs;
  const blob = data as Record<string, unknown>;
  prefs.inTab = boolOr(blob.inTab, prefs.inTab);
  prefs.sound = boolOr(blob.sound, prefs.sound);
  prefs.browser = boolOr(blob.browser, prefs.browser);
  if (blob.events && typeof blob.events === "object" && !Array.isArray(blob.events)) {
    const events = blob.events as Record<string, unknown>;
    prefs.events.question = boolOr(events.question, prefs.events.question);
    prefs.events.attention = boolOr(events.attention, prefs.events.attention);
    prefs.events.failed = boolOr(events.failed, prefs.events.failed);
    prefs.events.pull_request = boolOr(events.pull_request, prefs.events.pull_request);
  }
  return prefs;
}

/** One localStorage key holding one JSON blob, under the house `colonizer.*` naming. */
export function serializeNotificationPrefs(prefs: NotificationPrefs): string {
  return JSON.stringify({ inTab: prefs.inTab, sound: prefs.sound, browser: prefs.browser, events: { ...prefs.events } });
}

// ---------------------------------------------------------------------------
// Thin browser layer — everything below touches `window` or `document` and is
// deliberately left to the untestable side of the suite.
// ---------------------------------------------------------------------------

/** Applies the title count; with the in-tab layer off App passes 0, which is today's static title. */
export function applyTabTitle(count: number): void {
  document.title = tabTitle(count);
}

/** Swaps the existing icon link's href; the link element itself stays in index.html so the page has a favicon before any JS runs. */
export function applyFavicon(waiting: boolean): void {
  const link = document.querySelector<HTMLLinkElement>('link[rel="icon"]');
  if (!link) return;
  const href = faviconHref(waiting);
  if (link.getAttribute("href") === href) return;
  link.href = href;
}

/** A two-note WebAudio blip — no audio asset, no dependency. Any failure (no device, blocked context) is silence, which the strip and title already cover. */
export function playQuestionBlip(): void {
  try {
    const ctx = new AudioContext();
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.type = "sine";
    osc.frequency.setValueAtTime(880, ctx.currentTime);
    osc.frequency.setValueAtTime(1174.7, ctx.currentTime + 0.09);
    gain.gain.setValueAtTime(0.0001, ctx.currentTime);
    gain.gain.exponentialRampToValueAtTime(0.07, ctx.currentTime + 0.02);
    gain.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + 0.24);
    osc.connect(gain);
    gain.connect(ctx.destination);
    osc.onended = () => void ctx.close();
    osc.start();
    osc.stop(ctx.currentTime + 0.26);
  } catch {
    /* no audio here */
  }
}

export type NotificationPermissionState = "granted" | "denied" | "default" | "unsupported";

/** `unsupported` covers both a browser without the API and an insecure origin, where it is hidden. */
export function notificationSupport(): NotificationPermissionState {
  if (typeof window === "undefined" || typeof Notification === "undefined") return "unsupported";
  return Notification.permission;
}

/**
 * Asks the browser for permission to notify. Must be called directly from a click handler: the
 * prompt is only shown inside a user gesture, which is why this setting lives behind a button in
 * Settings and can never fire on load.
 */
export function requestNotificationPermission(): Promise<NotificationPermissionState> {
  if (notificationSupport() === "unsupported") return Promise.resolve("unsupported");
  return Notification.requestPermission().catch(() => "denied" as const);
}

/**
 * Fires one browser notification. The title is just "Colonizer" and the body the short event text,
 * because these appear on lock screens and other people's displays. A click focuses the window and
 * opens that colony's chat; `tag` collapses repeats per colony instead of stacking them, and the
 * system sound is muted so the sound channel stays the only thing that beeps.
 */
export function showColonyNotification(text: string, id: string, onSelect: (id: string) => void): void {
  if (notificationSupport() !== "granted") return;
  try {
    const notification = new Notification("Colonizer", { body: text, tag: `colonizer:${id}`, silent: true });
    notification.onclick = () => {
      window.focus();
      onSelect(id);
      notification.close();
    };
  } catch {
    /* some contexts throw on construction; the sidebar still shows the colony */
  }
}
