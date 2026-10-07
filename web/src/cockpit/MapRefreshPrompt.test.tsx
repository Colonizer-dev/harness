import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { mapLoopBody, mapLoopName, toUtcLoopCadence } from "./loops";
import { MapRefreshCovered, MapRefreshPrompt, mapRefreshKey, mapRefreshPrompt, parseMapRefreshAnswer, type MapRefreshAnswer } from "./MapRefreshPrompt";
import { mapLoop } from "./testFixtures";

describe("mapRefreshPrompt", () => {
  const repo = "acme/web";
  const now = Date.UTC(2026, 8, 26, 12, 0);
  const notNow = (days: number): MapRefreshAnswer => ({ answer: "not_now", at: new Date(now - days * 86_400_000).toISOString() });

  it("asks once a map exists and nothing covers it yet", () => {
    expect(mapRefreshPrompt([], repo, null, now)).toBe("ask");
    expect(mapRefreshPrompt([], repo, parseMapRefreshAnswer("{oops"), now)).toBe("ask");
  });

  it("stands down when an enabled map loop covers the repository or its whole org", () => {
    const covering = mapLoop({ repo, name: mapLoopName(repo) });
    expect(mapRefreshPrompt([covering], repo, null, now)).toEqual({ covered: covering });
    // The org wildcard covers every repository in it, owner compared case-insensitively.
    const orgWide = mapLoop({ repo: "ACME/*" });
    expect(mapRefreshPrompt([orgWide], repo, null, now)).toEqual({ covered: orgWide });
  });

  it("ignores colony loops, paused map loops and other repositories", () => {
    expect(mapRefreshPrompt([mapLoop({ kind: "colony" })], repo, null, now)).toBe("ask");
    expect(mapRefreshPrompt([mapLoop({ enabled: false })], repo, null, now)).toBe("ask");
    expect(mapRefreshPrompt([mapLoop({ repo: "acme/other" })], repo, null, now)).toBe("ask");
  });

  it("never re-asks a created answer, and not-now hides the question for 30 days", () => {
    expect(mapRefreshPrompt([], repo, { answer: "created" }, now)).toBe("hidden");
    expect(mapRefreshPrompt([], repo, notNow(29), now)).toBe("hidden");
    expect(mapRefreshPrompt([], repo, notNow(31), now)).toBe("ask");
  });

  it("reads a missing or garbled stored answer as never asked", () => {
    expect(parseMapRefreshAnswer(null)).toBeNull();
    expect(parseMapRefreshAnswer("{oops")).toBeNull();
    expect(parseMapRefreshAnswer('{"answer":"maybe"}')).toBeNull();
    expect(mapRefreshKey("ACME/Web")).toBe("colonizer.mapRefresh.acme/web");
  });
});

describe("MapRefreshPrompt", () => {
  const render = () => renderToStaticMarkup(<MapRefreshPrompt repo="acme/web" onCreate={() => {}} onNotNow={() => {}} />);

  it("asks with 14 days preselected, both scopes and both answers", () => {
    const html = render();
    expect(html).toContain("Keep this map up to date?");
    expect(html).toMatch(/<option [^>]*value="14"[^>]*selected/);
    expect(html).toContain("custom (days)");
    expect(html).toContain("this repository");
    expect(html).toContain("all repositories in acme");
    expect(html).toContain("Create loop");
    expect(html).toContain("Not now");
  });

  it("says how often a covered map refreshes, with the way to its loop", () => {
    const html = renderToStaticMarkup(<MapRefreshCovered covered={mapLoop()} onEdit={() => {}} />);
    expect(html).toContain("Refreshed every 14 days");
    expect(html).toContain("edit");
  });
});

describe("mapLoopBody", () => {
  const now = new Date(2026, 8, 26, 5, 0);

  it("builds the create-loop body the map question posts, for both scopes", () => {
    expect(mapLoopBody("acme/web", false, 14, now)).toEqual({
      name: "Keep the map of acme/web fresh",
      repo: "acme/web",
      prompt: "",
      kind: "map",
      cadence: toUtcLoopCadence({ every: "every_days", days: 14, time: "03:00" }, now),
      tz_offset_minutes: -now.getTimezoneOffset(),
      autopilot: true,
      enabled: true,
    });
    expect(mapLoopBody("acme/web", true, 7, now)).toMatchObject({ repo: "acme/*", name: "Keep every map in acme fresh", prompt: "", kind: "map" });
  });
});
