// The notification contract: what "needs you" means (one definition shared with the sidebar's
// rank-0), what the tab may show, which transitions are worth telling a person about, that the text
// can only ever name the repository and issue number — never the issue title, the question or an
// error, because notifications land on screens other people see — and that the stored preferences
// degrade to safe defaults, with everything off restoring today's tab exactly.

// The favicon contract reads index.html from disk at test time. Those are node builtins — vitest
// runs in plain node and resolves them fine, but this tsconfig types a browser build and carries
// no @types/node, so tsc must look away from exactly these two imports.
// @ts-expect-error node:fs — no @types/node in this browser-facing tsconfig
import { readFileSync } from "node:fs";
// @ts-expect-error node:url — no @types/node in this browser-facing tsconfig
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import {
  colonyLabel,
  defaultNotificationPrefs,
  diffEvents,
  eventText,
  faviconHref,
  needsYou,
  needsYouLabel,
  orgFilterForTarget,
  parseNotificationPrefs,
  serializeNotificationPrefs,
  snapshotOf,
  tabTitle,
  type ColonyEvent,
  type EventSwitches,
} from "./notifications";
import type { AttentionReason, Session } from "./types";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

let nextId = 0;

function session(overrides: Partial<Session> = {}): Session {
  const id = overrides.id ?? `s${++nextId}`;
  return {
    id,
    repo: "acme/webshop",
    org: "acme",
    issue: 42,
    issue_title: "Checkout fails for guest users",
    status: "running",
    branch: `colonizer/issue-42-${id}`,
    base: "main",
    parent: null,
    worktree: `/wt/${id}`,
    git_admin_dir: `/git/${id}`,
    sandbox: `colony-${id}`,
    mesh: null,
    agent: "claude-code",
    autopilot: false,
    pr_url: null,
    error: null,
    cost_usd: null,
    cleaned_up: false,
    created_at: "2026-09-18T09:00:00Z",
    updated_at: "2026-09-18T09:10:00Z",
    attention: null,
    ...overrides,
  };
}

const stalled = (nudges = 1) => ({ reason: "stalled" as const, since: "2026-09-18T09:05:00Z", nudges });

const ALL_ON: EventSwitches = { question: true, attention: true, failed: true, pull_request: true };
const ALL_OFF: EventSwitches = { question: false, attention: false, failed: false, pull_request: false };

// ---------------------------------------------------------------------------
// The predicate
// ---------------------------------------------------------------------------

describe("needsYou", () => {
  it("is true when a question waits on a person", () => {
    expect(needsYou(session({ status: "waiting_for_answer" }))).toBe(true);
  });

  it("is true for any attention flag, whatever its reason", () => {
    for (const reason of ["stalled", "waiting_for_answer", "nudges_exhausted", "autopilot_held"] satisfies AttentionReason[]) {
      expect(needsYou(session({ status: "running", attention: { reason, since: "2026-09-18T09:05:00Z", nudges: 1 } }))).toBe(true);
    }
  });

  it("is false for a colony that is merely working, finished or failed unflagged", () => {
    expect(needsYou(session({ status: "running" }))).toBe(false);
    expect(needsYou(session({ status: "failed" }))).toBe(false);
    expect(needsYou(session({ status: "pr_opened" }))).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// The tab
// ---------------------------------------------------------------------------

describe("tabTitle", () => {
  it("carries the count of colonies that need a person", () => {
    expect(tabTitle(1)).toBe("(1) Colonizer");
    expect(tabTitle(2)).toBe("(2) Colonizer");
  });

  it("is today's plain title at zero", () => {
    expect(tabTitle(0)).toBe("Colonizer");
  });
});

describe("faviconHref", () => {
  // The href index.html ships with, read from the file itself at test time rather than copied
  // here: a copy would let the page and this contract drift while every assertion still passed.
  // Byte-identical is the whole point — with the in-tab layer off the page must get today's
  // favicon back, not a lookalike.
  const indexHtml = readFileSync(fileURLToPath(new URL("../index.html", import.meta.url)), "utf8");
  const SHIPPED = indexHtml.match(/<link rel="icon" href="([^"]+)"/)?.[1] ?? "";

  it("finds the shipped favicon in index.html — the file this contract must not drift from", () => {
    expect(SHIPPED.length).toBeGreaterThan(0);
    expect(SHIPPED.startsWith("data:image/svg+xml")).toBe(true);
  });

  it("is the shipped favicon when nothing waits", () => {
    expect(faviconHref(false)).toBe(SHIPPED);
  });

  it("adds a dot on the same brand SVG while a colony waits", () => {
    const waiting = faviconHref(true);
    expect(waiting.startsWith("data:image/svg+xml,")).toBe(true);
    expect(waiting).not.toBe(SHIPPED);
    expect(waiting).toContain("<circle cx='25' cy='7'");
    expect(waiting).toContain(SHIPPED.slice(SHIPPED.indexOf("<rect"), SHIPPED.lastIndexOf("<circle")));
  });
});

describe("needsYouLabel", () => {
  it("is singular for one colony", () => {
    expect(needsYouLabel(1)).toBe("1 colony needs you");
  });

  it("is plural beyond one", () => {
    expect(needsYouLabel(2)).toBe("2 colonies need you");
  });
});

describe("colonyLabel", () => {
  it("names the repository and issue", () => {
    expect(colonyLabel("acme/webshop", 42)).toBe("acme/webshop #42");
  });

  it("is just the repository for a colony launched without an issue", () => {
    expect(colonyLabel("acme/webshop", null)).toBe("acme/webshop");
  });
});

// ---------------------------------------------------------------------------
// The strip's jump
// ---------------------------------------------------------------------------

describe("orgFilterForTarget", () => {
  it("keeps the filter when the colony it opens is already in it", () => {
    expect(orgFilterForTarget("acme", session())).toBe("acme");
  });

  it("clears the filter when the colony belongs to another org, so the jump lands in the visible list", () => {
    expect(orgFilterForTarget("acme", session({ org: "globex" }))).toBe(null);
  });

  it("leaves no filter alone — with every org shown nothing can be hidden", () => {
    expect(orgFilterForTarget(null, session({ org: "globex" }))).toBe(null);
  });

  it("matches the filter the way the list does, case-insensitively", () => {
    expect(orgFilterForTarget("ACME", session())).toBe("ACME");
  });

  it("resolves a colony whose org the mothership omits through the repository owner, without crashing or clearing spuriously", () => {
    expect(orgFilterForTarget("acme", session({ org: undefined }))).toBe("acme");
    expect(orgFilterForTarget("globex", session({ org: undefined }))).toBe(null);
  });
});

// ---------------------------------------------------------------------------
// The edge-triggered diff
// ---------------------------------------------------------------------------

describe("snapshotOf", () => {
  it("keeps the status and attention reason per colony", () => {
    expect(
      snapshotOf([session({ id: "a", status: "waiting_for_answer" }), session({ id: "b", status: "running", attention: stalled() })]),
    ).toEqual({
      a: { status: "waiting_for_answer", attention: null },
      b: { status: "running", attention: "stalled" },
    });
  });

  it("carries only the colonies it was given, so a rebuild prunes colonies that disappeared", () => {
    const before = snapshotOf([session({ id: "a" }), session({ id: "gone" })]);
    expect(before).toHaveProperty("gone");
    expect(snapshotOf([session({ id: "a" })])).not.toHaveProperty("gone");
  });
});

describe("diffEvents", () => {
  it("never fires on the first sight of a colony, however loudly it is waiting", () => {
    expect(
      diffEvents({}, [session({ status: "waiting_for_answer", attention: stalled() })], ALL_ON),
    ).toEqual([]);
  });

  it("fires question when a colony enters waiting_for_answer", () => {
    const previous = snapshotOf([session({ id: "a", status: "running" })]);
    expect(diffEvents(previous, [session({ id: "a", status: "waiting_for_answer" })], ALL_ON)).toEqual([
      { id: "a", repo: "acme/webshop", issue: 42, kind: "question", reason: null },
    ]);
  });

  it("does not repeat question while the colony stays waiting", () => {
    const previous = snapshotOf([session({ id: "a", status: "waiting_for_answer" })]);
    expect(diffEvents(previous, [session({ id: "a", status: "waiting_for_answer" })], ALL_ON)).toEqual([]);
  });

  it("fires attention for stalled and nudges_exhausted only — waiting_for_answer duplicates question, autopilot_held is not a person's turn", () => {
    const previous = snapshotOf([session({ id: "a", status: "running" })]);
    const fire = (reason: AttentionReason) =>
      diffEvents(previous, [session({ id: "a", status: "running", attention: { reason, since: "2026-09-18T09:05:00Z", nudges: 1 } })], ALL_ON);
    expect(fire("stalled").map((e) => e.kind)).toEqual(["attention"]);
    expect(fire("nudges_exhausted").map((e) => e.kind)).toEqual(["attention"]);
    expect(fire("waiting_for_answer")).toEqual([]);
    expect(fire("autopilot_held")).toEqual([]);
  });

  it("does not repeat attention while the same reason holds", () => {
    const previous = snapshotOf([session({ id: "a", status: "running", attention: stalled() })]);
    expect(diffEvents(previous, [session({ id: "a", status: "running", attention: stalled(2) })], ALL_ON)).toEqual([]);
  });

  it("fires attention again when the reason deepens from stalled to nudges_exhausted", () => {
    const previous = snapshotOf([session({ id: "a", status: "running", attention: stalled() })]);
    const events = diffEvents(
      previous,
      [session({ id: "a", status: "running", attention: { reason: "nudges_exhausted", since: "2026-09-18T09:05:00Z", nudges: 4 } })],
      ALL_ON,
    );
    expect(events.map((e) => [e.kind, e.reason])).toEqual([["attention", "nudges_exhausted"]]);
  });

  it("fires failed and pull_request on their transitions only", () => {
    const previous = snapshotOf([session({ id: "a", status: "running" })]);
    expect(diffEvents(previous, [session({ id: "a", status: "failed", error: "git push rejected" })], ALL_ON).map((e) => e.kind)).toEqual(["failed"]);
    expect(
      diffEvents(previous, [session({ id: "a", status: "pr_opened", pr_url: "https://github.com/acme/webshop/pull/61" })], ALL_ON).map((e) => e.kind),
    ).toEqual(["pull_request"]);
    const failedBefore = snapshotOf([session({ id: "a", status: "failed" })]);
    expect(diffEvents(failedBefore, [session({ id: "a", status: "failed" })], ALL_ON)).toEqual([]);
  });

  it("gates each kind on its own switch, leaving the others working", () => {
    const previous = snapshotOf([session({ id: "a", status: "running" })]);
    const waiting = [session({ id: "a", status: "waiting_for_answer" })];
    expect(diffEvents(previous, waiting, ALL_OFF)).toEqual([]);
    expect(diffEvents(previous, waiting, { ...ALL_OFF, question: true }).map((e) => e.kind)).toEqual(["question"]);
    expect(diffEvents(previous, waiting, { ...ALL_ON, question: false })).toEqual([]);
  });

  it("fires at most once per (colony, event) across many colonies, and seeds a colony seen for the first time mid-session", () => {
    const previous = snapshotOf([session({ id: "a", status: "running" }), session({ id: "b", status: "running" })]);
    const events = diffEvents(
      previous,
      [session({ id: "a", status: "waiting_for_answer" }), session({ id: "b", status: "failed" }), session({ id: "c", status: "waiting_for_answer" })],
      ALL_ON,
    );
    expect(events.map((e) => `${e.id}:${e.kind}`).sort()).toEqual(["a:question", "b:failed"]);
  });

  it("carries the address of the colony and nothing else", () => {
    const previous = snapshotOf([session({ id: "a", status: "running" })]);
    const [event] = diffEvents(previous, [session({ id: "a", status: "waiting_for_answer" })], ALL_ON);
    expect(event?.repo).toBe("acme/webshop");
    expect(event?.issue).toBe(42);
    expect(Object.values(event ?? {})).not.toContain("Checkout fails for guest users");
  });
});

// ---------------------------------------------------------------------------
// The text
// ---------------------------------------------------------------------------

describe("eventText", () => {
  const event = (overrides: Partial<ColonyEvent>): ColonyEvent => ({ id: "a", repo: "acme/webshop", issue: 42, kind: "question", reason: null, ...overrides });

  it("says each event in the same dull shape", () => {
    expect(eventText(event({ kind: "question" }))).toBe("acme/webshop #42 needs an answer");
    expect(eventText(event({ kind: "attention", reason: "stalled" }))).toBe("acme/webshop #42 has stalled");
    expect(eventText(event({ kind: "attention", reason: "nudges_exhausted" }))).toBe("acme/webshop #42 is out of nudges");
    expect(eventText(event({ kind: "failed" }))).toBe("acme/webshop #42 failed");
    expect(eventText(event({ kind: "pull_request" }))).toBe("acme/webshop #42 opened a pull request");
  });

  it("renders a colony without an issue as just the repository", () => {
    expect(eventText(event({ issue: null, kind: "question" }))).toBe("acme/webshop needs an answer");
  });
});

// ---------------------------------------------------------------------------
// The preferences blob
// ---------------------------------------------------------------------------

describe("notification preferences", () => {
  it("defaults to the in-tab layer on and everything noisy off", () => {
    expect(defaultNotificationPrefs()).toEqual({
      inTab: true,
      sound: false,
      browser: false,
      events: { question: true, attention: true, failed: true, pull_request: true },
    });
  });

  it("round-trips a stored blob exactly", () => {
    const prefs: NotificationPrefsLike = { inTab: false, sound: true, browser: true, events: { question: false, attention: true, failed: false, pull_request: true } };
    expect(parseNotificationPrefs(serializeNotificationPrefs(prefs))).toEqual(prefs);
  });

  it("falls back to the defaults for null, garbage and the wrong shape", () => {
    for (const raw of [null, "", "not json", "42", "null", '"a string"', "[]", "{}", '{"events":[]}']) {
      expect(parseNotificationPrefs(raw)).toEqual(defaultNotificationPrefs());
    }
  });

  it("fills missing fields and rejects wrong-typed ones, per field", () => {
    expect(parseNotificationPrefs(JSON.stringify({ sound: true, browser: "yes", events: { question: false } }))).toEqual({
      inTab: true,
      sound: true,
      browser: false,
      events: { question: false, attention: true, failed: true, pull_request: true },
    });
  });

  it("parses an everything-off blob to everything off — the state that restores today's behaviour", () => {
    const everythingOff = { inTab: false, sound: false, browser: false, events: ALL_OFF };
    expect(parseNotificationPrefs(serializeNotificationPrefs(everythingOff))).toEqual(everythingOff);
  });
});

type NotificationPrefsLike = ReturnType<typeof defaultNotificationPrefs>;
