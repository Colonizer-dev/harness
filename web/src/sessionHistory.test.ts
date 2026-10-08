// Lazy-loaded colony history (issue #1210): the colony opens on its newest page, older pages are prepended on
// demand, and what the UI derives from the whole log (settlers, cost, brief) comes from the mothership's summary.
import { describe, expect, it } from "vitest";

import {
  PAGE_SIZE,
  SessionStream,
  buildThread,
  earlierTotals,
  initialStreamState,
  prependPage,
  reduceFrame,
  settlersOf,
  type StreamState,
} from "./sessionStream";
import type { Api, SocketLike } from "./api";
import type { AgentEvent, EventsPage, HistorySummary, ServerFrame } from "./types";

const TS = "2026-09-17T10:00:00Z";
const ev = (seq: number, body: Record<string, unknown>): AgentEvent => ({ seq, ts: TS, ...body }) as unknown as AgentEvent;
const agent = (id: string, name = "Explore") => ({ id, name, description: `${name} task` });

/** One turn: the operator's message, a reply (optionally by a settler), and the turn's end. */
function turn(n: number, seq: number, opts: { settler?: string } = {}): AgentEvent[] {
  const who = opts.settler ? { agent: agent(opts.settler) } : {};
  return [
    ev(seq, { type: "user_message", id: `u${n}`, text: `ask ${n}` }),
    ev(seq + 1, { type: "assistant_text", message_id: `a${n}`, block_index: 0, text: `answer ${n}`, ...who }),
    ev(seq + 2, {
      type: "turn_end",
      is_error: false,
      result: null,
      cost_usd: n * 0.5,
      duration_ms: 10,
      model_usage: { m: { input_tokens: n * 100, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, thinking_tokens: 0 } },
    }),
  ];
}

const summary = (over: Partial<HistorySummary> = {}): HistorySummary => ({
  last_seq: 300,
  events: 300,
  turns: 100,
  cost_usd: 50,
  brief: null,
  model: "opus",
  agent_state: { state: "idle", detail: null },
  settlers: [],
  ...over,
});

const historyFrame = (over: Record<string, unknown> = {}): ServerFrame =>
  ({
    type: "history",
    has_more: true,
    oldest_seq: 289,
    epoch: 3,
    offset: 9000,
    baseline_usage: { m: { input_tokens: 9600, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 } },
    summary: summary(),
    ...over,
  }) as unknown as ServerFrame;

/** A colony as the socket's first paint leaves it: the history frame, then the newest page. */
function tailOnly(frameOver: Record<string, unknown> = {}, summaryOver: Partial<HistorySummary> = {}): StreamState {
  let state = reduceFrame(initialStreamState(), historyFrame({ ...frameOver, summary: summary(summaryOver) }));
  for (const e of [...turn(97, 289), ...turn(98, 292), ...turn(99, 295), ...turn(100, 298)]) state = reduceFrame(state, e);
  return state;
}

describe("the newest page only", () => {
  it("holds what the page holds, and says older events are on record", () => {
    const state = tailOnly();
    expect(state.messages).toHaveLength(8);
    expect(state.history.hasMore).toBe(true);
    expect(state.history.cursor).toEqual({ epoch: 3, seq: 289, offset: 9000 });
    expect(state.model).toBe("opus");
    expect(state.agentState).toBe("idle");
  });

  it("names the models of the first turn on the page against the baseline before it", () => {
    const state = tailOnly();
    // 9700 total now against 9600 before the page: the model grew, so it served the turn.
    expect(state.turns[0].models).toEqual(["m"]);
    const without = reduceFrame(initialStreamState(), historyFrame({ baseline_usage: null }));
    expect(without.modelUsage).toBeNull();
  });

  it("gets the cost and the earlier turn count from the summary, not from replaying them", () => {
    const state = tailOnly();
    // 4 turns loaded of 100: 96 earlier, and the colony's cost so far is the latest turn's.
    expect(earlierTotals(state)).toEqual({ turns: 96, costUsd: 50 });
  });

  it("does not count a live turn that came after the summary as an earlier one", () => {
    let state = tailOnly();
    for (const e of turn(101, 301)) state = reduceFrame(state, e);
    expect(earlierTotals(state).turns).toBe(96);
    expect(earlierTotals(state).costUsd).toBe(50.5);
  });

  it("numbers settlers over the whole run and lists the ones the loaded page never mentions", () => {
    const settlers = [
      { id: "t1", name: "Explore", description: "Explore task", steps: 7, errors: 2, last_tool: "Grep" },
      { id: "t2", name: "Explore", description: "Explore task", steps: 3, errors: 0, last_tool: "Read" },
    ];
    let state = reduceFrame(initialStreamState(), historyFrame({ summary: summary({ settlers }) }));
    // Only the second Explore settler spoke on the loaded page.
    for (const e of turn(100, 298, { settler: "t2" })) state = reduceFrame(state, e);
    const thread = buildThread(state);
    expect(thread.settlerNames).toEqual({ t1: "Scout Settler", t2: "Scout Settler 2" });
    const all = settlersOf(state);
    expect(all.map((s) => [s.agent.id, s.name, s.steps, s.errors])).toEqual([
      ["t1", "Scout Settler", 7, 2],
      ["t2", "Scout Settler 2", 0, 0],
    ]);
    expect(all[0].state).toBe("done");
    expect(all[0].last).toEqual({ name: "Grep", input: {} });
  });

  it("opens the thread with the brief while the messages after it are still unloaded", () => {
    const brief = { seq: 1, ts: TS, type: "user_message", id: "initial", text: "Fix the checkout." } as const;
    const state = tailOnly({}, { brief });
    const thread = buildThread(state);
    expect(thread.messages[0].id).toBe("initial");
    expect(thread.messages[0].role).toBe("user");
    expect(thread.messages).toHaveLength(9);
    // Once the whole log is loaded the brief is the log's own message, not a second one.
    const full = { ...state, history: { ...state.history, hasMore: false } };
    expect(buildThread(full).messages.map((m) => m.id)).not.toContain("initial");
  });

  it("is complete when nothing is behind the page: no brief injected, all settlers from the cards", () => {
    let state = reduceFrame(initialStreamState(), historyFrame({ has_more: false, summary: summary({ settlers: [{ id: "t1", name: "Explore", description: null, steps: 1, errors: 0, last_tool: null }] }) }));
    for (const e of turn(1, 1, { settler: "t1" })) state = reduceFrame(state, e);
    expect(settlersOf(state)).toHaveLength(1);
    expect(settlersOf(state)[0].steps).toBe(0);
  });
});

describe("prepending an older page", () => {
  const olderPage = (over: Partial<EventsPage> = {}): EventsPage => ({
    events: [...turn(95, 283), ...turn(96, 286)],
    has_more: true,
    oldest_seq: 283,
    epoch: 3,
    offset: 7000,
    baseline_usage: null,
    run_epoch: 3,
    ...over,
  });

  it("puts the page's messages, turns and cursor ahead of what is loaded", () => {
    const next = prependPage(tailOnly(), olderPage());
    expect(next.messages.map((m) => m.id)).toEqual(["u95", "a95", "u96", "a96", "u97", "a97", "u98", "a98", "u99", "a99", "u100", "a100"]);
    expect(next.turns.map((t) => t.costUsd)).toEqual([47.5, 48, 48.5, 49, 49.5, 50]);
    expect(next.history).toMatchObject({ hasMore: true, cursor: { epoch: 3, seq: 283, offset: 7000 }, loaded: 1, loading: false });
    // The live run's watermark and state are untouched.
    expect(next.lastSeq).toBe(tailOnly().lastSeq);
    expect(next.agentState).toBe("idle");
  });

  it("ends the walk when the page says nothing is older", () => {
    const next = prependPage(tailOnly(), olderPage({ has_more: false }));
    expect(next.history.hasMore).toBe(false);
  });

  it("joins an assistant message the page boundary cut in two", () => {
    const base = reduceFrame(
      reduceFrame(initialStreamState(), ev(10, { type: "assistant_text", message_id: "m", block_index: 1, text: "second half" })),
      historyFrame(),
    );
    const next = prependPage(
      base,
      olderPage({
        events: [ev(8, { type: "assistant_text", message_id: "m", block_index: 0, text: "first half" })],
      }),
    );
    expect(next.messages).toHaveLength(1);
    expect(next.messages[0].blocks.map((b) => (b.kind === "text" ? b.text : ""))).toEqual(["first half", "second half"]);
  });

  it("reads a page that reaches back into an earlier run, whose seqs start over", () => {
    const next = prependPage(
      tailOnly(),
      olderPage({
        events: [...turn(1, 40), ...turn(2, 1)], // the end of run 1, then the start of run 2
        epoch: 1,
      }),
    );
    expect(next.messages.slice(0, 4).map((m) => m.id)).toEqual(["u1", "a1", "u2", "a2"]);
  });

  it("does not show a message twice when an earlier run repeats its id", () => {
    const loaded = tailOnly();
    const dup = olderPage({ events: [ev(1, { type: "user_message", id: "u97", text: "same id" })] });
    expect(prependPage(loaded, dup).messages.filter((m) => m.id === "u97")).toHaveLength(1);
  });

  it("does not leave an older, unanswered question waiting on the operator", () => {
    const asked = ev(5, { type: "question", question_id: "q1", message_id: "mq", questions: [] });
    const next = prependPage(tailOnly(), olderPage({ events: [asked] }));
    const block = next.messages[0].blocks[0];
    expect(block.kind === "question" && block.answer).toBeTruthy();
  });
});

describe("SessionStream paging", () => {
  function setup(pages: EventsPage[]) {
    const opened: { since: number; epoch: number | undefined; limit: number | undefined }[] = [];
    const asked: unknown[] = [];
    let socket: { onmessage: ((e: { data: string }) => void) | null } | null = null;
    const api = {
      openEvents: (_id: string, since: number, epoch?: number, limit?: number): SocketLike => {
        opened.push({ since, epoch, limit });
        socket = { binaryType: "blob", readyState: 1, onopen: null, onmessage: null, onclose: null, onerror: null, send: () => {}, close: () => {} } as never;
        return socket as unknown as SocketLike;
      },
      eventsPage: async (_id: string, page: unknown) => {
        asked.push(page);
        const next = pages.shift();
        if (!next) throw new Error("no more pages");
        return next;
      },
    } as unknown as Api;
    const stream = new SessionStream(api, "s1");
    stream.start();
    const deliver = (frame: unknown) => socket?.onmessage?.({ data: JSON.stringify(frame) });
    return { stream, opened, asked, deliver };
  }

  const page = (events: AgentEvent[], over: Partial<EventsPage> = {}): EventsPage => ({
    events,
    has_more: true,
    oldest_seq: events[0].seq ?? 0,
    epoch: 3,
    offset: 100,
    baseline_usage: null,
    run_epoch: 3,
    ...over,
  });

  it("asks the socket for the newest page only", () => {
    const { opened } = setup([]);
    expect(opened).toEqual([{ since: 0, epoch: 0, limit: PAGE_SIZE }]);
  });

  it("pages back from the cursor the history frame named, one page at a time", async () => {
    const { stream, asked, deliver } = setup([page(turn(1, 4), { has_more: false, oldest_seq: 4, offset: 0 })]);
    deliver(historyFrame({ oldest_seq: 7, offset: 321 }));
    for (const e of turn(2, 7)) deliver(e);
    deliver({ type: "replay_done", seq: 9 });
    expect(stream.getState().messages.map((m) => m.id)).toEqual(["u2", "a2"]);
    const first = stream.loadOlder();
    const second = stream.loadOlder(); // while the first is running: refused
    expect(await second).toBe(false);
    expect(await first).toBe(true);
    expect(asked).toEqual([{ before: 7, epoch: 3, offset: 321, limit: PAGE_SIZE }]);
    expect(stream.getState().messages.map((m) => m.id)).toEqual(["u1", "a1", "u2", "a2"]);
    expect(stream.getState().history.hasMore).toBe(false);
    expect(await stream.loadOlder()).toBe(false); // nothing older: no request
    expect(asked).toHaveLength(1);
  });

  it("loads every older page for Jump to start", async () => {
    const { stream, deliver } = setup([
      page(turn(2, 4), { oldest_seq: 4, offset: 50 }),
      page(turn(1, 1), { has_more: false, oldest_seq: 1, offset: 0 }),
    ]);
    deliver(historyFrame({ oldest_seq: 7, offset: 321 }));
    for (const e of turn(3, 7)) deliver(e);
    await stream.loadAllOlder();
    expect(stream.getState().messages.map((m) => m.id)).toEqual(["u1", "a1", "u2", "a2", "u3", "a3"]);
    expect(stream.getState().history.loaded).toBe(2);
  });

  it("says why a page failed, keeps what is loaded, and lets the reader retry", async () => {
    const { stream, deliver } = setup([]);
    deliver(historyFrame());
    for (const e of turn(97, 289)) deliver(e);
    expect(await stream.loadOlder()).toBe(false);
    expect(stream.getState().history).toMatchObject({ loading: false, error: "no more pages", hasMore: true });
    expect(stream.getState().messages).toHaveLength(2);
  });

  it("keeps live events streaming at the bottom while older pages load", async () => {
    const { stream, deliver } = setup([page(turn(1, 4), { has_more: false, oldest_seq: 4, offset: 0 })]);
    deliver(historyFrame({ oldest_seq: 7, offset: 321 }));
    for (const e of turn(2, 7)) deliver(e);
    deliver({ type: "replay_done", seq: 9 });
    const loading = stream.loadOlder();
    for (const e of turn(3, 10)) deliver(e);
    await loading;
    expect(stream.getState().messages.map((m) => m.id)).toEqual(["u1", "a1", "u2", "a2", "u3", "a3"]);
    expect(stream.getState().lastSeq).toBe(12);
  });
});
