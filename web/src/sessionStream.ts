// Session event stream: WebSocket client for /api/sessions/{id}/events plus the reducer that turns
// protocol events (docs/protocol.md §2–4, §6) into chat state, and the adapter into assistant-ui messages.
import type { ThreadMessageLike } from "@assistant-ui/react";
import { useEffect, useState, useSyncExternalStore } from "react";
import { SOCKET_OPEN, type Api, type SocketLike } from "./api";
import type { AntRole } from "./components/AntAvatar";
import { isLive } from "./components/ui";
import { settlerName, settlerRole } from "./settlers";
import type {
  AgentEvent,
  AgentEventBody,
  AgentRef,
  AgentState,
  Answers,
  ClientCommand,
  LogLevel,
  MemoryProposal,
  ModelTokens,
  Origin,
  Question,
  ServerFrame,
  Session,
} from "./types";

export interface TextBlock {
  kind: "text";
  index: number;
  text: string;
  streaming: boolean;
}

export interface ThinkingBlock {
  kind: "thinking";
  index: number;
  text: string;
}

export interface ToolBlock {
  kind: "tool";
  id: string;
  name: string;
  input: Record<string, unknown>;
  output: string | null;
  isError: boolean;
}

export interface QuestionBlock {
  kind: "question";
  id: string;
  questions: Question[];
  /** Set once `question_answered` lands; `origin` names who answered (§6.3: the judge or the operator). */
  answer: AskUserResult | null;
  /** When the question event arrived (the frame's `ts`); null when the replay omitted one. */
  asked_at: string | null;
}

export type Block = TextBlock | ThinkingBlock | ToolBlock | QuestionBlock;

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  /** The subagent that spoke, when it was not the orchestrator. */
  agent?: AgentRef;
  /** The envelope origin (issue #312); absent on lines recorded before it, and on optimistic sends. */
  origin?: Origin;
  blocks: Block[];
  ts: string | null;
  /** Optimistic user message not yet echoed by the agent. */
  pending: boolean;
}

export interface TurnSummary {
  afterMessageId: string | null;
  isError: boolean;
  result: string | null;
  costUsd: number | null;
  durationMs: number | null;
  /**
   * The models that served *this* turn, most tokens first. Not the raw backend field: `turn_end.model_usage`
   * is cumulative for the whole colony (docs/protocol.md §4), so these are derived by diffing each `turn_end`
   * against the previous one. Empty when no model's total grew (a cached, no-op or error turn) or the runner
   * sent no usage.
   */
  models: string[];
  ts: string | null;
}

/** A colony proposed a shared-memory note (§6.2); shown inline where it happened. */
export interface MemoryNotice {
  proposal: MemoryProposal;
  afterMessageId: string | null;
}

export interface LogEntry {
  source: "harness" | "agent";
  level: LogLevel;
  message: string;
  ts: string | null;
}

export type ConnectionState = "connecting" | "open" | "reconnecting" | "closed";

export interface StreamState {
  session: Session | null;
  messages: ChatMessage[];
  turns: TurnSummary[];
  memoryNotices: MemoryNotice[];
  agentState: AgentState | null;
  agentDetail: string | null;
  logs: LogEntry[];
  lastSeq: number;
  connection: ConnectionState;
  /** Questions whose answer was sent but not yet acknowledged with `question_answered`. */
  submitting: Record<string, true>;
  /**
   * Summed tokens per model as of the last observed `turn_end` — the baseline the next `turn_end` is diffed
   * against to work out which models served the turn in between. Null until a `turn_end` with `model_usage`
   * is seen; past turns are recovered because a (re)connect replays the session's stored event log.
   */
  modelUsage: Record<string, number> | null;
  /** The model the colony's next turns use, from the latest `model_changed`. Null until the runner reports one. */
  model: string | null;
  /** A model asked for with `set_model` and not yet confirmed; a runner that can't switch logs a warning instead. */
  switchingModel: string | null;
  /** The last switch that ended in a warning instead of a `model_changed`; cleared by the next switch or report. */
  refusedModel: string | null;
}

export function initialStreamState(): StreamState {
  return {
    session: null,
    messages: [],
    turns: [],
    memoryNotices: [],
    agentState: null,
    agentDetail: null,
    logs: [],
    lastSeq: 0,
    connection: "connecting",
    submitting: {},
    modelUsage: null,
    model: null,
    switchingModel: null,
    refusedModel: null,
  };
}

/** The watchdog nudges an agent with a user_message whose id has this prefix (§6.3). */
export const WATCHDOG_PREFIX = "watchdog-";

export function isWatchdogMessageId(id: string | null | undefined): boolean {
  return typeof id === "string" && id.startsWith(WATCHDOG_PREFIX);
}

const MAX_LOGS = 400;

function textOf(message: ChatMessage): string {
  return message.blocks.map((b) => (b.kind === "text" ? b.text : "")).join("");
}

/** Returns a copy of `messages` with the assistant message `id` (created if missing) copied for mutation. */
function upsertAssistant(
  messages: ChatMessage[],
  id: string,
  ts: string | null,
  agent?: AgentRef,
): [ChatMessage[], ChatMessage] {
  const copy = messages.slice();
  for (let i = copy.length - 1; i >= 0; i--) {
    if (copy[i].id === id && copy[i].role === "assistant") {
      const message = { ...copy[i], blocks: copy[i].blocks.slice() };
      copy[i] = message;
      return [copy, message];
    }
  }
  const message: ChatMessage = { id, role: "assistant", agent, blocks: [], ts, pending: false };
  copy.push(message);
  return [copy, message];
}

function withoutKey(record: Record<string, true>, key: string): Record<string, true> {
  if (!(key in record)) return record;
  const next = { ...record };
  delete next[key];
  return next;
}

/** Sums a `model_usage` map into one total per model, treating missing or non-finite counts as zero. */
function usageTotals(usage: Record<string, ModelTokens> | undefined): Record<string, number> {
  const totals: Record<string, number> = {};
  for (const [model, tokens] of Object.entries(usage ?? {})) {
    if (!tokens || typeof tokens !== "object") continue;
    const count = (value: number) => (Number.isFinite(value) ? value : 0);
    totals[model] = count(tokens.input_tokens) + count(tokens.output_tokens) + count(tokens.cache_read_tokens) + count(tokens.cache_write_tokens);
  }
  return totals;
}

/**
 * The models that served one turn, most tokens first. `turn_end.model_usage` is the colony's cumulative
 * total (docs/protocol.md §4), so a model only counts if its total grew since the previous `turn_end`.
 * With no previous snapshot the whole total counts: a fresh colony's first `turn_end` really is all this
 * turn's usage, and any earlier turns a client missed come back as replayed `turn_end`s that rebuild the
 * baseline chain first (a resumed colony starts a fresh runner whose totals restart at zero, so nothing
 * that served before the resume is misattributed). Models whose total stalls or shrinks — a cached, no-op
 * or error turn — name nothing: silence beats inventing a model, and a shrunk total is never subtracted
 * into a negative.
 */
function modelsOfTurn(previous: Record<string, number> | null, current: Record<string, number>): string[] {
  return Object.entries(current)
    .map(([model, total]) => [model, total - (previous?.[model] ?? 0)] as const)
    .filter(([, delta]) => delta > 0)
    .sort((a, b) => b[1] - a[1])
    .map(([model]) => model);
}

/** One verification host event as a line for the activity strip, the same words the report uses. */
function verificationLine(ev: Extract<AgentEventBody, { type: "verification" }>): string {
  if (ev.by_declaration) return "verification: unverifiable by declaration (verify: none)";
  const verdict = String(ev.verdict ?? "unverifiable").toUpperCase();
  // The summary already quotes the first contradiction; each is said once.
  const contradictions = (ev.contradictions ?? []).filter((c) => !ev.summary?.includes(c));
  const notes = [...new Set(ev.advisories ?? [])].map((a) => `note: ${a}`);
  const detail = [...new Set([ev.summary, ...contradictions, ...notes])].filter(Boolean).join("; ");
  const seconds = Number.isFinite(ev.ms) ? ` (${(ev.ms / 1000).toFixed(1)}s)` : "";
  return `verification: ${verdict}${detail ? ` — ${detail}` : ""}${seconds}`;
}

export function reduceFrame(state: StreamState, frame: ServerFrame): StreamState {
  if (frame.type === "session") {
    // A colony that stopped being live drops commands, so a switch still pending will never be answered.
    const switchingModel = isLive(frame.session.status) ? state.switchingModel : null;
    return { ...state, session: frame.session, switchingModel };
  }
  if (frame.type === "harness_log") {
    // The harness replays recent logs on every reconnect.
    if (state.logs.some((l) => l.source === "harness" && l.ts === frame.ts && l.message === frame.message)) {
      return state;
    }
    const entry: LogEntry = { source: "harness", level: frame.level, message: frame.message, ts: frame.ts };
    return { ...state, logs: [...state.logs, entry].slice(-MAX_LOGS) };
  }
  if (frame.type === "memory_proposed") {
    const proposal = frame.proposal;
    if (!proposal?.id || state.memoryNotices.some((n) => n.proposal.id === proposal.id)) return state;
    const last = state.messages[state.messages.length - 1];
    return { ...state, memoryNotices: [...state.memoryNotices, { proposal, afterMessageId: last?.id ?? null }] };
  }

  const ev = frame as AgentEvent;
  let s = state;
  if (typeof ev.seq === "number") {
    if (ev.seq <= s.lastSeq) return s;
    s = { ...s, lastSeq: ev.seq };
  }
  const ts = ev.ts ?? null;

  switch (ev.type) {
    case "status":
      return { ...s, agentState: ev.state, agentDetail: ev.detail ?? null };

    case "user_message": {
      const pending = s.messages.findIndex((m) => m.role === "user" && m.pending && textOf(m) === ev.text);
      if (pending >= 0) {
        const messages = s.messages.slice();
        messages[pending] = { ...messages[pending], id: ev.id, pending: false, ts, origin: ev.origin };
        return { ...s, messages };
      }
      if (s.messages.some((m) => m.role === "user" && m.id === ev.id)) return s;
      const message: ChatMessage = {
        id: ev.id,
        role: "user",
        origin: ev.origin,
        blocks: [{ kind: "text", index: 0, text: ev.text, streaming: false }],
        ts,
        pending: false,
      };
      return { ...s, messages: [...s.messages, message] };
    }

    case "assistant_text_delta":
    case "assistant_text": {
      const [messages, message] = upsertAssistant(s.messages, ev.message_id, ts, ev.agent);
      const at = message.blocks.findIndex((b) => b.kind === "text" && b.index === ev.block_index);
      if (ev.type === "assistant_text_delta") {
        if (at >= 0) {
          const block = message.blocks[at] as TextBlock;
          if (!block.streaming) return s; // final text already arrived
          message.blocks[at] = { ...block, text: block.text + ev.delta };
        } else {
          message.blocks.push({ kind: "text", index: ev.block_index, text: ev.delta, streaming: true });
        }
      } else {
        const block: TextBlock = { kind: "text", index: ev.block_index, text: ev.text, streaming: false };
        if (at >= 0) message.blocks[at] = block;
        else message.blocks.push(block);
      }
      return { ...s, messages };
    }

    case "thinking": {
      const [messages, message] = upsertAssistant(s.messages, ev.message_id, ts, ev.agent);
      const at = message.blocks.findIndex((b) => b.kind === "thinking" && b.index === ev.block_index);
      const block: ThinkingBlock = { kind: "thinking", index: ev.block_index, text: ev.text };
      if (at >= 0) message.blocks[at] = block;
      else message.blocks.push(block);
      return { ...s, messages };
    }

    case "tool_call": {
      if (s.messages.some((m) => m.blocks.some((b) => b.kind === "tool" && b.id === ev.tool_call_id))) return s;
      const [messages, message] = upsertAssistant(s.messages, ev.message_id, ts, ev.agent);
      message.blocks.push({
        kind: "tool",
        id: ev.tool_call_id,
        name: ev.name,
        input: ev.input ?? {},
        output: null,
        isError: false,
      });
      return { ...s, messages };
    }

    case "tool_result": {
      for (let i = s.messages.length - 1; i >= 0; i--) {
        const at = s.messages[i].blocks.findIndex((b) => b.kind === "tool" && b.id === ev.tool_call_id);
        if (at < 0) continue;
        const messages = s.messages.slice();
        const message = { ...messages[i], blocks: messages[i].blocks.slice() };
        const block = message.blocks[at] as ToolBlock;
        message.blocks[at] = { ...block, output: ev.output, isError: ev.is_error };
        messages[i] = message;
        return { ...s, messages };
      }
      return s;
    }

    case "question": {
      if (s.messages.some((m) => m.blocks.some((b) => b.kind === "question" && b.id === ev.question_id))) return s;
      const last = s.messages[s.messages.length - 1];
      const targetId = ev.message_id ?? (last?.role === "assistant" ? last.id : `q-${ev.question_id}`);
      const [messages, message] = upsertAssistant(s.messages, targetId, ts);
      message.blocks.push({ kind: "question", id: ev.question_id, questions: ev.questions ?? [], answer: null, asked_at: ts });
      return { ...s, messages };
    }

    case "question_answered": {
      const submitting = withoutKey(s.submitting, ev.question_id);
      for (let i = s.messages.length - 1; i >= 0; i--) {
        const at = s.messages[i].blocks.findIndex((b) => b.kind === "question" && b.id === ev.question_id);
        if (at < 0) continue;
        const messages = s.messages.slice();
        const message = { ...messages[i], blocks: messages[i].blocks.slice() };
        const block = message.blocks[at] as QuestionBlock;
        message.blocks[at] = { ...block, answer: { answers: ev.answers ?? {}, response: ev.response ?? null, origin: ev.origin } };
        messages[i] = message;
        return { ...s, messages, submitting };
      }
      return { ...s, submitting };
    }

    case "turn_end": {
      const last = s.messages[s.messages.length - 1];
      // `model_usage` is the colony's cumulative total, not this turn's own usage, so diff it against the
      // previous `turn_end` to name the models that actually served this one.
      const usage = usageTotals(ev.model_usage);
      const turn: TurnSummary = {
        afterMessageId: last?.id ?? null,
        isError: ev.is_error,
        result: ev.result,
        costUsd: ev.cost_usd,
        durationMs: ev.duration_ms,
        models: modelsOfTurn(s.modelUsage, usage),
        ts,
      };
      // Any text still marked streaming is final now.
      const messages = s.messages.map((m) =>
        m.blocks.some((b) => b.kind === "text" && b.streaming)
          ? { ...m, blocks: m.blocks.map((b) => (b.kind === "text" ? { ...b, streaming: false } : b)) }
          : m,
      );
      // A `turn_end` without `model_usage` says nothing new about usage; keep the last known baseline.
      return { ...s, messages, turns: [...s.turns, turn], modelUsage: Object.keys(usage).length > 0 ? usage : s.modelUsage };
    }

    case "log": {
      const entry: LogEntry = { source: "agent", level: ev.level, message: ev.message, ts };
      // A failed switch comes back as a warning, not a `model_changed`, so any warning ends the wait.
      const logs = [...s.logs, entry].slice(-MAX_LOGS);
      const refused = ev.level !== "info" && s.switchingModel !== null;
      return refused ? { ...s, logs, switchingModel: null, refusedModel: s.switchingModel } : { ...s, logs };
    }

    case "verification": {
      // The mothership's own verdict on the colony's completion claim: one harness line in the activity
      // strip, warn-level when the claim was contradicted so it colors like other trouble.
      const entry: LogEntry = { source: "harness", level: ev.verdict === "contradicted" ? "warn" : "info", message: verificationLine(ev), ts };
      return { ...s, logs: [...s.logs, entry].slice(-MAX_LOGS) };
    }

    case "model_changed":
      return { ...s, model: ev.model, switchingModel: null, refusedModel: null };

    default:
      return s; // unknown event types are ignored
  }
}

export class SessionStream {
  readonly sessionId: string;
  private readonly api: Api;
  private ws: SocketLike | null = null;
  private state: StreamState = initialStreamState();
  /** The run the server is currently streaming; a change means the next run's seqs start over. */
  private runEpoch: number | null = null;
  private listeners = new Set<() => void>();
  private retries = 0;
  private stopped = false;
  private timer: ReturnType<typeof setTimeout> | null = null;
  /**
   * While a connection replays its backlog, frames are reduced without telling listeners, so a long
   * history renders once — on its latest messages — instead of frame by frame. `replay_done` ends it;
   * a mothership that never sends one is flushed after a quiet gap, or at the latest after HOLD_MAX_MS.
   */
  private holding = false;
  private holdSince = 0;
  private holdTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(api: Api, sessionId: string) {
    this.api = api;
    this.sessionId = sessionId;
  }

  getState = (): StreamState => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  start(): void {
    this.stopped = false;
    this.connect();
  }

  stop(): void {
    this.stopped = true;
    if (this.timer) clearTimeout(this.timer);
    this.release();
    const ws = this.ws;
    this.ws = null;
    ws?.close();
    this.update((s) => ({ ...s, connection: "closed" }));
  }

  /** Sends a command; returns false when the stream isn't connected. */
  send(command: ClientCommand): boolean {
    const ws = this.ws;
    if (!ws || ws.readyState !== SOCKET_OPEN) return false;
    ws.send(JSON.stringify(command));
    if (command.type === "user_message") {
      const message: ChatMessage = {
        id: `local-${Date.now()}`,
        role: "user",
        blocks: [{ kind: "text", index: 0, text: command.text, streaming: false }],
        ts: new Date().toISOString(),
        pending: true,
      };
      this.update((s) => ({ ...s, messages: [...s.messages, message] }));
    } else if (command.type === "answer") {
      this.update((s) => ({ ...s, submitting: { ...s.submitting, [command.question_id]: true } }));
    } else if (command.type === "set_model") {
      this.update((s) => ({ ...s, switchingModel: command.model, refusedModel: null }));
    }
    return true;
  }

  private update(fn: (s: StreamState) => StreamState): void {
    const next = fn(this.state);
    if (next === this.state) return;
    this.state = next;
    if (!this.holding) this.notify();
  }

  private notify(): void {
    for (const listener of this.listeners) listener();
  }

  /** Ends a replay hold and renders what it gathered, once. */
  private release(): void {
    if (this.holdTimer) clearTimeout(this.holdTimer);
    this.holdTimer = null;
    if (!this.holding) return;
    this.holding = false;
    this.notify();
  }

  /** Re-arms the quiet-gap flush while holding; past HOLD_MAX_MS it renders at once. */
  private extendHold(): void {
    if (!this.holding) return;
    if (Date.now() - this.holdSince > HOLD_MAX_MS) {
      this.release();
      return;
    }
    if (this.holdTimer) clearTimeout(this.holdTimer);
    this.holdTimer = setTimeout(() => this.release(), HOLD_QUIET_MS);
  }

  private connect(): void {
    this.update((s) => ({ ...s, connection: this.retries > 0 ? "reconnecting" : "connecting" }));
    const ws = this.api.openEvents(this.sessionId, this.state.lastSeq, this.runEpoch ?? 0);
    this.ws = ws;
    this.holding = true;
    this.holdSince = Date.now();
    this.extendHold();
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.retries = 0;
      this.update((s) => ({ ...s, connection: "open" }));
    };
    ws.onmessage = (event) => {
      if (this.ws !== ws || typeof event.data !== "string") return;
      let frame: ServerFrame;
      try {
        frame = JSON.parse(event.data) as ServerFrame;
      } catch {
        return;
      }
      if (!frame || typeof frame !== "object" || typeof frame.type !== "string") return;
      if (frame.type === "replay_done") {
        this.release();
        return;
      }
      if (frame.type === "run_epoch") {
        const e = frame.epoch;
        if (typeof e === "number") {
          if (this.runEpoch !== null && e !== this.runEpoch) {
            this.update((s) => ({ ...s, lastSeq: 0 }));
          }
          this.runEpoch = e;
        }
        return;
      }
      this.update((s) => reduceFrame(s, frame));
      this.extendHold();
    };
    ws.onerror = () => {};
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.release();
      if (this.stopped) return;
      // Answers and model switches in flight may not have arrived; let the user resubmit after reconnecting.
      this.update((s) => ({ ...s, connection: "reconnecting", submitting: {}, switchingModel: null }));
      const delay = Math.min(1000 * 2 ** this.retries, 10_000);
      this.retries += 1;
      this.timer = setTimeout(() => {
        if (!this.stopped) this.connect();
      }, delay);
    };
  }
}

/** A replay with no `replay_done` (an older mothership) renders after this long without a frame… */
const HOLD_QUIET_MS = 400;
/** …and a hold never outlasts this, so a slow backlog still shows before it finishes. */
const HOLD_MAX_MS = 4000;

const EMPTY_STATE = initialStreamState();
const noopSubscribe = () => () => {};
const getEmptyState = () => EMPTY_STATE;

export function useSessionStream(api: Api, sessionId: string | null): { stream: SessionStream | null; state: StreamState } {
  const [stream, setStream] = useState<SessionStream | null>(null);
  useEffect(() => {
    if (!sessionId) {
      setStream(null);
      return;
    }
    const next = new SessionStream(api, sessionId);
    next.start();
    setStream(next);
    return () => next.stop();
  }, [api, sessionId]);
  const state = useSyncExternalStore(stream?.subscribe ?? noopSubscribe, stream?.getState ?? getEmptyState);
  return { stream, state };
}

// ---------------------------------------------------------------------------
// assistant-ui adapter
// ---------------------------------------------------------------------------

type Part = Exclude<ThreadMessageLike["content"], string>[number];
type ToolCallPart = Extract<Part, { type: "tool-call" }>;

export const ASK_USER_TOOL = "ask_user";

/** Key in `ThreadView.turns` / `ThreadView.notices` for items that have no rendered message to follow. */
export const END_OF_THREAD = "__end__";

export interface AskUserArgs {
  questions: Question[];
}

export interface AskUserResult {
  answers: Answers;
  response: string | null;
  /** Who answered (issue #312): the autonomy judge's answer never renders as the operator's own. */
  origin?: Origin;
}

export interface ToolResultPayload {
  output: string;
  is_error: boolean;
}

/**
 * What a subagent is doing, as its card shows it. `working`: a tool is running. `thinking`: between steps, waiting on
 * its model. `writing`: its report is streaming in. `done`: it ended on text, its report. `continued`: the orchestrator
 * spoke in between, and this subagent's later work is in a card further down. Settlers working in parallel don't
 * interrupt each other: each keeps one card for the whole burst.
 */
export type SubagentState = "working" | "thinking" | "writing" | "done" | "continued";

export interface ToolRef {
  name: string;
  input: Record<string, unknown>;
}

export interface SubagentStep extends ToolRef {
  running: boolean;
  failed: boolean;
}

export interface SubagentView {
  agent: AgentRef;
  /** The settler name shown for it, numbered when a colony has several of one role. */
  name: string;
  /** Which ant it is. */
  role: AntRole;
  state: SubagentState;
  /** The tool running now, when `working`. */
  current: ToolRef | null;
  /** The last tool it started, for the status line between steps. */
  last: ToolRef | null;
  /** Tool calls in this card. */
  steps: number;
  /** Every tool call in this card, in order. */
  tools: SubagentStep[];
  /** Tool calls that came back as errors; the ant stumbles each time this grows. */
  errors: number;
  /** The text it ended on: its report, or the report so far while `writing`. Empty until then. */
  report: string;
  /** Set when this card is one of a crew, settlers the orchestrator sent out together: the crew's cards in order. */
  crew: { ids: string[]; index: number } | null;
}

/** Reads a subagent card's state from its blocks, as they stand now. */
export function subagentState(blocks: Block[]): Pick<SubagentView, "state" | "current" | "last" | "steps" | "tools" | "errors" | "report"> {
  const tools = blocks.filter((b): b is ToolBlock => b.kind === "tool");
  const running = [...tools].reverse().find((t) => t.output === null);
  const lastTool = tools[tools.length - 1];
  const ref = (t: ToolBlock | undefined): ToolRef | null => (t ? { name: t.name, input: t.input } : null);
  const lastBlock = blocks[blocks.length - 1];
  const state: SubagentState = running
    ? "working"
    : lastBlock?.kind === "text"
      ? lastBlock.streaming
        ? "writing"
        : "done"
      : "thinking";
  const afterTools = blocks.slice(blocks.lastIndexOf(lastTool as Block) + 1);
  const report =
    state === "writing" || state === "done"
      ? afterTools
          .map((b) => (b.kind === "text" ? b.text.trim() : ""))
          .filter(Boolean)
          .join("\n\n")
      : "";
  return {
    state,
    current: ref(running),
    last: ref(lastTool),
    steps: tools.length,
    tools: tools.map((t) => ({ name: t.name, input: t.input, running: t.output === null, failed: t.output !== null && t.isError })),
    errors: tools.filter((t) => t.output !== null && t.isError).length,
    report,
  };
}

export interface ThreadView {
  messages: ThreadMessageLike[];
  /** The subagent behind a rendered message, keyed by its id; absent means the orchestrator. */
  subagents: Record<string, SubagentView>;
  /** The envelope origin of a user message, keyed by its id; absent means the operator's own words. */
  origins: Record<string, Origin>;
  /** Turn summaries keyed by the id of the (grouped) message they follow. */
  turns: Record<string, TurnSummary[]>;
  /** Memory proposals keyed the same way. */
  notices: Record<string, MemoryNotice[]>;
  hasOpenQuestion: boolean;
}

function toParts(blocks: Block[]): Part[] {
  const parts: Part[] = [];
  for (const block of blocks) {
    switch (block.kind) {
      case "text":
        if (block.text) parts.push({ type: "text", text: block.text });
        break;
      case "thinking":
        parts.push({ type: "reasoning", text: block.text });
        break;
      case "tool": {
        const part: ToolCallPart = {
          type: "tool-call",
          toolCallId: block.id,
          toolName: block.name,
          args: block.input as ToolCallPart["args"],
          result:
            block.output === null ? undefined : ({ output: block.output, is_error: block.isError } satisfies ToolResultPayload),
          isError: block.output === null ? undefined : block.isError,
        };
        parts.push(part);
        break;
      }
      case "question": {
        const part: ToolCallPart = {
          type: "tool-call",
          toolCallId: block.id,
          toolName: ASK_USER_TOOL,
          args: { questions: block.questions } as unknown as ToolCallPart["args"],
          result: block.answer ?? undefined,
        };
        parts.push(part);
        break;
      }
    }
  }
  return parts;
}

/** Groups consecutive assistant messages (one per model response) into one chat bubble. */
export function buildThread(state: StreamState): ThreadView {
  const messages: ThreadMessageLike[] = [];
  const turns: Record<string, TurnSummary[]> = {};
  const notices: Record<string, MemoryNotice[]> = {};
  const groupOf = new Map<string, string>();
  // Assistant groups with nothing renderable (e.g. a failed turn's empty text) hand their turn
  // summaries to the message rendered before them.
  const fallbackOf = new Map<string, string | null>();
  const emitted = new Set<string>();
  let lastEmitted: string | null = null;
  let hasOpenQuestion = false;
  const subagents: Record<string, SubagentView> = {};
  const origins: Record<string, Origin> = {};
  // Settler names are numbered per role in order of first appearance: the second Explore is "Scout Settler 2".
  const names = new Map<string, string>();
  const perRole = new Map<string, number>();
  // The runner falls back to the task description when the Task call named no type.
  const typeOf = (agent: AgentRef): string | null => (agent.name === agent.description ? null : agent.name);
  const nameOf = (agent: AgentRef): string => {
    let name = names.get(agent.id);
    if (!name) {
      const type = typeOf(agent);
      const role = settlerName(type);
      const ordinal = (perRole.get(role) ?? 0) + 1;
      perRole.set(role, ordinal);
      name = settlerName(type, ordinal);
      names.set(agent.id, name);
    }
    return name;
  };
  type Group = { id: string; blocks: Block[]; ts: string | null; agent?: AgentRef };
  // The orchestrator's open bubble.
  let group: Group | null = null;
  // Settlers heard from since the orchestrator last spoke, in order of first appearance. Settlers working in parallel
  // interleave their events; each one still gets a single card.
  let crew: Group[] = [];

  const emit = (g: Group) => {
    const parts = toParts(g.blocks);
    if (parts.length > 0) {
      // Text still streaming is "running" wherever it is, not only in the newest bubble: settlers working in parallel
      // stream into cards above it, and a part that isn't running is drawn in whole chunks instead of revealed smoothly.
      const streaming = state.agentState === "working" && g.blocks.some((b) => b.kind === "text" && b.streaming);
      messages.push({
        role: "assistant",
        id: g.id,
        content: parts,
        createdAt: g.ts ? new Date(g.ts) : undefined,
        ...(streaming ? { status: { type: "running" } as const } : {}),
      });
      if (g.agent) {
        subagents[g.id] = {
          agent: g.agent,
          name: nameOf(g.agent),
          role: settlerRole(typeOf(g.agent)),
          crew: null,
          ...subagentState(g.blocks),
        };
      }
      emitted.add(g.id);
      lastEmitted = g.id;
    } else {
      fallbackOf.set(g.id, lastEmitted);
    }
  };
  const flush = () => {
    if (group) emit(group);
    group = null;
    for (const g of crew) emit(g);
    crew = [];
  };

  for (const message of state.messages) {
    if (message.role === "user") {
      flush();
      groupOf.set(message.id, message.id);
      messages.push({
        role: "user",
        id: message.id,
        content: [{ type: "text", text: textOf(message) }],
        createdAt: message.ts ? new Date(message.ts) : undefined,
      });
      if (message.origin) origins[message.id] = message.origin;
      emitted.add(message.id);
      lastEmitted = message.id;
      continue;
    }
    let target: Group;
    if (message.agent) {
      const agent = message.agent;
      if (group) emit(group);
      group = null;
      let member = crew.find((g) => g.agent?.id === agent.id);
      if (!member) {
        member = { id: message.id, blocks: [], ts: message.ts, agent };
        crew.push(member);
      }
      target = member;
    } else {
      if (crew.length > 0) flush();
      group ??= { id: message.id, blocks: [], ts: message.ts };
      target = group;
    }
    target.blocks.push(...message.blocks);
    groupOf.set(message.id, target.id);
    if (message.blocks.some((b) => b.kind === "question" && !b.answer)) hasOpenQuestion = true;
  }
  flush();

  // Status of the newest assistant bubble drives running indicators.
  const last = messages[messages.length - 1];
  if (last?.role === "assistant") {
    const status: ThreadMessageLike["status"] = hasOpenQuestion
      ? { type: "requires-action", reason: "tool-calls" }
      : state.agentState === "working"
        ? { type: "running" }
        : { type: "complete", reason: "stop" };
    messages[messages.length - 1] = { ...last, status };
  }

  const placement = (afterMessageId: string | null): string => {
    let key: string | null | undefined = afterMessageId ? groupOf.get(afterMessageId) : undefined;
    if (key && !emitted.has(key)) key = fallbackOf.get(key);
    return key ?? END_OF_THREAD;
  };
  for (const turn of state.turns) (turns[placement(turn.afterMessageId)] ??= []).push(turn);
  for (const notice of state.memoryNotices) (notices[placement(notice.afterMessageId)] ??= []).push(notice);

  // Only a subagent's latest card is live; an earlier one was interrupted by the orchestrator and carries on below.
  const latest = new Set<string>();
  for (let i = messages.length - 1; i >= 0; i--) {
    const view = subagents[messages[i].id ?? ""];
    if (!view) continue;
    if (latest.has(view.agent.id)) view.state = "continued";
    else latest.add(view.agent.id);
  }

  // Settler cards next to each other in the thread were sent out together: a crew, drawn on one trail.
  for (let i = 0; i < messages.length; ) {
    let end = i;
    while (end < messages.length && subagents[messages[end].id ?? ""]) end++;
    if (end - i > 1) {
      const ids = messages.slice(i, end).map((m) => m.id ?? "");
      ids.forEach((id, index) => (subagents[id].crew = { ids, index }));
    }
    i = Math.max(end, i + 1);
  }

  return { messages, subagents, origins, turns, notices, hasOpenQuestion };
}
