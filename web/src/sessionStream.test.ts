// The session stream's reducer and thread builder: what a colony's chat looks like as protocol frames arrive.
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  END_OF_THREAD,
  WATCHDOG_PREFIX,
  SessionStream,
  buildThread,
  initialStreamState,
  isWatchdogMessageId,
  reduceFrame,
  subagentState,
  type Block,
  type ChatMessage,
  type QuestionBlock,
  type StreamState,
  type TextBlock,
  type ThinkingBlock,
  type ToolBlock,
  type TurnSummary,
} from "./sessionStream";
import type { AgentEventBody, AgentRef, MemoryProposal, Origin, ServerFrame, Session } from "./types";
import type { Api, SocketLike } from "./api";

const SENT_AT = "2026-09-17T10:00:00Z";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

let seq = 0;

/** The next agent event on the wire; pass `n` to replay one the colony has already seen. */
const event = (body: AgentEventBody, n?: number): ServerFrame => ({ seq: n ?? ++seq, ts: SENT_AT, ...body });

/** The same event carrying the envelope's `origin` (issue #312). */
const withOrigin = (origin: Origin, body: AgentEventBody, n?: number): ServerFrame => ({
  seq: n ?? ++seq,
  ts: SENT_AT,
  origin,
  ...body,
});

const colony = (): StreamState => initialStreamState();
const send = (state: StreamState, frame: ServerFrame): StreamState => reduceFrame(state, frame);

/**
 * A duplicate arriving with a fresh seq is still consumed — `lastSeq` must move past it or a
 * reconnect would replay it and everything after — so the reducer clones the state instead of
 * returning it as-is. Assert on what the user sees, then, not on identity; only a stale frame
 * (`seq <= lastSeq`, ignored outright) hands back the very same object.
 */
const expectNoVisibleChange = (before: StreamState, after: StreamState): void => {
  expect({ ...after, lastSeq: before.lastSeq }).toEqual(before);
  expect(after.lastSeq).toBeGreaterThan(before.lastSeq);
};

const text = (t: string, streaming = false): TextBlock => ({ kind: "text", index: 0, text: t, streaming });
const thinking = (t: string): ThinkingBlock => ({ kind: "thinking", index: 0, text: t });
const tool = (id: string, opts: { output?: string | null; isError?: boolean; name?: string } = {}): ToolBlock => ({
  kind: "tool",
  id,
  name: opts.name ?? "Bash",
  input: { command: "ls" },
  output: opts.output ?? null,
  isError: opts.isError ?? false,
});
const questionBlock = (id: string, answered = false): QuestionBlock => ({
  kind: "question",
  id,
  questions: [{ question: "Push now?", header: "Push", multi_select: false, options: [{ label: "Yes" }] }],
  answer: answered ? { answers: { "Push now?": "Yes" }, response: null } : null,
  asked_at: null,
});
const proposal = (id: string): MemoryProposal => ({
  id,
  scope: "repo",
  key: "",
  title: "Use --locked",
  content: "Run cargo with --locked.",
  tags: [],
  created_at: SENT_AT,
  status: "pending",
  source: { session_id: "s1", repo: "acme/colonizer" },
});

const userSaid = (id: string, t: string): ChatMessage => ({ id, role: "user", blocks: [text(t)], ts: SENT_AT, pending: false });
// Sent optimistically by SessionStream.send before the agent has echoed it.
const optimistic = (id: string, t: string): ChatMessage => ({ id, role: "user", blocks: [text(t)], ts: null, pending: true });
const orchestrator = (id: string, ...blocks: Block[]): ChatMessage => ({ id, role: "assistant", blocks, ts: SENT_AT, pending: false });
const settlerSaid = (id: string, agent: AgentRef, ...blocks: Block[]): ChatMessage => ({
  id,
  role: "assistant",
  agent,
  blocks,
  ts: SENT_AT,
  pending: false,
});

const scout = (id: string): AgentRef => ({ id, name: "explore" });
const builder = (id: string): AgentRef => ({ id, name: "general-purpose" });
// The runner falls back to the task description when the Task call named no type.
const untyped = (id: string, task: string): AgentRef => ({ id, name: task, description: task });

const turn = (afterMessageId: string | null, result = "ok"): TurnSummary => ({
  afterMessageId,
  isError: false,
  result,
  costUsd: 0,
  durationMs: 1,
  models: [],
  ts: SENT_AT,
});

// ---------------------------------------------------------------------------
// isWatchdogMessageId
// ---------------------------------------------------------------------------

describe("isWatchdogMessageId", () => {
  it("spots the watchdog's nudges by their id prefix", () => {
    expect(isWatchdogMessageId(`${WATCHDOG_PREFIX}turn-3`)).toBe(true);
    expect(isWatchdogMessageId("u-1")).toBe(false);
    expect(isWatchdogMessageId(null)).toBe(false);
    expect(isWatchdogMessageId(undefined)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// subagentState
// ---------------------------------------------------------------------------

describe("subagentState", () => {
  it("an empty card is a settler still thinking", () => {
    expect(subagentState([])).toEqual({
      state: "thinking",
      current: null,
      last: null,
      steps: 0,
      tools: [],
      errors: 0,
      report: "",
    });
  });

  it("a tool with no output yet means working, on the newest tool still running", () => {
    const view = subagentState([tool("t1", { output: "listed 3 files" }), tool("t2", { name: "Grep" })]);
    expect(view.state).toBe("working");
    expect(view.current).toEqual({ name: "Grep", input: { command: "ls" } });
    expect(view.last).toEqual({ name: "Grep", input: { command: "ls" } });
  });

  it("between tools, with nothing said since, the settler is thinking", () => {
    const view = subagentState([tool("t1", { output: "listed 3 files" })]);
    expect(view.state).toBe("thinking");
    expect(view.current).toBeNull();
    expect(view.last).toEqual({ name: "Bash", input: { command: "ls" } });
    expect(view.report).toBe("");
  });

  it("a card of nothing but thinking is still thinking", () => {
    const view = subagentState([thinking("Where to dig?")]);
    expect(view.state).toBe("thinking");
    expect(view.report).toBe("");
  });

  it("words said before the first tool are not the report", () => {
    const view = subagentState([text("Let me look at the layout first."), tool("t1", { output: "ok" })]);
    expect(view.state).toBe("thinking");
    expect(view.report).toBe("");
  });

  it("text still streaming after the tools means the report is being written", () => {
    const view = subagentState([tool("t1", { output: "ok" }), text("All quiet on the", true)]);
    expect(view.state).toBe("writing");
    expect(view.report).toBe("All quiet on the");
  });

  it("text that has stopped streaming after the tools is the finished report", () => {
    const view = subagentState([tool("t1", { output: "ok" }), text("All quiet.")]);
    expect(view.state).toBe("done");
    expect(view.report).toBe("All quiet.");
  });

  it("the report trims each stretch of text and joins what is left", () => {
    const view = subagentState([
      tool("t1", { output: "ok" }),
      text("  First paragraph.  "),
      text(""),
      text("Second paragraph."),
    ]);
    expect(view.report).toBe("First paragraph.\n\nSecond paragraph.");
  });

  it("counts the steps and the stumbles", () => {
    const view = subagentState([
      tool("t1", { output: "ok" }),
      tool("t2", { output: "nope", isError: true }),
      tool("t3", { name: "Grep" }),
    ]);
    expect(view.steps).toBe(3);
    expect(view.errors).toBe(1);
    expect(view.tools).toEqual([
      { name: "Bash", input: { command: "ls" }, running: false, failed: false },
      { name: "Bash", input: { command: "ls" }, running: false, failed: true },
      { name: "Grep", input: { command: "ls" }, running: true, failed: false },
    ]);
  });
});

// ---------------------------------------------------------------------------
// reduceFrame
// ---------------------------------------------------------------------------

describe("reduceFrame", () => {
  describe("sequence gating", () => {
    it("drops a frame the colony has already seen, returning the very same state", () => {
      let s = send(colony(), event({ type: "status", state: "working" }, 7));
      expect(s.lastSeq).toBe(7);
      expect(send(s, event({ type: "status", state: "idle" }, 7))).toBe(s);
      expect(send(s, event({ type: "status", state: "idle" }, 3))).toBe(s);
    });

    it("drops a frame that arrives after a newer one, rather than rewinding", () => {
      let s = send(colony(), event({ type: "status", state: "working" }, 9));
      const late = send(s, event({ type: "status", state: "idle" }, 8));
      expect(late).toBe(s);
      expect(late.agentState).toBe("working");
    });

    it("does not gate the frames that carry no sequence", () => {
      const s = send(colony(), { type: "harness_log", level: "info", message: "harness up", ts: SENT_AT });
      expect(s.logs).toHaveLength(1);
    });
  });

  it("the status frame says what the colony is doing, and why", () => {
    let s = send(colony(), event({ type: "status", state: "working", detail: "Reading src/" }));
    expect(s.agentState).toBe("working");
    expect(s.agentDetail).toBe("Reading src/");
    s = send(s, event({ type: "status", state: "idle" }));
    expect(s.agentState).toBe("idle");
    expect(s.agentDetail).toBeNull();
  });

  describe("assistant text", () => {
    it("streams deltas into one block until the final text lands", () => {
      let s = send(colony(), event({ type: "assistant_text_delta", message_id: "m1", block_index: 0, delta: "Hello" }));
      s = send(s, event({ type: "assistant_text_delta", message_id: "m1", block_index: 0, delta: ", colony." }));
      expect(s.messages).toHaveLength(1);
      expect(s.messages[0].blocks).toEqual([text("Hello, colony.", true)]);

      s = send(s, event({ type: "assistant_text", message_id: "m1", block_index: 0, text: "Hello, colony!" }));
      expect(s.messages[0].blocks).toEqual([text("Hello, colony!")]);
    });

    it("a delta that arrives after the final text is dropped", () => {
      let s = send(colony(), event({ type: "assistant_text", message_id: "m1", block_index: 0, text: "Done." }));
      const late = send(s, event({ type: "assistant_text_delta", message_id: "m1", block_index: 0, delta: "more" }));
      expectNoVisibleChange(s, late);
    });

    it("keeps the messages their events name apart", () => {
      let s = send(colony(), event({ type: "assistant_text", message_id: "m1", block_index: 0, text: "First." }));
      s = send(s, event({ type: "assistant_text", message_id: "m2", block_index: 0, text: "Second." }));
      expect(s.messages.map((m) => m.id)).toEqual(["m1", "m2"]);
    });
  });

  describe("tool calls and results", () => {
    it("pairs each result with its call, searching back through the newest messages first", () => {
      let s = send(colony(), event({ type: "tool_call", message_id: "m1", tool_call_id: "t1", name: "Bash", input: { command: "ls" } }));
      s = send(s, event({ type: "tool_call", message_id: "m2", tool_call_id: "t2", name: "Grep", input: { pattern: "todo" } }));
      s = send(s, event({ type: "tool_result", tool_call_id: "t1", output: "listed 3 files", is_error: false }));
      expect(s.messages.map((m) => m.id)).toEqual(["m1", "m2"]);
      expect(s.messages[0].blocks[0]).toMatchObject({ id: "t1", output: "listed 3 files", isError: false });
      expect(s.messages[1].blocks[0]).toMatchObject({ id: "t2", output: null, isError: false });

      s = send(s, event({ type: "tool_result", tool_call_id: "t2", output: "boom", is_error: true }));
      expect(s.messages[1].blocks[0]).toMatchObject({ id: "t2", output: "boom", isError: true });
    });

    it("a replayed tool call opens no second step", () => {
      let s = send(colony(), event({ type: "tool_call", message_id: "m1", tool_call_id: "t1", name: "Bash", input: {} }));
      const replay = send(s, event({ type: "tool_call", message_id: "m1", tool_call_id: "t1", name: "Bash", input: {} }));
      expectNoVisibleChange(s, replay);
    });

    it("a result for a call the colony never saw is dropped", () => {
      const s = send(colony(), event({ type: "tool_result", tool_call_id: "t404", output: "ok", is_error: false }));
      expect(s.messages).toEqual([]);
    });
  });

  describe("questions and answers", () => {
    const ask = { question: "Push now?", header: "Push", multi_select: false, options: [{ label: "Yes" }] };

    it("a question opens on the message that asked it, carrying its timestamp, and its answer closes it", () => {
      let s = send(colony(), event({ type: "assistant_text", message_id: "m1", block_index: 0, text: "Shall I?" }));
      s = send(s, event({ type: "question", question_id: "q1", message_id: "m1", questions: [ask] }));
      expect(s.messages[0].blocks[1]).toMatchObject({ kind: "question", id: "q1", answer: null, asked_at: SENT_AT });

      s = send(s, event({ type: "question_answered", question_id: "q1", answers: { "Push now?": "Yes" }, response: "Go ahead" }));
      expect(s.messages[0].blocks[1]).toMatchObject({ answer: { answers: { "Push now?": "Yes" }, response: "Go ahead" } });
    });

    it("a question with no message of its own lands on the last assistant bubble", () => {
      let s = send(colony(), event({ type: "assistant_text", message_id: "m1", block_index: 0, text: "Work done." }));
      s = send(s, event({ type: "question", question_id: "q1", questions: [ask] }));
      expect(s.messages).toHaveLength(1);
      expect(s.messages[0].blocks[1]).toMatchObject({ kind: "question", id: "q1" });
    });

    it("a question asked right after the user gets a bubble of its own", () => {
      let s = send(colony(), event({ type: "user_message", id: "u1", text: "Hi" }));
      s = send(s, event({ type: "question", question_id: "q1", questions: [ask] }));
      expect(s.messages).toHaveLength(2);
      expect(s.messages[1].id).toBe("q-q1");
      expect(s.messages[1].blocks[0]).toMatchObject({ kind: "question", id: "q1" });
    });

    it("the same question twice is asked once", () => {
      let s = send(colony(), event({ type: "question", question_id: "q1", questions: [ask] }));
      expectNoVisibleChange(s, send(s, event({ type: "question", question_id: "q1", questions: [ask] })));
    });

    it("an answer to a question the thread never showed still clears the submit flag", () => {
      const s = send({ ...colony(), submitting: { q9: true } }, event({ type: "question_answered", question_id: "q9", answers: {} }));
      expect(s.submitting).toEqual({});
      expect(s.messages).toEqual([]);
    });

    it("an answer keeps its origin, so the judge's answer never renders as the operator's (issue #312)", () => {
      let s = send(colony(), event({ type: "question", question_id: "q1", questions: [ask] }));
      s = send(s, withOrigin("autonomy", { type: "question_answered", question_id: "q1", answers: { "Push now?": "Yes" } }));
      expect((s.messages[0].blocks[0] as QuestionBlock).answer?.origin).toBe("autonomy");

      // A line recorded before the envelope carried no origin: the operator answered, as it always read.
      s = send(colony(), event({ type: "question", question_id: "q1", questions: [ask] }));
      s = send(s, event({ type: "question_answered", question_id: "q1", answers: { "Push now?": "Yes" } }));
      expect((s.messages[0].blocks[0] as QuestionBlock).answer?.origin).toBeUndefined();
    });
  });

  describe("user messages", () => {
    it("an optimistic send adopts the id of the echo that matches its text", () => {
      let s = { ...colony(), messages: [optimistic("local-1", "Run the tests")] };
      s = send(s, event({ type: "user_message", id: "u-9", text: "Run the tests" }));
      expect(s.messages).toHaveLength(1);
      expect(s.messages[0]).toMatchObject({ id: "u-9", pending: false, ts: SENT_AT });
    });

    it("an echo of different words is a new message, and the optimistic one still waits", () => {
      let s = { ...colony(), messages: [optimistic("local-1", "Run the tests")] };
      s = send(s, event({ type: "user_message", id: "u-9", text: "Run the tests now" }));
      expect(s.messages).toHaveLength(2);
      expect(s.messages[0]).toMatchObject({ id: "local-1", pending: true });
      expect(s.messages[1]).toMatchObject({ id: "u-9", pending: false });
    });

    it("an echo that already arrived is not added twice", () => {
      let s = send(colony(), event({ type: "user_message", id: "u-1", text: "Hi" }));
      expectNoVisibleChange(s, send(s, event({ type: "user_message", id: "u-1", text: "Hi" })));
    });

    it("a message's origin rides along, so a nudge can be told from the operator's words (issue #312)", () => {
      const s = send(colony(), withOrigin("watchdog", { type: "user_message", id: "wd-1", text: "Still there?" }));
      expect(s.messages[0]).toMatchObject({ id: "wd-1", origin: "watchdog" });

      const plain = send(colony(), event({ type: "user_message", id: "u-1", text: "Hi" }));
      expect((plain.messages[0] as ChatMessage).origin).toBeUndefined();
    });
  });

  describe("turn_end", () => {
    it("hangs a summary after the last thing said", () => {
      let s = send(colony(), event({ type: "assistant_text", message_id: "m1", block_index: 0, text: "Done." }));
      s = send(s, event({ type: "turn_end", is_error: false, result: "3 files changed", cost_usd: 0.42, duration_ms: 1200 }));
      expect(s.turns).toEqual([
        { afterMessageId: "m1", isError: false, result: "3 files changed", costUsd: 0.42, durationMs: 1200, models: [], ts: SENT_AT },
      ]);
    });

    it("anchors the summary to the user's words when the turn ended right after them", () => {
      let s = send(colony(), event({ type: "user_message", id: "u1", text: "Hi" }));
      s = send(s, event({ type: "turn_end", is_error: true, result: null, cost_usd: null, duration_ms: null }));
      expect(s.turns[0]).toMatchObject({ afterMessageId: "u1", isError: true, result: null });
    });

    it("an empty colony has nothing to anchor the summary to", () => {
      const s = send(colony(), event({ type: "turn_end", is_error: false, result: null, cost_usd: null, duration_ms: null }));
      expect(s.turns[0].afterMessageId).toBeNull();
    });

    /** One model's cumulative usage; the cache halves and thinking tokens stay zero here. */
    const tokens = (input: number, output: number) => ({
      input_tokens: input,
      output_tokens: output,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
      thinking_tokens: 0,
    });

    it("names the models that served this turn, the biggest share first", () => {
      // `model_usage` is the colony's cumulative total, so a turn's own models
      // are the ones whose total grew since the previous `turn_end`.
      let s = send(
        colony(),
        event({
          type: "turn_end",
          is_error: false,
          result: null,
          cost_usd: null,
          duration_ms: null,
          model_usage: { "claude-opus-5": tokens(10, 10) },
        }),
      );
      expect(s.turns[0].models).toEqual(["claude-opus-5"]);

      s = send(
        s,
        event({
          type: "turn_end",
          is_error: false,
          result: null,
          cost_usd: null,
          duration_ms: null,
          model_usage: {
            // Unchanged since the turn before: it served nothing this turn.
            "claude-opus-5": tokens(10, 10),
            "claude-haiku-4-5": tokens(5, 5),
            "grok-4": tokens(40, 40),
          },
        }),
      );
      expect(s.turns[1].models).toEqual(["grok-4", "claude-haiku-4-5"]);
    });

    it("a turn that spent nothing new names no model", () => {
      const usage = { "claude-opus-5": tokens(7, 7) };
      let s = send(
        colony(),
        event({ type: "turn_end", is_error: false, result: null, cost_usd: null, duration_ms: null, model_usage: usage }),
      );
      s = send(
        s,
        event({ type: "turn_end", is_error: false, result: null, cost_usd: null, duration_ms: null, model_usage: usage }),
      );
      expect(s.turns[1].models).toEqual([]);
    });

    it("ending the turn finishes text that was still streaming", () => {
      let s = send(colony(), event({ type: "assistant_text_delta", message_id: "m1", block_index: 0, delta: "Still writ" }));
      expect(s.messages[0].blocks[0]).toMatchObject({ streaming: true });
      s = send(s, event({ type: "turn_end", is_error: false, result: null, cost_usd: null, duration_ms: null }));
      expect(s.messages[0].blocks[0]).toMatchObject({ streaming: false, text: "Still writ" });
    });
  });

  describe("logs", () => {
    it("keeps harness log replays from piling up", () => {
      // The harness replays recent logs on every reconnect.
      let s = send(colony(), { type: "harness_log", level: "warn", message: "slow disk", ts: SENT_AT });
      expect(send(s, { type: "harness_log", level: "warn", message: "slow disk", ts: SENT_AT })).toBe(s);
      // The same words at a later time are a fresh line, not a replay.
      const later = send(s, { type: "harness_log", level: "warn", message: "slow disk", ts: "2026-09-17T11:00:00Z" });
      expect(later.logs).toHaveLength(2);
    });

    it("keeps only the newest 400 log lines", () => {
      let s = colony();
      for (let i = 1; i <= 410; i++) {
        s = send(s, { type: "harness_log", level: "info", message: `line ${i}`, ts: SENT_AT });
      }
      expect(s.logs).toHaveLength(400);
      expect(s.logs[0].message).toBe("line 11");
      expect(s.logs[399].message).toBe("line 410");
    });

    it("files agent logs next to the harness's", () => {
      const s = send(colony(), event({ type: "log", level: "error", message: "tool failed" }));
      expect(s.logs).toEqual([{ source: "agent", level: "error", message: "tool failed", ts: SENT_AT }]);
    });
  });

  describe("model_changed (issue #240)", () => {
    it("the latest report is the colony's model, and it ends a switch in flight", () => {
      let s = send(colony(), event({ type: "model_changed", model: "claude-opus-5", previous: null }));
      expect(s.model).toBe("claude-opus-5");
      s = send({ ...s, switchingModel: "sonnet" }, event({ type: "model_changed", model: "sonnet", previous: "claude-opus-5" }));
      expect(s).toMatchObject({ model: "sonnet", switchingModel: null });
      expect(s.messages).toEqual([]);
    });

    it("a warning ends the wait for a switch that failed, and the model stays", () => {
      let s = send(colony(), event({ type: "model_changed", model: "claude-opus-5", previous: null }));
      s = send({ ...s, switchingModel: "nope" }, event({ type: "log", level: "info", message: "still working" }));
      expect(s.switchingModel).toBe("nope");
      s = send(s, event({ type: "log", level: "warn", message: "set_model failed" }));
      expect(s).toMatchObject({ model: "claude-opus-5", switchingModel: null, refusedModel: "nope" });
      s = send(s, event({ type: "model_changed", model: "sonnet", previous: "claude-opus-5" }));
      expect(s.refusedModel).toBeNull();
    });

    it("a colony that stops being live ends the wait, as the harness drops the command", () => {
      let s = send({ ...colony(), switchingModel: "sonnet" }, { type: "session", session: { status: "idle" } as Session });
      expect(s.switchingModel).toBe("sonnet");
      s = send(s, { type: "session", session: { status: "publishing" } as Session });
      expect(s).toMatchObject({ switchingModel: null, refusedModel: null });
    });
  });

  describe("memory proposals", () => {
    it("files a proposal after the latest message", () => {
      let s = send(colony(), event({ type: "assistant_text", message_id: "m1", block_index: 0, text: "Noted." }));
      s = send(s, { type: "memory_proposed", proposal: proposal("p1") });
      expect(s.memoryNotices).toEqual([{ proposal: proposal("p1"), afterMessageId: "m1" }]);
    });

    it("a proposal with nothing said yet waits at the very end", () => {
      const s = send(colony(), { type: "memory_proposed", proposal: proposal("p1") });
      expect(s.memoryNotices[0].afterMessageId).toBeNull();
    });

    it("the same proposal twice is one notice, and one without an id is none at all", () => {
      let s = send(colony(), { type: "memory_proposed", proposal: proposal("p1") });
      expect(send(s, { type: "memory_proposed", proposal: proposal("p1") })).toBe(s);
      expect(send(s, { type: "memory_proposed", proposal: { ...proposal(""), title: "No id" } })).toBe(s);
      expect(s.memoryNotices).toHaveLength(1);
    });
  });

  describe("verification (issue #328)", () => {
    const verdict = (body: Partial<Extract<AgentEventBody, { type: "verification" }>>) =>
      event({
        type: "verification",
        verdict: "confirmed",
        by_declaration: false,
        summary: "`npm test` green in a fresh checkout",
        contradictions: [],
        command: "npm test",
        command_source: "package.json",
        exit_code: 0,
        tests_ms: 8100,
        commits: 2,
        files_changed: ["src/scan.rs"],
        snapshot: "deadbeef",
        ms: 12345,
        ...body,
      });

    it("is one harness line in the activity log, info when the claim held up", () => {
      const s = send(colony(), verdict({}));
      expect(s.logs).toEqual([{ source: "harness", level: "info", message: "verification: CONFIRMED — `npm test` green in a fresh checkout (12.3s)", ts: SENT_AT }]);
    });

    it("warns when the claim was contradicted, and says why plainly", () => {
      const s = send(colony(), verdict({ verdict: "contradicted", summary: "`npm test` exited 1 in a fresh checkout", contradictions: ["described `x.rs` is not on the branch"], exit_code: 1, ms: 41234 }));
      expect(s.logs[0]).toMatchObject({ source: "harness", level: "warn", message: "verification: CONTRADICTED — `npm test` exited 1 in a fresh checkout; described `x.rs` is not on the branch (41.2s)" });
    });

    it("names each contradiction once, and adds advisories as notes without changing the verdict", () => {
      const once = send(
        colony(),
        verdict({ verdict: "contradicted", summary: "contradicted: described `x.rs` is not on the branch, and it is the only file the description names", contradictions: ["described `x.rs` is not on the branch, and it is the only file the description names"], ms: 1000 }),
      );
      expect(once.logs[0].message).toBe("verification: CONTRADICTED — contradicted: described `x.rs` is not on the branch, and it is the only file the description names (1.0s)");
      const noted = send(colony(), verdict({ advisories: ["described `docs/remote-access.md` is not on the branch", "described `docs/remote-access.md` is not on the branch"] }));
      expect(noted.logs[0]).toMatchObject({
        level: "info",
        message: "verification: CONFIRMED — `npm test` green in a fresh checkout; note: described `docs/remote-access.md` is not on the branch (12.3s)",
      });
    });

    it("verify: none is unverifiable by declaration, with nothing measured", () => {
      const s = send(colony(), verdict({ verdict: "unverifiable", by_declaration: true, command: null, command_source: null, exit_code: null, tests_ms: null, snapshot: null, ms: 12 }));
      expect(s.logs[0]).toMatchObject({ level: "info", message: "verification: unverifiable by declaration (verify: none)" });
    });
  });

  it("event types the colony does not know change nothing in the transcript", () => {
    const s = send(colony(), event({ type: "mystery" } as unknown as AgentEventBody));
    expect(s.messages).toEqual([]);
    expect(s.turns).toEqual([]);
    expect(s.logs).toEqual([]);
  });

  it("reducing never rewrites the state it was handed", () => {
    let s = send(colony(), event({ type: "assistant_text_delta", message_id: "m1", block_index: 0, delta: "Hi" }));
    const snapshot = s;
    s = send(s, event({ type: "tool_call", message_id: "m1", tool_call_id: "t1", name: "Bash", input: {} }));
    s = send(s, event({ type: "tool_result", tool_call_id: "t1", output: "ok", is_error: false }));
    s = send(s, event({ type: "turn_end", is_error: false, result: null, cost_usd: null, duration_ms: null }));
    // The snapshot's single streaming block is untouched by everything that came after it.
    expect(snapshot.messages[0].blocks).toEqual([text("Hi", true)]);
    expect(s.messages[0].blocks.map((b) => b.kind)).toEqual(["text", "tool"]);
  });
});

// ---------------------------------------------------------------------------
// buildThread
// ---------------------------------------------------------------------------

describe("buildThread", () => {
  const thread = (messages: ChatMessage[], extra: Partial<StreamState> = {}): StreamState => ({
    ...initialStreamState(),
    messages,
    ...extra,
  });

  it("consecutive orchestrator messages share one bubble", () => {
    const view = buildThread(thread([orchestrator("m1", text("First half.")), orchestrator("m2", text("Second half."))]));
    expect(view.messages).toHaveLength(1);
    expect(view.messages[0].id).toBe("m1");
    expect(view.messages[0].content).toEqual([
      { type: "text", text: "First half." },
      { type: "text", text: "Second half." },
    ]);
  });

  it("a user message splits the bubbles", () => {
    const view = buildThread(
      thread([orchestrator("m1", text("On it.")), userSaid("u1", "Thanks"), orchestrator("m2", text("More work."))]),
    );
    expect(view.messages.map((m) => m.role)).toEqual(["assistant", "user", "assistant"]);
  });

  it("a user message's origin is keyed by its id for the render (issue #312); the operator's is not", () => {
    const view = buildThread(thread([{ ...userSaid("wd1", "Still there?"), origin: "watchdog" }, userSaid("u1", "Hi")]));
    expect(view.origins).toEqual({ wd1: "watchdog" });
  });

  it("one settler, however many messages it took, gets one card", () => {
    const view = buildThread(
      thread([settlerSaid("m1", scout("a1"), tool("t1", { output: "gone" })), settlerSaid("m2", scout("a1"), text("Found it."))]),
    );
    expect(view.messages).toHaveLength(1);
    expect(view.subagents["m2"]).toBeUndefined();
    expect(view.subagents["m1"].steps).toBe(1);
    expect(view.subagents["m1"].state).toBe("done");
    expect(view.subagents["m1"].report).toBe("Found it.");
  });

  it("settlers working in parallel interleave into one card each", () => {
    const view = buildThread(
      thread([
        settlerSaid("m1", scout("a1"), text("Scout one, first word.")),
        settlerSaid("m2", builder("a2"), text("Builder, first word.")),
        settlerSaid("m3", scout("a1"), text("Scout one, second word.")),
        settlerSaid("m4", builder("a2"), text("Builder, second word.")),
      ]),
    );
    expect(view.messages.map((m) => m.id)).toEqual(["m1", "m2"]);
    expect(view.subagents["m1"].report).toContain("second word");
    expect(view.subagents["m2"].report).toContain("second word");
  });

  it("settlers of one role are numbered in order of first appearance", () => {
    const view = buildThread(
      thread([
        settlerSaid("m1", scout("a1"), text("First scout.")),
        settlerSaid("m2", builder("a2"), text("A builder.")),
        settlerSaid("m3", scout("a3"), text("Second scout.")),
      ]),
    );
    expect(view.subagents["m1"].name).toBe("Scout Settler");
    expect(view.subagents["m1"].role).toBe("scout");
    expect(view.subagents["m2"].name).toBe("Builder Settler");
    expect(view.subagents["m3"].name).toBe("Scout Settler 2");
  });

  it("the same settler keeps its name when it is heard from later", () => {
    const view = buildThread(
      thread([
        settlerSaid("m1", scout("a1"), text("First.")),
        settlerSaid("m2", scout("a1"), text("Second.")),
        settlerSaid("m3", scout("a3"), text("Another scout.")),
      ]),
    );
    expect(view.subagents["m1"].name).toBe("Scout Settler");
    expect(view.subagents["m3"].name).toBe("Scout Settler 2");
  });

  it("a settler sent with no named type is a Builder named for its task", () => {
    const view = buildThread(thread([settlerSaid("m1", untyped("a1", "Dig test holes"), text("Holes dug."))]));
    expect(view.subagents["m1"].name).toBe("Builder Settler");
    expect(view.subagents["m1"].role).toBe("builder");
  });

  it("a settler the orchestrator interrupted carries on in a card further down", () => {
    const view = buildThread(
      thread([
        settlerSaid("m1", scout("a1"), tool("t1")),
        orchestrator("m2", text("Keep going, scout.")),
        settlerSaid("m3", scout("a1"), tool("t2", { name: "Grep" })),
      ]),
    );
    expect(view.subagents["m1"].state).toBe("continued");
    expect(view.subagents["m3"].state).toBe("working");
    expect(view.subagents["m3"].current).toEqual({ name: "Grep", input: { command: "ls" } });
  });

  it("settlers sent out together crew up on neighbouring cards", () => {
    const view = buildThread(
      thread([settlerSaid("m1", scout("a1"), text("Left flank.")), settlerSaid("m2", scout("a2"), text("Right flank.")), orchestrator("m3", text("Report, crew."))]),
    );
    expect(view.subagents["m1"].crew).toEqual({ ids: ["m1", "m2"], index: 0 });
    expect(view.subagents["m2"].crew).toEqual({ ids: ["m1", "m2"], index: 1 });
  });

  it("a settler out alone crews with nobody", () => {
    const view = buildThread(thread([settlerSaid("m1", scout("a1"), text("Alone.")), orchestrator("m2", text("And?"))]));
    expect(view.subagents["m1"].crew).toBeNull();
  });

  it("an unanswered question holds the thread for the user", () => {
    const view = buildThread(thread([orchestrator("m1", text("Shall I push?"), questionBlock("q1"))], { agentState: "waiting_for_answer" }));
    expect(view.hasOpenQuestion).toBe(true);
    expect(view.messages[0].status).toEqual({ type: "requires-action", reason: "tool-calls" });
  });

  it("an answered question does not hold the thread", () => {
    const view = buildThread(thread([orchestrator("m1", text("Pushing then."), questionBlock("q1", true))], { agentState: "idle" }));
    expect(view.hasOpenQuestion).toBe(false);
    expect(view.messages[0].status).toEqual({ type: "complete", reason: "stop" });
  });

  it("the last bubble shows running while the colony works", () => {
    const view = buildThread(thread([orchestrator("m1", text("Working..."))], { agentState: "working" }));
    expect(view.messages[0].status).toEqual({ type: "running" });
  });

  describe("turns and notices", () => {
    it("a summary lands under the bubble it followed, however many messages the bubble took", () => {
      const view = buildThread(
        thread([orchestrator("m1", text("Half one.")), orchestrator("m2", text("Half two.")), userSaid("u1", "Go on")], {
          turns: [turn("m2"), turn("u1")],
        }),
      );
      // m1 and m2 are one bubble, so the summary after m2 hangs off the bubble's id, m1.
      expect(Object.keys(view.turns)).toEqual(["m1", "u1"]);
    });

    it("a summary after a bubble that rendered nothing falls back to the last bubble that did", () => {
      const view = buildThread(
        thread([orchestrator("m1", text("Something.")), userSaid("u1", "Go on"), orchestrator("m2")], {
          // The next turn said nothing at all, so its summary hands back to the user's words.
          turns: [turn("m2")],
        }),
      );
      expect(Object.keys(view.turns)).toEqual(["u1"]);
    });

    it("a summary with nothing to follow waits at the end of the thread", () => {
      const view = buildThread(thread([orchestrator("m1", text("The end.")), userSaid("u1", "Bye")], { turns: [turn(null), turn("missing")] }));
      expect(view.turns[END_OF_THREAD]).toHaveLength(2);
    });

    it("memory proposals are filed the same way", () => {
      const view = buildThread(
        thread([orchestrator("m1", text("Noted."))], {
          memoryNotices: [
            { proposal: proposal("p1"), afterMessageId: "m1" },
            { proposal: proposal("p2"), afterMessageId: null },
          ],
        }),
      );
      expect(Object.keys(view.notices)).toEqual(["m1", END_OF_THREAD]);
    });
  });
});

// ---------------------------------------------------------------------------
// run_epoch (issue #96): a resumed run restarts its seqs, so the stream tracks which run it is
// following and drops the old watermark when the run changes, keeping the transcript.
// ---------------------------------------------------------------------------

describe("run_epoch", () => {
  const setup = () => {
    const opened: { sessionId: string; since: number; epoch: number | undefined }[] = [];
    const sockets: { onmessage: ((event: { data: string }) => void) | null }[] = [];
    const api = {
      openEvents: (sessionId: string, since: number, epoch?: number): SocketLike => {
        opened.push({ sessionId, since, epoch });
        const socket = {
          binaryType: "blob",
          readyState: 1,
          onopen: null,
          onmessage: null,
          onclose: null,
          onerror: null,
          send: () => {},
          close: () => {},
        };
        sockets.push(socket);
        return socket as unknown as SocketLike;
      },
    } as unknown as Api;
    const stream = new SessionStream(api, "s1");
    stream.start();
    const deliver = (frame: ServerFrame): void => {
      sockets[sockets.length - 1]?.onmessage?.({ data: JSON.stringify(frame) });
    };
    return { stream, opened, deliver };
  };

  it("the first connect asks from the start with no epoch yet", () => {
    const { opened } = setup();
    expect(opened).toEqual([{ sessionId: "s1", since: 0, epoch: 0 }]);
  });

  it("a run_epoch frame records the run without touching the watermark or the transcript", () => {
    const { stream, deliver } = setup();
    deliver(event({ type: "status", state: "working" }, 41));
    expect(stream.getState().lastSeq).toBe(41);
    deliver({ type: "run_epoch", epoch: 7 });
    expect(stream.getState().lastSeq).toBe(41);
    expect(stream.getState().messages).toEqual([]);
  });

  it("a new epoch drops the watermark but keeps the old run's messages", () => {
    const { stream, deliver } = setup();
    deliver({ type: "run_epoch", epoch: 7 });
    deliver(event({ type: "user_message", id: "u1", text: "Hi" }, 50));
    expect(stream.getState().messages.map((m) => m.id)).toEqual(["u1"]);
    deliver({ type: "run_epoch", epoch: 8 });
    expect(stream.getState().lastSeq).toBe(0);
    expect(stream.getState().messages.map((m) => m.id)).toEqual(["u1"]);
  });

  it("the same epoch twice leaves the watermark alone", () => {
    const { stream, deliver } = setup();
    deliver({ type: "run_epoch", epoch: 7 });
    deliver(event({ type: "status", state: "working" }, 50));
    deliver({ type: "run_epoch", epoch: 7 });
    expect(stream.getState().lastSeq).toBe(50);
  });

  it("the new run's low-numbered events pass the guard after a reset", () => {
    const { stream, deliver } = setup();
    deliver({ type: "run_epoch", epoch: 7 });
    deliver(event({ type: "status", state: "working" }, 50));
    deliver({ type: "run_epoch", epoch: 8 });
    deliver(event({ type: "assistant_text", message_id: "m2", block_index: 0, text: "Fresh run." }, 1));
    expect(stream.getState().messages.map((m) => m.id)).toEqual(["m2"]);
    expect(stream.getState().lastSeq).toBe(1);
  });

  it("a reconnect replays from the watermark with the current epoch", () => {
    const { stream, opened, deliver } = setup();
    deliver({ type: "run_epoch", epoch: 7 });
    deliver(event({ type: "status", state: "working" }, 50));
    stream.stop();
    stream.start();
    expect(opened[opened.length - 1]).toEqual({ sessionId: "s1", since: 50, epoch: 7 });
  });
});

// ---------------------------------------------------------------------------
// replay hold: a long backlog renders once, on its latest messages, not frame by frame.
// ---------------------------------------------------------------------------

describe("replay hold", () => {
  const setup = () => {
    const sockets: { onmessage: ((event: { data: string }) => void) | null }[] = [];
    const api = {
      openEvents: (): SocketLike => {
        const socket = { binaryType: "blob", readyState: 1, onopen: null, onmessage: null, onclose: null, onerror: null, send: () => {}, close: () => {} };
        sockets.push(socket);
        return socket as unknown as SocketLike;
      },
    } as unknown as Api;
    const stream = new SessionStream(api, "s1");
    let renders = 0;
    stream.subscribe(() => {
      renders += 1;
    });
    stream.start();
    const deliver = (frame: ServerFrame): void => {
      sockets[sockets.length - 1]?.onmessage?.({ data: JSON.stringify(frame) });
    };
    return { stream, deliver, renders: () => renders };
  };

  afterEach(() => {
    vi.useRealTimers();
  });

  it("gathers the backlog silently and renders once on replay_done", () => {
    vi.useFakeTimers();
    const { stream, deliver, renders } = setup();
    const before = renders();
    for (let seq = 1; seq <= 50; seq++) deliver(event({ type: "user_message", id: `u${seq}`, text: "Hi" }, seq));
    expect(renders()).toBe(before);
    expect(stream.getState().messages).toHaveLength(50);
    deliver({ type: "replay_done", seq: 50 });
    expect(renders()).toBe(before + 1);
    deliver(event({ type: "user_message", id: "live", text: "Hi" }, 51));
    expect(renders()).toBe(before + 2);
  });

  it("an older mothership with no replay_done still renders after a quiet gap", () => {
    vi.useFakeTimers();
    const { deliver, renders } = setup();
    const before = renders();
    deliver(event({ type: "user_message", id: "u1", text: "Hi" }, 1));
    expect(renders()).toBe(before);
    vi.advanceTimersByTime(1000);
    expect(renders()).toBe(before + 1);
  });
});
