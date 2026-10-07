// parentOf, ancestorsOf and childrenOf over the colony list: what matters is that a chain reads
// root first, that a parent id which has left the list reads as unstacked rather than crashing,
// and that a cycle or an absurd depth ends the walk instead of hanging the render.
import { describe, expect, it } from "vitest";

import { MAX_STACK_DEPTH, ancestorsOf, childrenOf, parentOf } from "./stack";
import type { Session } from "./types";

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

describe("parentOf", () => {
  it("answers null for a colony that branches from the default branch", () => {
    expect(parentOf([], session())).toBeNull();
  });

  it("resolves the colony a stacked one branches from", () => {
    const root = session({ id: "root", branch: "colonizer/issue-42-root" });
    const stacked = session({ id: "s2", parent: "root", base: "colonizer/issue-42-root" });
    expect(parentOf([root, stacked], stacked)).toBe(root);
  });

  it("answers null when the parent has aged out of the list", () => {
    expect(parentOf([], session({ parent: "gone0001" }))).toBeNull();
  });
});

describe("ancestorsOf", () => {
  it("is empty without a parent", () => {
    expect(ancestorsOf([], session())).toEqual([]);
  });

  it("walks a two-deep chain root first", () => {
    const root = session({ id: "root" });
    const child = session({ id: "child", parent: "root" });
    expect(ancestorsOf([root, child], child)).toEqual([root]);
  });

  it("walks a three-deep chain root first", () => {
    const root = session({ id: "root" });
    const middle = session({ id: "middle", parent: "root" });
    const top = session({ id: "top", parent: "middle" });
    expect(ancestorsOf([top, middle, root], top)).toEqual([root, middle]);
  });

  it("ends the walk at a cycle instead of following it around", () => {
    const a = session({ id: "a", parent: "b" });
    const b = session({ id: "b", parent: "a" });
    expect(ancestorsOf([a, b], a)).toEqual([b]);
  });

  it("ends the walk at a parent id that is not in the list", () => {
    const child = session({ id: "child", parent: "gone0001" });
    const other = session({ id: "other" });
    expect(ancestorsOf([child, other], child)).toEqual([]);
  });

  it("gives up at a pathological depth rather than walking forever", () => {
    const chain = Array.from({ length: MAX_STACK_DEPTH + 10 }, (_, i) => session({ id: `s${i}`, parent: `s${i + 1}` }));
    expect(ancestorsOf(chain, chain[0])).toHaveLength(MAX_STACK_DEPTH);
  });
});

describe("childrenOf", () => {
  it("finds the colonies stacked directly on this one", () => {
    const root = session({ id: "root" });
    const first = session({ id: "first", parent: "root" });
    const second = session({ id: "second", parent: "root" });
    const elsewhere = session({ id: "elsewhere", parent: "middle" });
    expect(childrenOf([root, first, second, elsewhere], root)).toEqual([first, second]);
  });

  it("does not count grandchildren, only direct children", () => {
    const root = session({ id: "root" });
    const middle = session({ id: "middle", parent: "root" });
    const top = session({ id: "top", parent: "middle" });
    expect(childrenOf([root, middle, top], root)).toEqual([middle]);
  });

  it("does not count the colony itself when its parent link points at it", () => {
    const self = session({ id: "s1", parent: "s1" });
    expect(childrenOf([self], self)).toEqual([]);
  });
});
