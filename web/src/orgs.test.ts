// The org list's pure half, after it learned about switched-off and newly-appeared orgs (issue #176):
// a workspace choice is an org that is on and decided, a switched-off org stays reachable with its
// counts, and the prompt asks for pending orgs one at a time in a stable order.
import { describe, expect, it } from "vitest";

import { orgEnabled, orgEntries, pendingOrgPrompt } from "./orgs";
import type { OrgInfo, Session } from "./types";

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

function org(org: string, overrides: Partial<OrgInfo> = {}): OrgInfo {
  return {
    colonies: { live: 0, total: 0 },
    pending_memory: 0,
    settings: {},
    ...overrides,
    org,
  };
}

describe("orgEntries", () => {
  it("keeps orgs that only appear in the colony list", () => {
    const { visible, hidden } = orgEntries([], [session()]);
    expect(visible.map((e) => e.org)).toEqual(["acme"]);
    expect(visible[0].total).toBe(1);
    expect(visible[0].live).toBe(1);
    expect(hidden).toEqual([]);
  });

  it("excludes a switched-off org from the choices but keeps it in hidden with its counts", () => {
    const { visible, hidden } = orgEntries([org("acme", { settings: { enabled: false } })], [session()]);
    expect(visible).toEqual([]);
    expect(hidden.map((e) => e.org)).toEqual(["acme"]);
    expect(hidden[0].total).toBe(1);
  });

  it("excludes an org that is still awaiting a decision from both lists", () => {
    // It is not a workspace yet; the prompt card is where that gets decided.
    const { visible, hidden } = orgEntries([org("acme", { awaiting_decision: true })], []);
    expect(visible).toEqual([]);
    expect(hidden).toEqual([]);
  });

  it("treats absent, null and true `enabled` as on, so an existing install keeps every org", () => {
    const { visible } = orgEntries(
      [org("a", {}), org("b", { settings: { enabled: null } }), org("c", { settings: { enabled: true } })],
      [],
    );
    expect(visible.map((e) => e.org)).toEqual(["a", "b", "c"]);
  });

  it("carries the avatar onto the rows, and leaves it null for a colony-only org", () => {
    const { visible, hidden } = orgEntries(
      [org("acme", { avatar_url: "https://example.com/acme.png" }), org("octo", { settings: { enabled: false }, avatar_url: "https://example.com/octo.png" })],
      [session({ repo: "octo/hello", org: "octo", id: "s2" }), session({ repo: "globex/hello", org: "globex", id: "s3" })],
    );
    expect(visible.map((e) => [e.org, e.avatar])).toEqual([["acme", "https://example.com/acme.png"], ["globex", null]]);
    expect(hidden.map((e) => [e.org, e.avatar])).toEqual([["octo", "https://example.com/octo.png"]]);
  });

  it("carries the org's spend from /api/orgs onto the row, and leaves it off a colony-only org", () => {
    const { visible } = orgEntries(
      [org("acme", { spend: { cost_usd: 12.5, routed_cost_usd: 0.5, tokens: { input: 1, output: 1, cache_read: 0, cache_write: 0 }, models: [] } })],
      [session()],
    );
    expect(visible[0].spend?.cost_usd).toBe(12.5);
    expect(visible[0].spend?.routed_cost_usd).toBe(0.5);
  });

  it("keeps the spend when the org was first seen in the colony list", () => {
    const { visible } = orgEntries([org("acme", { spend: { cost_usd: 1, routed_cost_usd: null, tokens: { input: 1, output: 1, cache_read: 0, cache_write: 0 }, models: [] } })], [session()]);
    expect(visible[0].spend?.cost_usd).toBe(1);
  });

  it("merges case-insensitively on the first spelling seen, as before", () => {
    const { visible } = orgEntries([org("Acme", { avatar_url: "https://example.com/a.png" })], [session({ repo: "acme/webshop", org: "acme" })]);
    expect(visible).toHaveLength(1);
    expect(visible[0].org).toBe("Acme");
    expect(visible[0].avatar).toBe("https://example.com/a.png");
    expect(visible[0].total).toBe(1);
  });
});

describe("pendingOrgPrompt", () => {
  it("is null when nothing is pending", () => {
    expect(pendingOrgPrompt([org("acme"), org("octo")], new Set())).toBeNull();
  });

  it("is null on an old mothership that never sends the field", () => {
    // The field is optional: absent simply means no pending orgs.
    expect(pendingOrgPrompt([org("acme", { awaiting_decision: undefined }), org("octo")], new Set())).toBeNull();
  });

  it("asks for several pending one at a time, in name order", () => {
    const pending = [org("charlie", { awaiting_decision: true }), org("alpha", { awaiting_decision: true }), org("beta", { awaiting_decision: true })];
    expect(pendingOrgPrompt(pending, new Set())?.org).toBe("alpha");
  });

  it("skips an org already answered this session and asks the next", () => {
    const pending = [org("alpha", { awaiting_decision: true }), org("beta", { awaiting_decision: true })];
    expect(pendingOrgPrompt(pending, new Set(["alpha"]))?.org).toBe("beta");
  });

  it("matches an answered org case-insensitively", () => {
    const pending = [org("alpha", { awaiting_decision: true }), org("beta", { awaiting_decision: true })];
    expect(pendingOrgPrompt(pending, new Set(["ALPHA"]))?.org).toBe("beta");
  });

  it("is null once the last pending org has been answered", () => {
    expect(pendingOrgPrompt([org("alpha", { awaiting_decision: true })], new Set(["alpha"]))).toBeNull();
  });
});

describe("orgEnabled", () => {
  it("is on unless explicitly false", () => {
    expect(orgEnabled(undefined)).toBe(true);
    expect(orgEnabled({})).toBe(true);
    expect(orgEnabled({ enabled: null })).toBe(true);
    expect(orgEnabled({ enabled: true })).toBe(true);
    expect(orgEnabled({ enabled: false })).toBe(false);
  });
});
