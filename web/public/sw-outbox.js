// The outbox (issue #746): commands the cockpit queues while its socket to a colony is down, sent
// the moment the mothership is reachable again. The queue lives in IndexedDB and survives the
// worker, so a phone that answered a question offline still delivers it on the next open. The core
// is pure logic over an injectable store + fetch (colonizerCreateOutbox) so the tests can pin the
// delivery rules exactly like colonizerRoute; only the store and the wiring at the foot touch the
// browser. Loaded by sw.js via importScripts, and precached with the shell.
(function () {
  /**
   * The queue in IndexedDB: one object store keyed by the item id. Every method answers a promise,
   * which is the whole surface the core asks of a store.
   */
  function openStore(name) {
    let opening = null;
    const db = () => {
      if (!opening) {
        opening = new Promise((resolve, reject) => {
          const request = indexedDB.open(name, 1);
          request.onupgradeneeded = () => request.result.createObjectStore("queue", { keyPath: "id" });
          request.onsuccess = () => resolve(request.result);
          request.onerror = () => reject(request.error || new Error("indexedDB unavailable"));
        });
      }
      return opening;
    };
    const settled = (request) =>
      new Promise((resolve, reject) => {
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
    const tx = (mode, run) => db().then((database) => settled(run(database.transaction("queue", mode).objectStore("queue"))));
    return {
      all: () => tx("readonly", (s) => s.getAll()),
      get: (id) => tx("readonly", (s) => s.get(id)),
      put: (item) => tx("readwrite", (s) => s.put(item)),
      delete: (id) => tx("readwrite", (s) => s.delete(id)),
    };
  }

  /**
   * The delivery rules, over whatever store and fetch it is given. enqueue is idempotent by id (the
   * page re-sends on a flaky button and the mothership dedupes anyway); flush sends one item at a
   * time in the order the items were queued, and concurrent calls share the single in-flight flush,
   * so nothing is ever sent twice. An ok answer removes the item (delivered); a final refusal from
   * the mothership removes it and reports it dropped; anything that says "not now" — the network, a
   * 5xx, an auth 401/403 that a re-sign-in may fix — keeps the item and stops, so the next flush
   * retries in order. A flush that drains the store looks once more before it ends, so an item
   * enqueued while it was running goes out with that same flush instead of waiting for a nudge.
   * An item older than MAX_AGE_MS is dropped unsent (reported with status 0): an answer or a
   * message a day old says something the operator may no longer mean.
   */
  const MAX_AGE_MS = 24 * 60 * 60 * 1000;
  function createOutbox({ store, fetch: send, report, now = () => Date.now() }) {
    let flushing = null;
    let sequence = 0; // ties the queuedAt stamp when two items land in the same millisecond
    const refused = (status) => status >= 400 && status < 500 && status !== 401 && status !== 403;
    // getAll answers in key order, and the ids are random UUIDs: the queue order is the stamped one.
    const inOrder = (queue) => queue.slice().sort((a, b) => a.queuedAt - b.queuedAt || (a.seq ?? 0) - (b.seq ?? 0));
    const announce = async (delivered, dropped) => {
      const pending = (await store.all()).length;
      if (report) await report({ pending, delivered, dropped });
    };
    async function runFlush() {
      const delivered = [];
      const dropped = [];
      let stopped = false; // the head item said "not now": re-reading cannot help until later
      for (;;) {
        const queue = inOrder(await store.all());
        if (queue.length === 0) {
          if (stopped || (await store.all()).length === 0) break;
          continue; // something arrived while this flush was running: send it too
        }
        const item = queue[0];
        if (now() - item.queuedAt > MAX_AGE_MS) {
          await store.delete(item.id);
          dropped.push({ id: item.id, status: 0 });
          continue;
        }
        let response;
        try {
          response = await send(item.url, { method: "POST", headers: { "content-type": "application/json" }, body: item.body });
        } catch (error) {
          stopped = true;
          break; // offline: the item stays first in line
        }
        if (response.status >= 200 && response.status < 300) {
          await store.delete(item.id);
          delivered.push(item.id);
        } else if (refused(response.status)) {
          await store.delete(item.id);
          dropped.push({ id: item.id, status: response.status });
        } else {
          stopped = true;
          break; // 5xx or 401/403: try again after the next sync, in the same order
        }
      }
      await announce(delivered, dropped);
    }
    return {
      async enqueue(item) {
        const known = await store.get(item.id);
        if (!known) await store.put({ seq: ++sequence, ...item });
        await announce([], []);
        return !known;
      },
      flush() {
        if (!flushing) flushing = runFlush().finally(() => (flushing = null));
        return flushing;
      },
    };
  }

  /**
   * The worker-side listeners: Background Sync when the browser offers it, and the page's
   * "flush now" nudge for when it does not — on load, on `online`, on returning to the tab.
   */
  function wireOutbox(target, outbox) {
    target.addEventListener("sync", (event) => {
      if (event.tag === "colonizer-outbox") event.waitUntil(outbox.flush());
    });
    target.addEventListener("message", (event) => {
      const data = event.data;
      if (!data || typeof data !== "object") return;
      if (data.type === "colonizer:outbox-enqueue" && data.item && typeof data.item.id === "string") {
        event.waitUntil(outbox.enqueue(data.item).then(() => outbox.flush()));
      } else if (data.type === "colonizer:outbox-flush") {
        event.waitUntil(outbox.flush());
      }
    });
  }

  self.colonizerCreateOutbox = createOutbox;
  self.colonizerWireOutbox = wireOutbox;

  // The worker's own instance, wired once on load. The store seam is for the tests: they run this
  // file against a `self` and hand the wiring a fake, the way they hand `fetch` in.
  if (typeof self.addEventListener === "function" && self.clients && typeof self.clients.matchAll === "function") {
    const outbox = createOutbox({
      store: self.colonizerOutboxStore || openStore("colonizer-outbox"),
      fetch: (input, init) => fetch(input, init),
      report: async (status) => {
        const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
        for (const client of windows) client.postMessage({ type: "colonizer:outbox", ...status });
      },
    });
    self.colonizerOutbox = outbox;
    wireOutbox(self, outbox);
  }
})();
