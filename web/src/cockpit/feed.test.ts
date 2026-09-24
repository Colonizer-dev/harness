// The inbox reads the colony list and nothing else, so what matters is that every
// status lands on the right line, that "needs you" beats the status, that the day headings follow
// the local calendar rather than an elapsed count, and that the order holds between polls.
import { describe, expect, it } from "vitest";

import {
  OVERVIEW_FILTERS,
  RETURNED,
  dayLabel,
  feedEntries,
  feedKind,
  headlineFor,
  heldSlots,
  matchesOverviewFilter,
  needCountByOrg,
  needFor,
  overviewCounts,
  overviewSessions,
  queueStalled,
} from "./feed";
import type { Session, SessionStatus } from "../types";

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "s1",
    repo: "acme/webshop",
    org: "acme",
    issue: 42,
    issue_title: "Checkout fails for guest users",
    status: "running",
    branch: "colonizer/issue-42-s1",
    base: "main",
    parent: null,
    worktree: "/wt/s1",
    git_admin_dir: "/git/s1",
    sandbox: "colony-s1",
    mesh: null,
    agent: "claude-code",
    autopilot: false,
    pr_url: null,
    error: null,
    cost_usd: null,
    cleaned_up: false, keep_worktree: false,
    created_at: "2026-09-18T09:00:00Z",
    updated_at: "2026-09-18T09:10:00Z",
    attention: null,
    ...overrides,
  };
}

describe("feedKind", () => {
  it("reads a waiting colony as a question", () => {
    expect(feedKind(session({ status: "waiting_for_answer" }))).toBe("question");
  });

  it("lets a watchdog flag beat the status, the way the sidebar does", () => {
    const flagged = session({ status: "running", attention: { reason: "stalled", since: "2026-09-18T09:05:00Z", nudges: 2 } });
    expect(feedKind(flagged)).toBe("question");
  });

  it("groups every terminal pull-request status as returned", () => {
    for (const status of ["pr_opened", "merged", "closed", "no_changes"] as SessionStatus[]) {
      expect(feedKind(session({ status }))).toBe("returned");
    }
  });

  it("keeps the returned grouping in lockstep with the overview's RETURNED set", () => {
    // feedKind reads RETURNED itself now, so this pins the two against meeting by accident;
    // a returned status the overview counts must read as "returned" on the timeline and vice versa.
    const all: SessionStatus[] = Object.keys({ queued: 1, starting: 1, running: 1, waiting_for_answer: 1, idle: 1, publishing: 1, pr_opened: 1, merged: 1, closed: 1, no_changes: 1, stopped: 1, failed: 1 }) as SessionStatus[];
    for (const status of all) {
      expect(feedKind(session({ status })) === "returned").toBe(RETURNED.has(status));
    }
  });

  it("keeps failure, stop and queue apart from the working states", () => {
    expect(feedKind(session({ status: "failed" }))).toBe("failed");
    expect(feedKind(session({ status: "stopped" }))).toBe("stopped");
    expect(feedKind(session({ status: "queued" }))).toBe("queued");
    expect(feedKind(session({ status: "starting" }))).toBe("launched");
    expect(feedKind(session({ status: "publishing" }))).toBe("launched");
  });
});

describe("feedEntry text", () => {
  it("names the colony by its short repo and issue", () => {
    expect(feedEntries([session({ status: "pr_opened" })])[0].text).toBe("webshop#42 returned a pull request");
  });

  it("says what the watchdog flagged, not just that something waits", () => {
    const stalled = session({ attention: { reason: "stalled", since: "2026-09-18T09:05:00Z", nudges: 1 } });
    expect(feedEntries([stalled])[0].text).toBe("webshop#42 has stopped making progress");
  });

  it("reads a stacked colony's queue as waiting for its parent, not a slot", () => {
    const stacked = session({ status: "queued", parent: "root0001" });
    expect(feedEntries([stacked])[0].text).toBe("webshop#42 waits for the colony it is stacked on");
  });

  it("falls back to the repository for a colony started without an issue", () => {
    expect(feedEntries([session({ issue: null, status: "queued" })])[0].text).toBe("webshop waits for a free slot");
  });
});

describe("feedEntries", () => {
  it("puts the newest colony first", () => {
    const older = session({ id: "a", updated_at: "2026-09-18T08:00:00Z" });
    const newer = session({ id: "b", updated_at: "2026-09-18T12:00:00Z" });
    expect(feedEntries([older, newer]).map((e) => e.id)).toEqual(["b", "a"]);
  });

  it("breaks a tie on id, so the order survives a poll", () => {
    const one = session({ id: "b", updated_at: "2026-09-18T08:00:00Z" });
    const two = session({ id: "a", updated_at: "2026-09-18T08:00:00Z" });
    expect(feedEntries([one, two]).map((e) => e.id)).toEqual(["a", "b"]);
    expect(feedEntries([two, one]).map((e) => e.id)).toEqual(["a", "b"]);
  });
});

describe("dayLabel", () => {
  const now = new Date(2026, 8, 18, 10, 0, 0); // 18 Sep 2026, local

  it("calls today today", () => {
    expect(dayLabel(new Date(2026, 8, 18, 9, 0, 0).toISOString(), now)).toBe("TODAY");
  });

  it("follows the calendar, not the clock: 23:50 last night is yesterday", () => {
    expect(dayLabel(new Date(2026, 8, 17, 23, 50, 0).toISOString(), now)).toBe("YESTERDAY");
  });

  it("dates anything older", () => {
    expect(dayLabel(new Date(2026, 8, 12, 9, 0, 0).toISOString(), now)).toBe("12 SEP");
  });

  it("does not crash on a timestamp it cannot read", () => {
    expect(dayLabel("not a date", now)).toBe("EARLIER");
  });
});

describe("needCountByOrg", () => {
  it("counts only the colonies waiting on a person", () => {
    const counts = needCountByOrg([
      session({ id: "a", org: "acme", status: "waiting_for_answer" }),
      session({ id: "b", org: "acme", status: "running" }),
      session({ id: "c", org: "other", status: "waiting_for_answer" }),
    ]);
    expect(counts).toEqual({ acme: 1, other: 1 });
  });

  it("falls back to the repository owner when the mothership omits the org", () => {
    const counts = needCountByOrg([session({ id: "a", org: undefined, status: "waiting_for_answer" })]);
    expect(counts).toEqual({ acme: 1 });
  });

  it("keys by the lowercased org, so the /api/orgs spelling finds it (needFor)", () => {
    const counts = needCountByOrg([
      session({ id: "a", org: "Acme", status: "waiting_for_answer" }),
      session({ id: "b", org: "acme", status: "waiting_for_answer" }),
    ]);
    expect(counts).toEqual({ acme: 2 });
    expect(needFor(counts, "ACME")).toBe(2);
    expect(needFor(counts, "other")).toBe(0);
  });

  it("is empty when nothing waits", () => {
    expect(needCountByOrg([session({ status: "running" })])).toEqual({});
  });
});

describe("headlineFor", () => {
  it("leads with what is waiting on a person", () => {
    expect(headlineFor(2, 3)).toBe("2 colonies need you · 3 working");
  });

  it("counts one colony in the singular", () => {
    expect(headlineFor(1, 3)).toBe("1 colony needs you · 3 working");
  });

  it("says so when nothing is waiting", () => {
    expect(headlineFor(0, 3)).toBe("All quiet · 3 working");
  });

  it("does not mention work that is not happening", () => {
    expect(headlineFor(0, 0)).toBe("All quiet");
  });
});

describe("overview buckets, counts and filters", () => {
  const stalled = session({ id: "stalled", status: "running", attention: { reason: "stalled", since: "2026-09-18T09:05:00Z", nudges: 1 } });
  const list = [
    session({ id: "running", status: "running" }),
    session({ id: "waiting", status: "waiting_for_answer" }),
    stalled,
    session({ id: "pr", status: "pr_opened" }),
    session({ id: "no-changes", status: "no_changes" }),
    session({ id: "queued", status: "queued" }),
    session({ id: "idle", status: "idle" }),
    session({ id: "stopped", status: "stopped" }),
  ];

  it("defines returned as the pull-request round-trip statuses", () => {
    expect([...RETURNED].sort()).toEqual(["closed", "merged", "no_changes", "pr_opened"]);
  });

  it("lets a waiting colony count as both live and need-you, and still in the bucketed total", () => {
    // The buckets overlap on purpose (you count the same colony in each pane it belongs to), so
    // the four counters may not add up to the list — but each equals its own pane's rows.
    expect(overviewCounts(list)).toEqual({ live: 4, "need you": 2, returned: 2, queued: 1 });
  });

  it("keeps each filter in lockstep with its counter", () => {
    for (const filter of OVERVIEW_FILTERS) {
      const byPredicate = list.filter((s) => matchesOverviewFilter(s, filter));
      const byHelper = overviewSessions(list, filter);
      expect(byPredicate.map((s) => s.id)).toEqual(byHelper.map((s) => s.id));
      expect(overviewCounts(list)[filter]).toBe(byHelper.length);
    }
  });

  it("shows exactly the bucket's colonies under each filter", () => {
    expect(overviewSessions(list, "live").map((s) => s.id)).toEqual(["running", "waiting", "stalled", "idle"]);
    expect(overviewSessions(list, "need you").map((s) => s.id)).toEqual(["waiting", "stalled"]);
    expect(overviewSessions(list, "returned").map((s) => s.id)).toEqual(["pr", "no-changes"]);
    expect(overviewSessions(list, "queued").map((s) => s.id)).toEqual(["queued"]);
  });

  it("restores the whole list when the filter is cleared — a second click or the counts", () => {
    expect(overviewSessions(list, null).map((s) => s.id)).toEqual(list.map((s) => s.id));
    expect(overviewSessions([], "returned")).toEqual([]);
  });
});

// Held slots and the stalled queue (issue #217): idle colonies whose PR autopilot holds occupy
// parallel slots without doing work. When every live colony is held, the queue cannot drain.
describe("heldSlots", () => {
  const NOW = Date.parse("2026-09-18T11:48:00Z");
  const held = (id: string, since: string): Session =>
    session({ id, status: "idle", attention: { reason: "autopilot_held", since, nudges: 0 } });

  it("counts only idle colonies held by autopilot, with the oldest wait", () => {
    // A running colony carries no hold; a running colony *with* the flag is doing work, not holding.
    const list = [
      held("h1", "2026-09-18T10:00:00Z"),
      held("h2", "2026-09-18T09:00:00Z"),
      session({ id: "r1", status: "running" }),
      session({ id: "r2", status: "running", attention: { reason: "autopilot_held", since: "2026-09-18T09:00:00Z", nudges: 0 } }),
      session({ id: "q1", status: "queued" }),
    ];
    expect(heldSlots(list, NOW)).toEqual({
      count: 2,
      oldestSince: "2026-09-18T09:00:00Z",
      oldestAgeMs: NOW - Date.parse("2026-09-18T09:00:00Z"),
    });
  });

  it("is empty when nothing is held", () => {
    expect(heldSlots([session({ status: "running" })], NOW)).toEqual({ count: 0, oldestSince: null, oldestAgeMs: null });
  });
});

describe("queueStalled", () => {
  const held = (id: string): Session =>
    session({ id, status: "idle", attention: { reason: "autopilot_held", since: "2026-09-18T09:00:00Z", nudges: 0 } });
  const queued = (id: string): Session => session({ id, status: "queued" });

  it("queued plus one running plus one held is a healthy busy queue", () => {
    expect(queueStalled([held("h1"), session({ id: "r1", status: "running" }), queued("q1")])).toBe(false);
  });

  it("queued with every live colony held is stalled", () => {
    expect(queueStalled([held("h1"), held("h2"), queued("q1"), queued("q2")])).toBe(true);
  });

  it("no queued colonies is never stalled, even when everything is held", () => {
    expect(queueStalled([held("h1"), held("h2")])).toBe(false);
  });

  it("a colony mid-publish holds its slot and is still working, so the queue is not stalled", () => {
    expect(queueStalled([held("h1"), held("h2"), session({ id: "p1", status: "publishing" }), queued("q1")])).toBe(false);
  });

  it("queued with no live colonies is waiting on slots, not stalled by holds", () => {
    expect(queueStalled([queued("q1"), session({ id: "s1", status: "stopped" })])).toBe(false);
  });
});
