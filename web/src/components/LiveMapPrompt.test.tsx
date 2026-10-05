// The live-map prompt on a phone: a fixed sheet at the foot of the screen covered the inbox's option
// rows and buttons, so below `sm` it renders in the page flow — at the top of the Inbox list, inside
// its scroll root — and the desktop corner card is unchanged. Consent stays as it was: nothing is
// sent on render, only a click answers. Rendered to static markup (the test environment has no DOM).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import { ApiContext } from "../context";
import type { Api } from "../api";
import { InboxView } from "../cockpit/InboxView";
import { demoDecisions } from "../features/decisions/mock";
import type { DecisionsView } from "../types";
import { LiveMapPrompt, PHONE_QUERY, liveMapPlacement } from "./LiveMapPrompt";

function render(inline: boolean, setTelemetry = vi.fn()) {
  const api = { setTelemetry } as unknown as Api;
  const html = renderToStaticMarkup(
    <ApiContext.Provider value={api}>
      <LiveMapPrompt inline={inline} onAnswered={() => {}} onDetails={() => {}} />
    </ApiContext.Provider>,
  );
  return { html, setTelemetry };
}

describe("LiveMapPrompt", () => {
  it("goes in the flow on a phone and stays in the fixed corner everywhere else", () => {
    expect(PHONE_QUERY).toBe("(max-width: 639px)");
    expect(liveMapPlacement(true)).toBe("inline");
    expect(liveMapPlacement(false)).toBe("fixed");
  });

  it("asks the same question either way, and sends nothing until a click", () => {
    for (const inline of [true, false]) {
      const { html, setTelemetry } = render(inline);
      expect(html).toContain("Put this mothership on the live map?");
      expect(html).toContain("Off unless you say yes.");
      expect(html).toContain(">Show on the map<");
      expect(html).toContain(">No thanks<");
      expect(html).toContain(">What is sent<");
      expect(html).toContain(`data-placement="${inline ? "inline" : "fixed"}"`);
      expect(setTelemetry).not.toHaveBeenCalled();
    }
  });

  it("drops the floating shadow when it sits in the flow", () => {
    expect(render(true).html).not.toContain("shadow-[var(--shadow)]");
    expect(render(false).html).toContain("shadow-[var(--shadow)]");
  });

  it("sits at the top of the Inbox's scroll root on a phone, above the decision cards", () => {
    const [decision] = demoDecisions();
    const decisions: DecisionsView = { count: 1, decisions: [decision], prs: [], orgs: [], writes_blocked: false, writes_blocked_reason: null, paused_until: null, poll_minutes: 15 } as unknown as DecisionsView;
    const html = renderToStaticMarkup(
      <InboxView
        sessions={[]}
        onOpenColony={() => {}}
        onOpenNotificationSettings={() => {}}
        decisions={decisions}
        notice={<p data-testid="notice">Put this mothership on the live map?</p>}
      />,
    );
    const root = html.indexOf("<main data-page");
    const notice = html.indexOf('data-testid="notice"');
    expect(root).toBeGreaterThanOrEqual(0);
    // Inside the scrolling <main>, before the heading and the cards: it scrolls away with the list.
    expect(notice).toBeGreaterThan(root);
    expect(notice).toBeLessThan(html.indexOf(">Inbox<"));
    expect(notice).toBeLessThan(html.indexOf(decision.question));
    // Phone only: hidden from `sm` up, where the corner card is used instead.
    expect(html).toContain('<div class="sm:hidden"><p data-testid="notice">');
  });

  it("adds nothing to the Inbox without a notice", () => {
    const html = renderToStaticMarkup(<InboxView sessions={[]} onOpenColony={() => {}} onOpenNotificationSettings={() => {}} />);
    expect(html).not.toContain("sm:hidden\"><p");
    expect(html).not.toContain("live map");
  });
});
