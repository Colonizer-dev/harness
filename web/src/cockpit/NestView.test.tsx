// The nest's colony affordances are real focusable buttons — a chamber selects/opens a colony and
// a waiting colony's strip jumps straight to the colony view — never clickable divs. Issue #214:
// the cockpit view (with all its ways in) is what a blocked machine must no longer hide, so the
// ways in themselves are pinned here as enabled buttons. Rendering through react-dom/server,
// because this codebase keeps tests off jsdom; effects never run, so the plot's ResizeObserver is
// a non-issue and normalizeBox's 0×0 fallback gives the prose a valid box.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { Session } from "../types";
import { NestView, planBalloons, type BalloonAnchor } from "./NestView";

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

  it("shows every chamber's feed line in a visible balloon, no hover needed", () => {
    const markup = renderToStaticMarkup(
      <NestView
        sessions={[session({ id: "s1" }), session({ id: "s2", status: "failed" })]}
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
    // The colony-level feed lines render as static text, toned by state, above the carriers.
    expect(markup).toContain('title="webshop#42 is working"');
    expect(markup).toContain('title="webshop#42 failed"');
    expect(markup).toContain("pointer-events-none absolute inset-0 z-[4]");
    // The overlay swallows no clicks and hides nothing from assistive tech: the chambers'
    // own title/aria-label already convey the text.
    expect(markup).toContain('aria-hidden="true"');
  });

  it("escalates the selected chamber to the live stream detail, falling back to feed text", () => {
    const props = {
      sessions: [session({ id: "s1" }), session({ id: "s2", status: "failed" })],
      selectedId: "s1" as string | null,
      mothershipSelected: false,
      settlers: [],
      backlogCount: 3,
      avatarFor: () => null,
      onSelect: noop,
      onOpen: noop,
      onSelectMothership: noop,
      onLaunch: noop,
    };
    const live = renderToStaticMarkup(<NestView {...props} liveDetail="Cloning acme/webshop" />);
    expect(live).toContain('title="Cloning acme/webshop"');
    expect(live).toContain('title="webshop#42 failed"');
    const quiet = renderToStaticMarkup(<NestView {...props} liveDetail="" />);
    expect(quiet).toContain('title="webshop#42 is working"');
  });

  it("draws only as many chambers as the machine runs at once: 5 by default", () => {
    const sessions = Array.from({ length: 7 }, (_, i) => session({ id: `s${i + 1}` }));
    const markup = renderToStaticMarkup(
      <NestView
        sessions={sessions}
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
    // Each chamber is a real button naming its colony; the carriers' own buttons read
    // "webshop#42 · working" with no org, so only chambers match here.
    expect(markup.match(/aria-label="acme\/webshop #42, Working"/g)?.length ?? 0).toBe(5);
    // All five chambers are taken, so there is nowhere left to dig.
    expect(markup).not.toContain("DIG");
  });

  it("opens every chamber the machine runs when capacity covers the sessions, plus a DIG slot", () => {
    const sessions = Array.from({ length: 7 }, (_, i) => session({ id: `s${i + 1}` }));
    const markup = renderToStaticMarkup(
      <NestView
        sessions={sessions}
        capacity={8}
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
    expect(markup.match(/aria-label="acme\/webshop #42, Working"/g)?.length ?? 0).toBe(7);
    expect(markup).toContain("DIG");
  });

  it("clips long balloon text to one truncated line keeping the full string in the title", () => {
    const long = "Cloning acme/webshop and then running the whole migration suite end to end";
    const markup = renderToStaticMarkup(
      <NestView
        sessions={[session({ id: "s1" })]}
        selectedId="s1"
        mothershipSelected={false}
        settlers={[]}
        liveDetail={long}
        backlogCount={3}
        avatarFor={() => null}
        onSelect={noop}
        onOpen={noop}
        onSelectMothership={noop}
        onLaunch={noop}
      />,
    );
    expect(markup).toContain(`title="${long}"`);
    expect(markup).toContain("truncate");
    expect(markup).toContain("max-width");
  });
});

describe("planBalloons", () => {
  function anchor(overrides: Partial<BalloonAnchor> = {}): BalloonAnchor {
    return { id: "a", x: 100, y: 300, r: 70, diameter: 140, updatedAt: "2026-09-18T09:10:00Z", selected: false, ...overrides };
  }

  it("suppresses balloons on chambers too small to read, but always keeps the selected one", () => {
    const shown = planBalloons([
      anchor({ id: "small", diameter: 80, x: 100 }),
      anchor({ id: "tiny-selected", diameter: 80, x: 400, selected: true }),
      anchor({ id: "roomy", diameter: 140, x: 700 }),
    ]).map((a) => a.id);
    expect(shown).not.toContain("small");
    expect(shown).toContain("tiny-selected");
    expect(shown).toContain("roomy");
  });

  it("resolves overlaps newest-first, skipping balloons anchored near one already shown", () => {
    const shown = planBalloons([
      anchor({ id: "old", x: 100, updatedAt: "2026-09-18T09:00:00Z" }),
      // 50px from "old": the newer one wins, the older is skipped.
      anchor({ id: "new", x: 150, updatedAt: "2026-09-18T09:20:00Z" }),
      anchor({ id: "far", x: 600, updatedAt: "2026-09-18T08:00:00Z" }),
    ]).map((a) => a.id);
    expect(shown).toContain("new");
    expect(shown).not.toContain("old");
    expect(shown).toContain("far");
  });
});