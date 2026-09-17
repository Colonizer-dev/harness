// Session event stream: WebSocket client for /api/sessions/{id}/events plus the reducer that turns
// protocol events (docs/protocol.md §2–4, §6) into chat state, and the adapter into assistant-ui messages.
import type { ThreadMessageLike } from "@assistant-ui/react";
import { useEffect, useState, useSyncExternalStore } from "react";
import { SOCKET_OPEN, type Api, type SocketLike } from "./api";
import { settlerName } from "./settlers";
import type {
  AgentEvent,
  AgentRef,
  AgentState,
  Answers,
  ClientCommand,
  LogLevel,
  MemoryProposal,
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
  answer: { answers: Answers; response: string | null } | null;
}

export type Block = TextBlock | ThinkingBlock | ToolBlock | QuestionBlock;

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  /** The subagent that spoke, when it was not the orchestrator. */
  agent?: AgentRef;
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

export function reduceFrame(state: StreamState, frame: ServerFrame): StreamState {
  if (frame.type === "session") return { ...state, session: frame.session };
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
        messages[pending] = { ...messages[pending], id: ev.id, pending: false, ts };
        return { ...s, messages };
      }
      if (s.messages.some((m) => m.role === "user" && m.id === ev.id)) return s;
      const message: ChatMessage = {
        id: ev.id,
        role: "user",
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
      message.blocks.push({ kind: "question", id: ev.question_id, questions: ev.questions ?? [], answer: null });
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
        message.blocks[at] = { ...block, answer: { answers: ev.answers ?? {}, response: ev.response ?? null } };
        messages[i] = message;
        return { ...s, messages, submitting };
      }
      return { ...s, submitting };
    }

    case "turn_end": {
      const last = s.messages[s.messages.length - 1];
      const turn: TurnSummary = {
        afterMessageId: last?.id ?? null,
        isError: ev.is_error,
        result: ev.result,
        costUsd: ev.cost_usd,
        durationMs: ev.duration_ms,
        ts,
      };
      // Any text still marked streaming is final now.
      const messages = s.messages.map((m) =>
        m.blocks.some((b) => b.kind === "text" && b.streaming)
          ? { ...m, blocks: m.blocks.map((b) => (b.kind === "text" ? { ...b, streaming: false } : b)) }
          : m,
      );
      return { ...s, messages, turns: [...s.turns, turn] };
    }

    case "log": {
      const entry: LogEntry = { source: "agent", level: ev.level, message: ev.message, ts };
      return { ...s, logs: [...s.logs, entry].slice(-MAX_LOGS) };
    }

    default:
      return s; // unknown event types are ignored
  }
}

export class SessionStream {
  readonly sessionId: string;
  private readonly api: Api;
  private ws: SocketLike | null = null;
  private state: StreamState = initialStreamState();
  private listeners = new Set<() => void>();
  private retries = 0;
  private stopped = false;
  private timer: ReturnType<typeof setTimeout> | null = null;

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
    }
    return true;
  }

  private update(fn: (s: StreamState) => StreamState): void {
    const next = fn(this.state);
    if (next === this.state) return;
    this.state = next;
    for (const listener of this.listeners) listener();
  }

  private connect(): void {
    this.update((s) => ({ ...s, connection: this.retries > 0 ? "reconnecting" : "connecting" }));
    const ws = this.api.openEvents(this.sessionId, this.state.lastSeq);
    this.ws = ws;
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
      this.update((s) => reduceFrame(s, frame));
    };
    ws.onerror = () => {};
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      if (this.stopped) return;
      // Answers in flight may not have arrived; let the user resubmit after reconnecting.
      this.update((s) => ({ ...s, connection: "reconnecting", submitting: {} }));
      const delay = Math.min(1000 * 2 ** this.retries, 10_000);
      this.retries += 1;
      this.timer = setTimeout(() => {
        if (!this.stopped) this.connect();
      }, delay);
    };
  }
}

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
}

export interface ToolResultPayload {
  output: string;
  is_error: boolean;
}

/**
 * What a subagent is doing, as its card shows it. `working`: a tool is running. `thinking`: between steps, waiting on
 * its model. `writing`: its report is streaming in. `done`: it ended on text, its report. `continued`: the orchestrator
 * spoke in between, and this subagent's later work is in a card further down.
 */
export type SubagentState = "working" | "thinking" | "writing" | "done" | "continued";

export interface ToolRef {
  name: string;
  input: Record<string, unknown>;
}

export interface SubagentView {
  agent: AgentRef;
  /** The settler name shown for it, numbered when a colony has several of one role. */
  name: string;
  state: SubagentState;
  /** The tool running now, when `working`. */
  current: ToolRef | null;
  /** The last tool it started, for the status line between steps. */
  last: ToolRef | null;
  /** Tool calls in this card. */
  steps: number;
}

/** Reads a subagent card's state from its blocks, as they stand now. */
export function subagentState(blocks: Block[]): Pick<SubagentView, "state" | "current" | "last" | "steps"> {
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
  return { state, current: ref(running), last: ref(lastTool), steps: tools.length };
}

export interface ThreadView {
  messages: ThreadMessageLike[];
  /** The subagent behind a rendered message, keyed by its id; absent means the orchestrator. */
  subagents: Record<string, SubagentView>;
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
  // Settler names are numbered per role in order of first appearance: the second Explore is "Scout Settler 2".
  const names = new Map<string, string>();
  const perRole = new Map<string, number>();
  const nameOf = (agent: AgentRef): string => {
    let name = names.get(agent.id);
    if (!name) {
      // The runner falls back to the task description when the Task call named no type.
      const type = agent.name === agent.description ? null : agent.name;
      const role = settlerName(type);
      const ordinal = (perRole.get(role) ?? 0) + 1;
      perRole.set(role, ordinal);
      name = settlerName(type, ordinal);
      names.set(agent.id, name);
    }
    return name;
  };
  let group: { id: string; blocks: Block[]; ts: string | null; prev: string | null; agent?: AgentRef } | null = null;

  const flush = () => {
    if (!group) return;
    const parts = toParts(group.blocks);
    if (parts.length > 0) {
      messages.push({
        role: "assistant",
        id: group.id,
        content: parts,
        createdAt: group.ts ? new Date(group.ts) : undefined,
      });
      if (group.agent) {
        subagents[group.id] = { agent: group.agent, name: nameOf(group.agent), ...subagentState(group.blocks) };
      }
      emitted.add(group.id);
      lastEmitted = group.id;
    } else {
      fallbackOf.set(group.id, group.prev);
    }
    group = null;
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
      emitted.add(message.id);
      lastEmitted = message.id;
      continue;
    }
    if (group && group.agent?.id !== message.agent?.id) flush();
    if (!group) group = { id: message.id, blocks: [], ts: message.ts, prev: lastEmitted, agent: message.agent };
    group.blocks.push(...message.blocks);
    groupOf.set(message.id, group.id);
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

  return { messages, subagents, turns, notices, hasOpenQuestion };
}
