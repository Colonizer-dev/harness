// Contract for the issue-#159 poll helpers: the 4s session poll must keep ticking in a
// hidden tab (worker-backed), degrade to setInterval where workers are unavailable, stop
// on cleanup, and re-run the moment the tab becomes visible again.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { refreshOnVisible, startPollInterval } from "./workerInterval";

class FakeWorker {
  static instances: FakeWorker[] = [];
  onmessage: (() => void) | null = null;
  terminated = false;
  url: string;
  constructor(url: string) {
    this.url = url;
    FakeWorker.instances.push(this);
  }
  terminate() {
    this.terminated = true;
  }
  tick() {
    this.onmessage?.();
  }
}

function stubWorkerEnv() {
  FakeWorker.instances = [];
  vi.stubGlobal("Worker", FakeWorker);
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:fake");
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
}

function stubDocument(visibilityState: Document["visibilityState"]) {
  const listeners = new Map<string, Set<() => void>>();
  const doc = {
    visibilityState,
    addEventListener: vi.fn((type: string, cb: () => void) => {
      const set = listeners.get(type) ?? new Set<() => void>();
      set.add(cb);
      listeners.set(type, set);
    }),
    removeEventListener: vi.fn((type: string, cb: () => void) => {
      listeners.get(type)?.delete(cb);
    }),
    fire(type: string) {
      listeners.get(type)?.forEach((cb) => cb());
    },
  };
  vi.stubGlobal("document", doc);
  return doc;
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("startPollInterval", () => {
  it("falls back to setInterval when Worker is unavailable", () => {
    vi.stubGlobal("Worker", undefined);
    const cb = vi.fn();
    const stop = startPollInterval(cb, 4000);
    vi.advanceTimersByTime(8000);
    expect(cb).toHaveBeenCalledTimes(2);
    stop();
    vi.advanceTimersByTime(8000);
    expect(cb).toHaveBeenCalledTimes(2);
  });

  it("ticks the callback from the worker and cleans the worker up on stop", () => {
    stubWorkerEnv();
    const cb = vi.fn();
    const stop = startPollInterval(cb, 4000);
    expect(FakeWorker.instances).toHaveLength(1);
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    FakeWorker.instances[0]?.tick();
    FakeWorker.instances[0]?.tick();
    expect(cb).toHaveBeenCalledTimes(2);
    stop();
    expect(FakeWorker.instances[0]?.terminated).toBe(true);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:fake");
    // No fallback setInterval was left behind: advancing time calls nothing more.
    vi.advanceTimersByTime(60_000);
    expect(cb).toHaveBeenCalledTimes(2);
  });

  it("falls back to setInterval when Worker construction throws", () => {
    vi.stubGlobal(
      "Worker",
      class {
        constructor() {
          throw new Error("denied");
        }
      },
    );
    const cb = vi.fn();
    const stop = startPollInterval(cb, 4000);
    vi.advanceTimersByTime(4000);
    expect(cb).toHaveBeenCalledTimes(1);
    stop();
  });
});

describe("refreshOnVisible", () => {
  it("re-runs the callback when the tab becomes visible, and not while hidden", () => {
    const doc = stubDocument("hidden");
    const cb = vi.fn();
    const stop = refreshOnVisible(cb);
    expect(doc.addEventListener).toHaveBeenCalledWith("visibilitychange", expect.any(Function));
    doc.fire("visibilitychange");
    expect(cb).not.toHaveBeenCalled();
    doc.visibilityState = "visible";
    doc.fire("visibilitychange");
    expect(cb).toHaveBeenCalledTimes(1);
    stop();
    expect(doc.removeEventListener).toHaveBeenCalledWith("visibilitychange", expect.any(Function));
  });

  it("is a noop where there is no document", () => {
    expect(() => refreshOnVisible(vi.fn())()).not.toThrow();
  });
});
