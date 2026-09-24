// heldByFor, the cockpit mirror of the mothership's `issue_held_by`: the pre-submit duplicate
// check must refuse exactly the statuses the server's 409 refuses, and free the rest for retry.
import { describe, expect, it } from "vitest";

import { claimWaitPosition, claimWaitersFor, heldByFor, heldInBatch, holdsIssue } from "./api";
import type { Session, SessionStatus } from "./types";

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "s1",
    repo: "acme/webshop",
    org: "acme",
    issue: 7,
    issue_title: "Checkout fails for guest users",
    status: "running",
    branch: "colonizer/issue-7-s1",
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
    cleaned_up: false,
    keep_worktree: false,
    created_at: "2026-09-18T09:00:00Z",
    updated_at: "2026-09-18T09:05:00Z",
    ...overrides,
  };
}

const HOLDING: SessionStatus[] = ["queued", "starting", "running", "waiting_for_answer", "idle", "publishing", "pr_opened"];
const FREE: SessionStatus[] = ["stopped", "failed", "no_changes", "merged", "closed"];

describe("holdsIssue", () => {
  it("holds for queued, live, publishing and pr_opened", () => {
    for (const status of HOLDING) expect(holdsIssue(status)).toBe(true);
  });

  it("frees stopped, failed, no_changes, merged and closed for retry", () => {
    for (const status of FREE) expect(holdsIssue(status)).toBe(false);
  });
});

describe("heldByFor", () => {
  it("returns the colony holding the same repo and issue", () => {
    const holder = session({ id: "abc123" });
    expect(heldByFor([holder], "acme/webshop", 7)?.id).toBe("abc123");
  });

  it("ignores terminal colonies on the same issue", () => {
    for (const status of FREE) {
      expect(heldByFor([session({ status })], "acme/webshop", 7)).toBeNull();
    }
  });

  it("ignores other repos, other issues and open colonies", () => {
    const list = [
      session({ id: "other-repo", repo: "acme/other", issue: 7 }),
      session({ id: "other-issue", issue: 8 }),
      session({ id: "open", issue: null }),
    ];
    expect(heldByFor(list, "acme/webshop", 7)).toBeNull();
  });

  it("returns null for an empty list", () => {
    expect(heldByFor([], "acme/webshop", 7)).toBeNull();
  });

  // Issue #321: a queued `claim_wait` successor never reads as the holder while the real holder
  // is still live — even an older waiter listed first, as the mothership's `issue_held_by` prefers
  // the first holding colony that is not waiting in line.
  it("prefers the holder over an older claim_wait waiter", () => {
    const list = [
      session({ id: "waiter", status: "queued", claim_wait: true, created_at: "2026-09-18T09:30:00Z" }),
      session({ id: "holder1", created_at: "2026-09-18T10:00:00Z" }),
    ];
    expect(heldByFor(list, "acme/webshop", 7)?.id).toBe("holder1");
  });

  it("names the oldest waiter once the holder is gone and only waiters remain", () => {
    const list = [
      session({ id: "later", status: "queued", claim_wait: true, created_at: "2026-09-18T10:00:00Z" }),
      session({ id: "earliest", status: "queued", claim_wait: true, created_at: "2026-09-18T09:30:00Z" }),
      session({ id: "stopped-holder", status: "stopped", created_at: "2026-09-18T08:00:00Z" }),
    ];
    expect(heldByFor(list, "acme/webshop", 7)?.id).toBe("earliest");
  });

  it("falls back to any holding waiter, not only queued ones, and breaks created_at ties by list order", () => {
    const list = [
      session({ id: "listed-first", claim_wait: true, created_at: "2026-09-18T09:30:00Z" }),
      session({ id: "listed-second", claim_wait: true, created_at: "2026-09-18T09:30:00Z" }),
    ];
    expect(heldByFor(list, "acme/webshop", 7)?.id).toBe("listed-first");
    expect(heldByFor([session({ id: "starting", claim_wait: true, status: "starting" })], "acme/webshop", 7)?.id).toBe(
      "starting",
    );
  });
});

describe("heldInBatch", () => {
  const list = [
    session({ id: "live", issue: 3 }),
    session({ id: "done", issue: 4, status: "merged" }),
    session({ id: "pr", issue: 9, status: "pr_opened" }),
    session({ id: "elsewhere", repo: "acme/other", issue: 5 }),
  ];

  it("returns the selected issues another colony holds, in the order given", () => {
    expect(heldInBatch(list, "acme/webshop", [9, 4, 5, 3])).toEqual([9, 3]);
  });

  it("takes the picker's selection set as it is", () => {
    expect(heldInBatch(list, "acme/webshop", new Set([3, 4]))).toEqual([3]);
  });

  it("returns nothing when no selected issue is held", () => {
    expect(heldInBatch(list, "acme/webshop", [4, 5, 6])).toEqual([]);
    expect(heldInBatch([], "acme/webshop", [3, 9])).toEqual([]);
  });
});

// The successor queue (issue #321): queued `claim_wait` colonies line up per issue, oldest first.
describe("claimWaitersFor and claimWaitPosition", () => {
  const waiter = (id: string, created_at: string, overrides: Partial<Session> = {}) =>
    session({ id, status: "queued", claim_wait: true, queued_behind: "holder1", created_at, ...overrides });

  it("lists a repo+issue's waiters oldest first, and no one else", () => {
    const list = [
      session({ id: "holder1" }),
      waiter("late", "2026-09-18T10:00:00Z"),
      waiter("early", "2026-09-18T09:30:00Z"),
      waiter("other-issue", "2026-09-18T09:00:00Z", { issue: 8 }),
      waiter("not-queued", "2026-09-18T09:00:00Z", { status: "running" }),
      waiter("plain-queue", "2026-09-18T09:00:00Z", { claim_wait: undefined }),
    ];
    expect(claimWaitersFor(list, "acme/webshop", 7).map((s) => s.id)).toEqual(["early", "late"]);
  });

  it("positions a waiter at one past the waiters created before it", () => {
    const list = [waiter("first", "2026-09-18T09:30:00Z"), waiter("second", "2026-09-18T10:00:00Z")];
    expect(claimWaitPosition(list, list[0])).toBe(1);
    expect(claimWaitPosition(list, list[1])).toBe(2);
  });

  it("answers null for anything not a queued claim_wait colony", () => {
    const list = [session({ id: "live" }), waiter("waiting", "2026-09-18T09:30:00Z")];
    expect(claimWaitPosition(list, list[0])).toBeNull();
    // A colony the list has lost, or an open session without an issue, has no place in line.
    expect(claimWaitPosition(list, waiter("gone", "2026-09-18T09:30:00Z"))).toBeNull();
    expect(claimWaitPosition(list, waiter("open", "2026-09-18T09:30:00Z", { issue: null }))).toBeNull();
  });
});
