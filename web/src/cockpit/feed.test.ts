// The inbox and timeline read the colony list and nothing else, so what matters is that every
// status lands on the right line, that "needs you" beats the status, that the day headings follow
// the local calendar rather than an elapsed count, and that the order holds between polls.
import { describe, expect, it } from "vitest";

import { dayLabel, feedEntries, feedKind, headlineFor, historyRows, matchesFilter, needCountByOrg } from "./feed";
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
    worktree: "/wt/s1",
    git_admin_dir: "/git/s1",
    sandbox: "colony-s1",
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

describe("matchesFilter", () => {
  it("lets everything through on all", () => {
    expect(matchesFilter("queued", "all")).toBe(true);
    expect(matchesFilter("question", "all")).toBe(true);
  });

  it("keeps the pills apart", () => {
    expect(matchesFilter("question", "questions")).toBe(true);
    expect(matchesFilter("question", "returned")).toBe(false);
    expect(matchesFilter("returned", "returned")).toBe(true);
    expect(matchesFilter("failed", "returned")).toBe(true);
    expect(matchesFilter("launched", "launches")).toBe(true);
    expect(matchesFilter("stopped", "launches")).toBe(true);
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

describe("historyRows", () => {
  const now = new Date(2026, 8, 18, 10, 0, 0);

  it("heads the first entry of each day and no others", () => {
    const rows = historyRows(
      [
        session({ id: "a", updated_at: new Date(2026, 8, 18, 9, 0, 0).toISOString() }),
        session({ id: "b", updated_at: new Date(2026, 8, 18, 8, 0, 0).toISOString() }),
        session({ id: "c", updated_at: new Date(2026, 8, 17, 8, 0, 0).toISOString() }),
      ],
      "all",
      now,
    );
    expect(rows.map((r) => r.day)).toEqual(["TODAY", null, "YESTERDAY"]);
  });

  it("drops what the filter excludes", () => {
    const rows = historyRows(
      [session({ id: "a", status: "waiting_for_answer" }), session({ id: "b", status: "running" })],
      "questions",
      now,
    );
    expect(rows.map((r) => r.entry.id)).toEqual(["a"]);
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
