// The demo build (issue #682): the MODE=demo flag alone forces the mock api on — no `?mock=1` in
// the address bar — and driving it touches none of the browser's network primitives, so the hosted
// page at colonizer.dev/demo makes no /api calls. The flag is a build-time constant, so the tests
// re-import the modules under a stubbed env instead of going through `?mock=1`.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const PRIMITIVES = ["fetch", "WebSocket", "EventSource", "XMLHttpRequest"] as const;
let touched: string[] = [];

beforeEach(() => {
  vi.resetModules();
  // The test environment has no DOM: a bare location is enough — the mock reads only `?mesh=`,
  // `?runtime=` and `?quota=` off it, and none of those are set here.
  vi.stubGlobal("location", new URL("https://colonizer.dev/demo"));
  touched = [];
  for (const primitive of PRIMITIVES) {
    vi.stubGlobal(
      primitive,
      vi.fn(() => {
        touched.push(primitive);
        throw new Error(`${primitive} must not be reached in the demo`);
      }),
    );
  }
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
  vi.useRealTimers();
});

/** loadApi as the demo build's bundle has it: MODE=demo baked in, no query string involved. */
async function demoLoadApi() {
  vi.stubEnv("MODE", "demo");
  const { loadApi } = await import("./api");
  return loadApi();
}

describe("demo build", () => {
  it("flips the flag off again outside --mode demo, where ?mock=1 stays the only way in", async () => {
    const { DEMO } = await import("./demo");
    expect(DEMO).toBe(false);
    const { loadApi } = await import("./api");
    expect((await loadApi()).mock).toBe(false);
  });

  it("returns the mock api with no ?mock in the address bar", async () => {
    const api = await demoLoadApi();
    expect(api.mock).toBe(true);
    expect(touched).toEqual([]);
  });

  it("serves sessions, a session detail, issues and the repo map without a network primitive", async () => {
    const api = await demoLoadApi();
    const sessions = await api.sessions();
    expect(sessions.length).toBeGreaterThan(0);
    await api.session("demo1234");
    const issues = await api.issues("acme/webshop");
    expect(issues.length).toBeGreaterThan(0);
    const map = await api.repoMap("acme/webshop");
    expect(map.map).not.toBeNull();
    api.openEvents("demo1234", 0);
    api.openTerminal("demo1234", 80, 24);
    api.openStream();
    expect(touched).toEqual([]);
  });

  it("answers the demo colony's question once its intro gets to it", async () => {
    vi.useFakeTimers();
    const api = await demoLoadApi();
    const frames: Array<{ type?: string }> = [];
    const socket = api.openEvents("demo1234", 0);
    socket.onmessage = (event) => frames.push(JSON.parse(String(event.data)));
    // The mock socket opens 150 ms in; the colony's scripted intro then runs to its question.
    await vi.advanceTimersByTimeAsync(200);
    for (let i = 0; i < 60 && !frames.some((f) => f.type === "question"); i += 1) {
      await vi.advanceTimersByTimeAsync(1000);
    }
    expect(frames.some((f) => f.type === "question")).toBe(true);
    socket.send(JSON.stringify({ type: "answer", question_id: "toolu_q1", answers: {}, response: "Guest cart by email" }));
    await vi.advanceTimersByTimeAsync(600);
    expect(frames.some((f) => f.type === "question_answered")).toBe(true);
    socket.close();
    expect(touched).toEqual([]);
  });
});
