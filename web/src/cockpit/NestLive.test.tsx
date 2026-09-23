// The nest's live chrome (Cockpit Dashboards v3): the page head, the Live dot, the live strip and
// the flash on a chamber whose colony just moved. Static markup never runs effects, so the events
// are handed in through `liveEvents` — the same shape useLiveEvents derives in production.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { LiveEvents } from "./liveEvents";
import { NestLiveStrip, NestView, STRIP_LIMIT } from "./NestView";
import { session } from "./testFixtures";

const noop = () => {};

function nest(extra: Partial<Parameters<typeof NestView>[0]> = {}): string {
  return renderToStaticMarkup(
    <NestView
      sessions={[
        session({ id: "s1", status: "waiting_for_answer" }),
        session({ id: "s2", issue: 7, status: "running" }),
        session({ id: "s3", issue: 8, status: "queued" }),
      ]}
      capacity={5}
      selectedId={null}
      mothershipSelected={false}
      settlers={[]}
      backlogCount={0}
      avatarFor={() => null}
      onSelect={noop}
      onOpen={noop}
      onSelectMothership={noop}
      onLaunch={noop}
      {...extra}
    />,
  );
}

describe("nest live chrome", () => {
  it("heads the nest with its counts, read off the sessions", () => {
    const markup = nest();
    expect(markup).toContain(">Nest</h1>");
    expect(markup).toContain("1 need you · 2 live · 1 queued · capacity 2/5");
  });

  it("stays quiet until something actually moves", () => {
    expect(nest()).toContain("No changes yet");
  });

  it("lists events newest first, clickable while the colony is still in the nest", () => {
    const events = Array.from({ length: STRIP_LIMIT + 2 }, (_, i) => ({ id: i === 0 ? "s1" : `gone${i}`, kind: "asked" as const, text: `event ${i}`, at: i }));
    const markup = renderToStaticMarkup(<NestLiveStrip events={events} known={new Set(["s1"])} onSelect={noop} />);
    expect(markup.indexOf("event 0")).toBeLessThan(markup.indexOf("event 1"));
    expect(markup).not.toContain(`event ${STRIP_LIMIT}`);
    expect(markup).toMatch(/<button[^>]*title="event 0"/);
    expect(markup).toMatch(/<span[^>]*title="event 1"/);
  });

  it("flashes the chamber whose status just moved, and only that one", () => {
    const now = Date.now();
    const liveEvents: LiveEvents = {
      latest: { id: "s2", kind: "started", text: "acme/webshop #7 started", at: now },
      flashed: { s2: now },
      bumped: {},
      recent: [{ id: "s2", kind: "started", text: "acme/webshop #7 started", at: now }],
    };
    const markup = nest({ liveEvents });
    expect(markup).toContain("acme/webshop #7 started");
    expect(markup.match(/data-flash="true"/g)?.length).toBe(1);
    expect(markup).toMatch(/data-flash="true"[^>]*aria-label="acme\/webshop #7, Working"|aria-label="acme\/webshop #7, Working"[^>]*data-flash="true"/);
  });

  it("does not flash a move older than the flash window", () => {
    const old = Date.now() - 10_000;
    const markup = nest({ liveEvents: { latest: null, flashed: { s2: old }, bumped: {}, recent: [] } });
    expect(markup).not.toContain('data-flash="true"');
  });
});
