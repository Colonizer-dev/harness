// heldByFor, the cockpit mirror of the mothership's `issue_held_by`: the pre-submit duplicate
// check must refuse exactly the statuses the server's 409 refuses, and free the rest for retry.
import { describe, expect, it } from "vitest";

import { heldByFor, holdsIssue } from "./api";
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
});
