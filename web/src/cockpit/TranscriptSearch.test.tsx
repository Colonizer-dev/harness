// Transcript search (issue #739): the form's query, a hit's target, and how the rows render. Static
// markup runs no effects, so fetched states arrive through the `initial` prop; the click's
// colony + turn is pinned through the pure `openHit`.
import { renderToStaticMarkup } from "react-dom/server";
import type { ReactElement } from "react";
import { describe, expect, it, vi } from "vitest";

import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import type { HistoryHit } from "../types";
import { EMPTY_TRANSCRIPT_FILTERS, TranscriptSearch, openHit, searchQuery } from "./TranscriptSearch";

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

describe("searchQuery", () => {
  it("trims the query and drops every blank filter", () => {
    expect(searchQuery({ ...EMPTY_TRANSCRIPT_FILTERS, q: "  flaky test  " })).toEqual({
      q: "flaky test",
      repo: undefined,
      org: undefined,
      agent: undefined,
      status: undefined,
      since: undefined,
      until: undefined,
    });
  });

  it("keeps every filled filter", () => {
    expect(
      searchQuery({ q: "bug", repo: "acme/webshop", org: "acme", agent: "claude-code", status: "failed", since: "2026-10-01", until: "2026-10-02" }),
    ).toEqual({ q: "bug", repo: "acme/webshop", org: "acme", agent: "claude-code", status: "failed", since: "2026-10-01", until: "2026-10-02" });
  });
});

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

  it("seeds the workspace filter from the org in view", () => {
    expect(render(<TranscriptSearch org="acme" onOpen={noop} initial={null} />)).toContain('value="acme"');
  });
});
