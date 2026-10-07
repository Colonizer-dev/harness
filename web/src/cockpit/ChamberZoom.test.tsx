// The chamber zoom renders without jsdom, through react-dom/server like the nest itself:
// effects never run, so Escape and the zoom-out timer are the reducer's business, pinned below.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { ChamberZoom, zoomReducer } from "./ChamberZoom";
import { colonySays, settlerSays } from "./bubbles";
import { feedEntry } from "./feed";
import { session, settlerView } from "./testFixtures";

const noop = () => {};

function zoomed() {
  const scout = settlerView();
  const builder = settlerView({
    agent: { id: "a2", name: "builder" },
    name: "Builder 1",
    role: "builder",
    state: "thinking",
    current: null,
    last: { name: "Edit", input: { file_path: "src/cockpit/feed.ts" } },
  });
  const markup = renderToStaticMarkup(
    <ChamberZoom
      session={session()}
      settlers={[scout, builder]}
      liveDetail="Cloning acme/webshop"
      slot={{ x: 440, y: 300 }}
      blob="48% 52% 44% 56% / 52% 46% 54% 48%"
      edge="var(--accent)"
      closing={false}
      onClose={noop}
      onClosed={noop}
      onOpen={noop}
    />,
  );
  return { markup, scout, builder };
}

describe("ChamberZoom", () => {
  it("stages the whole den: a named dialog, an ant and bubble per settler, and working buttons", () => {
    const { markup, scout, builder } = zoomed();
    // The dialog names the colony and grows out of its chamber.
    expect(markup).toContain('role="dialog"');
    expect(markup).toContain('aria-modal="true"');
    expect(markup).toContain('aria-label="Inside acme/webshop #42"');
    expect(markup).toContain("transform-origin:440px 300px");
    // One ant per settler plus the colony's own, each named.
    expect(markup.match(/data-zoom-ant/g)?.length ?? 0).toBe(3);
    expect(markup).toContain("Scout 1");
    expect(markup).toContain("Builder 1");
    expect(markup).toContain("orchestrator");
    // Every ant speaks its own always-visible bubble; the colony ant reads the live detail.
    expect(markup).toContain(`title="${settlerSays(scout).title}"`);
    expect(markup).toContain(`title="${settlerSays(builder).title}"`);
    expect(markup).toContain(`title="${colonySays("Cloning acme/webshop", feedEntry(session()).text).title}"`);
    // A real back button and a real open-colony button, plus the header's facts.
    expect(markup).toMatch(/<button type="button"[^>]*>[\s\S]*?← nest[\s\S]*?<\/button>/);
    expect(markup).toMatch(/<button type="button"[^>]*>[\s\S]*?open colony →[\s\S]*?<\/button>/);
    expect(markup).toContain("acme/webshop #42");
    expect(markup).toContain("Checkout fails for guest users");
  });
});

describe("zoomReducer", () => {
  it("plays the zoom-out before unmounting on close", () => {
    const open = zoomReducer({ phase: "closed" }, { type: "open", id: "s1", x: 1, y: 2 });
    expect(open).toEqual({ phase: "open", id: "s1", x: 1, y: 2 });
    const closing = zoomReducer(open, { type: "close" });
    expect(closing).toEqual({ phase: "closing", id: "s1", x: 1, y: 2 });
    expect(zoomReducer(closing, { type: "closed" })).toEqual({ phase: "closed" });
    // Closing an already-closed zoom is a no-op.
    expect(zoomReducer({ phase: "closed" }, { type: "close" })).toEqual({ phase: "closed" });
  });

  it("zooms into another chamber straight after closing", () => {
    const first = zoomReducer({ phase: "closed" }, { type: "open", id: "s1", x: 1, y: 2 });
    const out = zoomReducer(zoomReducer(first, { type: "close" }), { type: "closed" });
    expect(zoomReducer(out, { type: "open", id: "s2", x: 3, y: 4 })).toEqual({
      phase: "open",
      id: "s2",
      x: 3,
      y: 4,
    });
  });

  it("drops the zoom when the selection moves elsewhere, and keeps it when it names the zoomed colony", () => {
    const open = zoomReducer({ phase: "closed" }, { type: "open", id: "s1", x: 1, y: 2 });
    expect(zoomReducer(open, { type: "selection", id: "s1" })).toBe(open);
    expect(zoomReducer(open, { type: "selection", id: "s2" })).toEqual({ phase: "closed" });
    expect(zoomReducer(open, { type: "selection", id: null })).toEqual({ phase: "closed" });
    expect(zoomReducer({ phase: "closed" }, { type: "selection", id: "s2" })).toEqual({ phase: "closed" });
  });
});
