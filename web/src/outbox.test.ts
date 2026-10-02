// The service worker's outbox (issue #746), run the way importScripts runs it: against a `self`,
// a fake store and a fake fetch — the delivery rules, the single in-flight flush, the item shapes
// the page builds, and the worker wiring that sync and the page's messages drive.
import { describe, expect, it, vi } from "vitest";

import outboxSource from "../public/sw-outbox.js?raw";
import { canQueue, commandToItem, droppedText, outboxId, type OutboxItem } from "./outbox";

interface Outbox {
  enqueue(item: OutboxItem): Promise<boolean>;
  flush(): Promise<void>;
}

interface FakeStore {
  all(): Promise<OutboxItem[]>;
  get(id: string): Promise<OutboxItem | undefined>;
  put(item: OutboxItem): Promise<void>;
  delete(id: string): Promise<boolean>;
}

/** The queue over a Map, seeded like the worker would have found it. */
function fakeStore(seed: OutboxItem[] = []): FakeStore & { items: Map<string, OutboxItem> } {
  const items = new Map<string, OutboxItem>(seed.map((item) => [item.id, item]));
  return {
    items,
    all: async () => [...items.values()],
    get: async (id) => items.get(id),
    put: async (item) => void items.set(item.id, item),
    delete: async (id) => items.delete(id),
  };
}

const ok = () => new Response(null, { status: 204 });

/** Lets promise-and-timer chains settle until `fn` holds — bounded, so a regression fails rather than hangs. */
const until = async (fn: () => boolean) => {
  for (let i = 0; i < 100 && !fn(); i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

const item = (id: string, queuedAt: number, url: string): OutboxItem => ({ id, url, body: "{}", queuedAt });

/** An answer as commandToItem builds it, the thing a phone would have queued offline. */
const answerItem = (id = "q1", queuedAt = 1): OutboxItem => ({
  id,
  url: "/api/sessions/demo1234/answer",
  body: JSON.stringify({ question_id: "w1", answers: { "Proceed?": "Yes" }, response: null, questions: [{ question: "Proceed?" }] }),
  queuedAt,
});

// The pure core: evaluated once against a bare `self` (no clients, so the wiring stays off).
type Deps = { store: FakeStore; fetch: (url: string, init?: RequestInit) => Promise<Response>; report?: (status: unknown) => Promise<void>; now?: () => number };
const core: { self: { colonizerCreateOutbox?: (deps: Deps) => Outbox } } = { self: {} };
new Function("self", "fetch", outboxSource)(core.self, async () => ok());
// The core's clock is pinned just after the items' queuedAt stamps, unless a test moves it.
const createOutbox = (deps: Deps) => core.self.colonizerCreateOutbox!({ now: () => 1_000, ...deps });
const DAY = 24 * 60 * 60 * 1000;

describe("the outbox core", () => {
  it("an answer queued while offline stays pending, and says it is waiting", async () => {
    const store = fakeStore();
    const fetchImpl = vi.fn(async () => {
      throw new TypeError("the network is down");
    });
    const report = vi.fn(async () => {});
    const outbox = createOutbox({ store, fetch: fetchImpl, report });
    expect(await outbox.enqueue(answerItem())).toBe(true);
    await outbox.flush();
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect((await store.all()).map((item) => item.id)).toEqual(["q1"]);
    expect(report).toHaveBeenLastCalledWith({ pending: 1, delivered: [], dropped: [] });
  });

  it("after reconnect, concurrent flushes deliver it exactly once, and a later flush sends nothing", async () => {
    const store = fakeStore([answerItem()]);
    const fetchImpl = vi.fn(async (_url: string, _init?: RequestInit) => ok());
    const outbox = createOutbox({ store, fetch: fetchImpl });
    await Promise.all([outbox.flush(), outbox.flush(), outbox.flush()]);
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    const [url, init] = fetchImpl.mock.calls[0];
    expect(url).toBe("/api/sessions/demo1234/answer");
    expect(init?.method).toBe("POST");
    expect(init?.headers).toEqual({ "content-type": "application/json" });
    expect(JSON.parse(String(init?.body))).toEqual(JSON.parse(answerItem().body));
    expect(await store.all()).toEqual([]);
    await outbox.flush();
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it("a 409 — the question was already answered — drops the item and reports the drop", async () => {
    const store = fakeStore([answerItem("stale")]);
    const report = vi.fn(async () => {});
    const outbox = createOutbox({ store, fetch: async () => new Response(null, { status: 409 }), report });
    await outbox.flush();
    expect(await store.all()).toEqual([]);
    expect(report).toHaveBeenLastCalledWith({ pending: 0, delivered: [], dropped: [{ id: "stale", status: 409 }] });
  });

  it("a 5xx keeps the queue and stops, so the order is kept for the next flush", async () => {
    const store = fakeStore([answerItem("first"), answerItem("second")]);
    const fetchImpl = vi.fn(async () => new Response(null, { status: 503 }));
    const outbox = createOutbox({ store, fetch: fetchImpl });
    await outbox.flush();
    expect(fetchImpl).toHaveBeenCalledTimes(1); // the second item was never attempted
    expect((await store.all()).map((item) => item.id)).toEqual(["first", "second"]);
  });

  it("an auth 401 keeps the item too — a re-sign-in may fix it — while a plain 400 is final", async () => {
    const keep = fakeStore([answerItem("authed")]);
    await createOutbox({ store: keep, fetch: async () => new Response(null, { status: 401 }) }).flush();
    expect(await keep.all()).toEqual([{ ...answerItem("authed") }]);
    const drop = fakeStore([answerItem("bad")]);
    await createOutbox({ store: drop, fetch: async () => new Response(null, { status: 400 }) }).flush();
    expect(await drop.all()).toEqual([]);
  });

  it("sends one at a time, in queue order, and enqueue is idempotent by id", async () => {
    const store = fakeStore();
    let inFlight = 0;
    let deepest = 0;
    const fetchImpl = vi.fn(async (_url: string, _init?: RequestInit) => {
      inFlight += 1;
      deepest = Math.max(deepest, inFlight);
      await new Promise((resolve) => setTimeout(resolve, 0));
      inFlight -= 1;
      return ok();
    });
    const outbox = createOutbox({ store, fetch: fetchImpl });
    expect(await outbox.enqueue(answerItem("a"))).toBe(true);
    expect(await outbox.enqueue(answerItem("a"))).toBe(false); // the same id again: kept once
    await outbox.enqueue(answerItem("b"));
    await outbox.flush();
    expect(deepest).toBe(1);
    expect(fetchImpl.mock.calls.map(([url]) => url)).toEqual(["/api/sessions/demo1234/answer", "/api/sessions/demo1234/answer"]);
    expect(await store.all()).toEqual([]);
  });

  it("a flush picks up items enqueued while it was running", async () => {
    const store = fakeStore([answerItem("first")]);
    let release!: () => void;
    const inFlight = new Promise<void>((resolve) => (release = resolve));
    const fetchImpl = vi.fn(async (_url: string, _init?: RequestInit) => {
      await inFlight; // hold the first send until the second item has been enqueued
      return ok();
    });
    const outbox = createOutbox({ store, fetch: fetchImpl });
    const flushing = outbox.flush();
    await outbox.enqueue(answerItem("second"));
    release();
    await flushing;
    expect(fetchImpl.mock.calls.map(([url]) => url)).toHaveLength(2);
    expect(await store.all()).toEqual([]);
  });

  it("an item enqueued as the flush is finishing still goes out with that same flush", async () => {
    const items = new Map<string, OutboxItem>();
    let release: ((queue: OutboxItem[]) => void) | null = null;
    let steered = 2; // the flush's first read and its finishing re-check are steered by hand
    const store: FakeStore = {
      all: async () => {
        if (steered > 0) {
          steered -= 1;
          return new Promise<OutboxItem[]>((resolve) => (release = resolve));
        }
        return [...items.values()];
      },
      get: async (id) => items.get(id),
      put: async (item) => void items.set(item.id, item),
      delete: async (id) => items.delete(id),
    };
    const fetchImpl = vi.fn(async (_url: string, _init?: RequestInit) => ok());
    const outbox = createOutbox({ store, fetch: fetchImpl });
    const flushing = outbox.flush();
    await until(() => release !== null);
    release!([]); // the loop saw an empty queue…
    release = null;
    await until(() => release !== null);
    items.set("late", answerItem("late")); // …and an enqueue landed right after that read
    release!([answerItem("late")]);
    await flushing;
    expect(fetchImpl).toHaveBeenCalledTimes(1); // sent by this flush, not stranded until the next nudge
    expect(fetchImpl.mock.calls[0][0]).toBe("/api/sessions/demo1234/answer");
    expect(items.size).toBe(0);
  });

  it("an item that waited offline over a day is dropped unsent, reported with status 0", async () => {
    const store = fakeStore([answerItem("old", 1), answerItem("fresh", DAY)]);
    const fetchImpl = vi.fn(async (_url: string, _init?: RequestInit) => ok());
    const report = vi.fn(async () => {});
    await createOutbox({ store, fetch: fetchImpl, report, now: () => DAY + 2 }).flush();
    expect(fetchImpl).toHaveBeenCalledTimes(1); // only the fresh one went out
    expect(await store.all()).toEqual([]);
    expect(report).toHaveBeenLastCalledWith({ pending: 0, delivered: ["fresh"], dropped: [{ id: "old", status: 0 }] });
  });

  it("a replayed answer that the mothership already took, or whose question changed, is dropped — never resent", async () => {
    // The server's side of both (sessions/api.rs): the first delivery closes the question, so a
    // repeat is a 409; an answer whose `questions` no longer match the open question is a 409 too.
    const store = fakeStore([answerItem("lost-reply"), answerItem("changed")]);
    const answered = new Set<string>();
    const fetchImpl = vi.fn(async (_url: string, init?: RequestInit) => {
      const body = JSON.parse(String(init?.body)) as { question_id: string; questions?: unknown };
      if (answered.has(body.question_id) || !body.questions) return new Response(null, { status: 409 });
      answered.add(body.question_id);
      return ok();
    });
    const report = vi.fn(async () => {});
    await createOutbox({ store, fetch: fetchImpl, report }).flush();
    expect(fetchImpl).toHaveBeenCalledTimes(2); // each item is sent once, and neither comes back
    expect(await store.all()).toEqual([]);
    expect(report).toHaveBeenLastCalledWith({ pending: 0, delivered: ["lost-reply"], dropped: [{ id: "changed", status: 409 }] });
  });

  it("sends in the order the items were queued, not the store's key order", async () => {
    // getAll answers in key order, and the ids are random UUIDs: "a" here is the *later* message.
    const store = fakeStore([item("a", 200, "/later"), item("b", 100, "/earlier")]);
    const fetchImpl = vi.fn(async (_url: string, _init?: RequestInit) => ok());
    await createOutbox({ store, fetch: fetchImpl }).flush();
    expect(fetchImpl.mock.calls.map(([url]) => url)).toEqual(["/earlier", "/later"]);
  });

  it("breaks a queuedAt tie by the sequence stamped at enqueue", async () => {
    const store = fakeStore();
    const fetchImpl = vi.fn(async (_url: string, _init?: RequestInit) => ok());
    const outbox = createOutbox({ store, fetch: fetchImpl });
    await outbox.enqueue(item("b", 7, "/one"));
    await outbox.enqueue(item("a", 7, "/two")); // the same millisecond; "a" sorts first by key
    const byId = (id: string) => store.items.get(id);
    expect(byId("b")?.seq).toBe(1); // the stamp says which came first…
    expect(byId("a")?.seq).toBe(2); // …since the store's key order cannot
    await outbox.flush();
    expect(fetchImpl.mock.calls.map(([url]) => url)).toEqual(["/one", "/two"]);
  });
});

describe("the page's items", () => {
  it("an answer becomes the HTTP twin of the socket command, carrying the questions it answers", () => {
    const questions = [{ question: "q", header: "Q", multi_select: false, options: [{ label: "a" }] }];
    const item = commandToItem({ type: "answer", question_id: "w1", answers: { q: "a" }, response: "go on" }, "demo1234", "q9", 5, questions);
    expect(item).toEqual({
      id: "q9",
      url: "/api/sessions/demo1234/answer",
      body: JSON.stringify({ question_id: "w1", answers: { q: "a" }, response: "go on", questions }),
      queuedAt: 5,
    });
    const bare = commandToItem({ type: "answer", question_id: "w1", answers: {}, response: null }, "demo1234", "q9", 5);
    expect(JSON.parse(bare?.body ?? "{}")).not.toHaveProperty("questions");
  });

  it("a message carries the dedupe id the endpoint asks for", () => {
    const item = commandToItem({ type: "user_message", text: "hold on a moment" }, "demo1234", "q9", 6);
    expect(item?.url).toBe("/api/sessions/demo1234/messages");
    expect(JSON.parse(item?.body ?? "{}")).toEqual({ id: "q9", text: "hold on a moment" });
  });

  it("an interrupt or a model switch must not outlive the moment", () => {
    expect(commandToItem({ type: "interrupt" }, "demo1234", "q9", 0)).toBeNull();
    expect(commandToItem({ type: "set_model", model: "claude-x" }, "demo1234", "q9", 0)).toBeNull();
  });

  it("outboxId fits the endpoint's dedupe id shape", () => {
    expect(outboxId()).toMatch(/^[A-Za-z0-9_-]{1,64}$/);
    expect(outboxId()).not.toBe(outboxId());
  });

  it("canQueue says yes only when a service-worker controller exists to take the item", () => {
    expect(canQueue({ serviceWorker: { controller: {} } })).toBe(true);
    expect(canQueue({ serviceWorker: { controller: null } })).toBe(false);
    expect(canQueue({ serviceWorker: null })).toBe(false);
    expect(canQueue({})).toBe(false);
    expect(canQueue(undefined)).toBe(false);
  });

  it("reads a 409 calmly, and differently per kind", () => {
    expect(droppedText("answer", 409)).toBe("The question was no longer open, or had changed — it may already have been answered.");
    expect(droppedText("message", 409)).toBe("The colony isn't taking messages right now, so the queued message wasn't sent.");
    expect(droppedText("message", 404)).toBe("The colony is gone.");
    expect(droppedText("answer", 400)).toBe("The mothership refused it (400).");
    expect(droppedText("message", 0)).toBe("It waited offline too long, so the queued message was not sent.");
  });
});

describe("the worker wiring", () => {
  type Listener = (event: { tag?: string; data?: unknown; waitUntil: (promise: Promise<unknown>) => void }) => void;

  /** A `self` with captured listeners, the fake store on the seam, and clients to broadcast to. */
  function wire(store: FakeStore, fetchImpl: (url: string, init?: RequestInit) => Promise<Response>) {
    const posted: unknown[] = [];
    const listeners: Record<string, Listener[]> = {};
    const self: Record<string, unknown> = {
      addEventListener: (type: unknown, handler: unknown) => void (listeners[type as string] ??= []).push(handler as Listener),
      clients: { matchAll: async () => [{ postMessage: (message: unknown) => posted.push(message) }] },
      colonizerOutboxStore: store,
    };
    new Function("self", "fetch", outboxSource)(self, fetchImpl);
    const dispatch = async (type: string, event: Record<string, unknown> = {}) => {
      const pending: Promise<unknown>[] = [];
      for (const handler of listeners[type] ?? []) handler({ waitUntil: (p) => void pending.push(p), ...event });
      await Promise.all(pending);
    };
    return { self, dispatch, posted };
  }

  it("flushes on the sync tag and on the page's messages, and ignores other tags", async () => {
    const store = fakeStore([answerItem("q1", Date.now())]);
    const fetchImpl = vi.fn(async () => ok());
    const worker = wire(store, fetchImpl);
    await worker.dispatch("sync", { tag: "someone-elses-sync" });
    expect(fetchImpl).not.toHaveBeenCalled();
    await worker.dispatch("sync", { tag: "colonizer-outbox" });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(await store.all()).toEqual([]);
  });

  it("enqueues and flushes on the page's messages, and reports progress to every tab", async () => {
    const store = fakeStore();
    const fetchImpl = vi.fn(async () => new Response(null, { status: 409 }));
    const worker = wire(store, fetchImpl);
    await worker.dispatch("message", { data: { type: "colonizer:outbox-enqueue", item: answerItem("queued", Date.now()) } });
    expect(fetchImpl).toHaveBeenCalledTimes(1); // enqueue is followed by a flush
    expect(await store.get("queued")).toBeUndefined(); // and the 409 dropped it within that same flush
    expect(worker.posted.at(-1)).toEqual({ type: "colonizer:outbox", pending: 0, delivered: [], dropped: [{ id: "queued", status: 409 }] });
    await worker.dispatch("message", { data: { type: "colonizer:outbox-flush" } });
    await worker.dispatch("message", { data: null });
    expect(fetchImpl).toHaveBeenCalledTimes(1); // the queue is empty; a stray message costs nothing
  });

  it("a message enqueue lands in the queue and waits when the network is down", async () => {
    const store = fakeStore();
    const fetchImpl = vi.fn(async () => {
      throw new TypeError("the network is down");
    });
    const worker = wire(store, fetchImpl);
    await worker.dispatch("message", { data: { type: "colonizer:outbox-enqueue", item: answerItem("waiting", Date.now()) } });
    expect(await store.get("waiting")).toMatchObject({ id: "waiting", seq: 1 }); // the worker stamped its order
    expect(worker.posted.at(-1)).toEqual({ type: "colonizer:outbox", pending: 1, delivered: [], dropped: [] });
  });
});
