// The nest's colony affordances are real focusable buttons — a chamber selects/opens a colony and
// a waiting colony's strip jumps straight to the colony view — never clickable divs. Issue #214:
// the cockpit view (with all its ways in) is what a blocked machine must no longer hide, so the
// ways in themselves are pinned here as enabled buttons. Rendering through react-dom/server,
// because this codebase keeps tests off jsdom; effects never run, so the plot's ResizeObserver is
// a non-issue and normalizeBox's 0×0 fallback gives the prose a valid box.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { Session } from "../types";
import { NestView } from "./NestView";

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

const noop = () => {};

describe("NestView", () => {
  it("opens a waiting colony's strip through a real enabled button", () => {
    const markup = renderToStaticMarkup(
      <NestView
        sessions={[session({ id: "s1", status: "waiting_for_answer" })]}
        selectedId={null}
        mothershipSelected={false}
        settlers={[]}
        backlogCount={3}
        avatarFor={() => null}
        onSelect={noop}
        onOpen={noop}
        onSelectMothership={noop}
        onLaunch={noop}
      />,
    );
    expect(markup).toContain("answer →");
    expect(markup).toMatch(/<button type="button"[^>]*>[\s\S]*?answer →<\/span><\/button>/);
    expect(markup).not.toContain("disabled");
  });

  it("renders every colony chamber as a real focusable button naming the colony", () => {
    const markup = renderToStaticMarkup(
      <NestView
        sessions={[session({ id: "s1" }), session({ id: "s2", repo: "acme/design-system", issue: 7 })]}
        selectedId="s1"
        mothershipSelected={false}
        settlers={[]}
        backlogCount={3}
        avatarFor={() => null}
        onSelect={noop}
        onOpen={noop}
        onSelectMothership={noop}
        onLaunch={noop}
      />,
    );
    // Each chamber is a real button with an aria-label naming its colony and status.
    expect(markup.match(/<button type="button"/g)?.length ?? 0).toBeGreaterThanOrEqual(2);
    expect(markup).toContain('aria-label="acme/webshop #42, Working"');
    expect(markup).toContain('aria-label="acme/design-system #7, Working"');
    expect(markup).not.toContain("disabled");
  });
});