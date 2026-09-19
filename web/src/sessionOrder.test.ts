// sessionRank, after "needs you" was extracted for the notifications: the extraction must not have
// moved anything — a colony that needs a person still ranks 0 ahead of every group, whatever its
// status, and unflagged colonies still sort by the status table.
import { describe, expect, it } from "vitest";

import { sessionRank } from "./sessionOrder";
import type { AttentionReason, Session } from "./types";

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
    cleaned_up: false,
    created_at: "2026-09-18T09:00:00Z",
    updated_at: "2026-09-18T09:10:00Z",
    attention: null,
    ...overrides,
  };
}

describe("sessionRank", () => {
  it("ranks a waiting question first", () => {
    expect(sessionRank(session({ status: "waiting_for_answer" }))).toBe(0);
  });

  it("ranks any attention flag first, whatever its reason", () => {
    for (const reason of ["stalled", "waiting_for_answer", "nudges_exhausted", "autopilot_held"] satisfies AttentionReason[]) {
      expect(sessionRank(session({ status: "failed", attention: { reason, since: "2026-09-18T09:05:00Z", nudges: 1 } }))).toBe(0);
    }
  });

  it("leaves unflagged colonies to the status table", () => {
    expect(sessionRank(session({ status: "running" }))).toBe(1);
    expect(sessionRank(session({ status: "failed" }))).toBe(4);
  });
});
