// The overview's colony rows are disclosures: collapsed they must not leak the issue title (that is
// the whole point of the issue), a click reveals the title and a working link to the issue itself,
// and the toggle is pure enough to pin in the plain node environment — a Set keyed by session id, so
// one row's state cannot touch its siblings'. Rendering through react-dom/server, because this
// codebase keeps tests off jsdom: the collapsed and expanded markup are both fully visible there.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { OrgEntry } from "../orgs";
import type { Session } from "../types";
import { ColonyRow, OverviewView, flipExpanded } from "./OverviewView";

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

const noop = () => {};

const ACME: OrgEntry = { org: "acme", live: 1, queued: 0, total: 1, pending: 0, avatar: null };

describe("flipExpanded", () => {
  it("expands a collapsed row on the first click", () => {
    expect(flipExpanded(new Set(), "s1")).toEqual(new Set(["s1"]));
  });

  it("collapses an expanded row on the second click", () => {
    expect(flipExpanded(new Set(["s1", "s2"]), "s1")).toEqual(new Set(["s2"]));
  });

  it("leaves the other rows' state alone, so rows expand independently", () => {
    expect(flipExpanded(new Set(["s2"]), "s1")).toEqual(new Set(["s1", "s2"]));
  });
});

describe("ColonyRow", () => {
  it("collapsed: one compact line with no issue title, and aria-expanded false", () => {
    const markup = renderToStaticMarkup(<ColonyRow session={session()} open={false} onToggle={noop} onOpenColony={noop} />);
    expect(markup).not.toContain("Checkout fails for guest users");
    expect(markup).toContain("webshop#42");
    expect(markup).not.toContain("open colony →");
    expect(markup).toMatch(/aria-expanded="false"/);
    expect(markup).toMatch(/aria-controls="/);
  });

  it("expanded: reveals the issue title and a real link to the issue, and keeps the way in", () => {
    const markup = renderToStaticMarkup(<ColonyRow session={session()} open onToggle={noop} onOpenColony={noop} />);
    expect(markup).toContain("Checkout fails for guest users");
    expect(markup).toContain('href="https://github.com/acme/webshop/issues/42"');
    expect(markup).toContain("acme/webshop #42");
    expect(markup).toContain("open colony →");
    expect(markup).toMatch(/aria-expanded="true"/);
  });

  it("never invents an issue link for a colony with no issue", () => {
    const markup = renderToStaticMarkup(<ColonyRow session={session({ issue: null, issue_title: "" })} open onToggle={noop} onOpenColony={noop} />);
    expect(markup).not.toContain("/issues/");
    expect(markup).toContain("acme/webshop");
  });

  it("says why a flagged colony needs attention", () => {
    const flagged = session({ attention: { reason: "stalled", since: "2026-09-18T09:05:00Z", nudges: 2 } });
    const markup = renderToStaticMarkup(<ColonyRow session={flagged} open onToggle={noop} onOpenColony={noop} />);
    expect(markup).toContain("No progress, nudged 2×");
  });
});

describe("OverviewView", () => {
  it("shows no issue title anywhere by default", () => {
    const markup = renderToStaticMarkup(
      <OverviewView
        sessions={[session({ id: "s1" }), session({ id: "s2", repo: "acme/design-system", issue: 7, issue_title: "Bad contrast on the nav" })]}
        orgs={[ACME]}
        cost={null}
        onOpenOrg={noop}
        onOpenColony={noop}
      />,
    );
    expect(markup).not.toContain("Checkout fails for guest users");
    expect(markup).not.toContain("Bad contrast on the nav");
    // One disclosure header per colony, all collapsed; no "open colony →" affordance leaks into the roster.
    expect(markup.match(/aria-expanded="false"/g)?.length).toBe(2);
    expect(markup).not.toContain("open colony →");
  });
});