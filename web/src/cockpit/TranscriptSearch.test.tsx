// Transcript search (issue #739): the form's query, a hit's target, and how the rows render. Static
// markup runs no effects, so fetched states arrive through the `initial` prop; the click's
// colony + turn is pinned through the pure `openHit`.
import { renderToStaticMarkup } from "react-dom/server";
import type { ReactElement } from "react";
import { describe, expect, it, vi } from "vitest";

import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import type { HistoryHit } from "../types";
import { TranscriptSearch, openHit } from "./TranscriptSearch";

const api = createMockApi();
const noop = () => {};

function hit(overrides: Partial<HistoryHit> = {}): HistoryHit {
  return {
    colony: "s-1",
    repo: "acme/webshop",
    org: "acme",
    agent: "claude-code",
    status: "running",
    created_at: "2026-10-01T10:00:00Z",
    seq: 7,
    ts: "2026-10-01T10:05:00Z",
    turn: "m-1",
    role: "user",
    snippet: "the checkout flow breaks for guests",
    ...overrides,
  };
}

const render = (node: ReactElement) => renderToStaticMarkup(<ApiContext.Provider value={api}>{node}</ApiContext.Provider>);

describe("openHit", () => {
  it("opens the hit's colony at its turn", () => {
    const open = vi.fn();
    openHit(hit(), open);
    expect(open).toHaveBeenCalledWith("s-1", "m-1");
  });

  it("opens the colony alone when the mothership named no turn", () => {
    const open = vi.fn();
    openHit(hit({ turn: null }), open);
    expect(open).toHaveBeenCalledWith("s-1", null);
  });
});

describe("TranscriptSearch", () => {
  it("renders a hit's repository, agent, status, role and snippet", () => {
    const markup = render(<TranscriptSearch onOpen={noop} initial={[hit()]} />);
    expect(markup).toContain("acme/webshop");
    expect(markup).toContain("claude-code");
    expect(markup).toContain("Working");
    expect(markup).toContain("the checkout flow breaks for guests");
    expect(markup).toContain("You");
  });

  it("draws a snippet as plain text, never HTML", () => {
    const markup = render(<TranscriptSearch onOpen={noop} initial={[hit({ snippet: "<b>bold</b> alert(1)" })]} />);
    expect(markup).toContain("&lt;b&gt;bold&lt;/b&gt; alert(1)");
    expect(markup).not.toContain("<b>bold</b>");
  });

  it("says so when nothing matched", () => {
    expect(render(<TranscriptSearch onOpen={noop} initial={[]} />)).toContain("No turn matches this search.");
  });

  it("shows the filter chips, no org field and no Search button", () => {
    const markup = render(<TranscriptSearch org="acme" onOpen={noop} initial={null} />);
    for (const name of ["Repository", "Agent", "Status", "Date"]) expect(markup).toContain(name);
    expect(markup).not.toContain('aria-label="Workspace"');
    expect(markup).not.toContain(">Search</button>");
  });
});
