import {
  AssistantRuntimeProvider,
  ComposerPrimitive,
  MessagePrimitive,
  ThreadPrimitive,
  makeAssistantToolUI,
  useExternalStoreRuntime,
  type AppendMessage,
  type ReasoningMessagePartProps,
  type TextMessagePartProps,
  type ThreadMessageLike,
  type ToolCallMessagePartProps,
} from "@assistant-ui/react";
import { MarkdownTextPrimitive } from "@assistant-ui/react-markdown";
import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { errorMessage, useToast } from "../context";
import { canQueue, droppedText, sendOrQueue, useOutbox } from "../outbox";
import { usePendingTurn } from "../cockpit/turnFocus";
import { useModels } from "../useModels";
import {
  ASK_USER_TOOL,
  END_OF_THREAD,
  buildThread,
  earlierTotals,
  isWatchdogMessageId,
  type AskUserArgs,
  type AskUserResult,
  type MemoryNotice,
  type SessionStream,
  type StreamState,
  type SubagentState,
  type SubagentView,
  type ToolResultPayload,
  type TurnSummary,
} from "../sessionStream";
import type { MemoryScope, Origin } from "../types";
import { AntAvatar, type AntActivity } from "./AntAvatar";
import { antActivity, describeTool, isNoiseTool, toolDetail, type ActivityIcon } from "./activity";
import { AskUserCard, QuestionActionsContext, type QuestionActions } from "./AskUserCard";
import { BoundaryRow } from "./BoundaryRow";
import {
  IconAlert,
  IconBranch,
  IconCheck,
  IconChevron,
  IconCpu,
  IconMemory,
  IconMenu,
  IconNetwork,
  IconPencil,
  IconQuestion,
  IconRefresh,
  IconSearch,
  IconSend,
  IconSpark,
  IconStop,
  IconTerminal,
  IconTrash,
  IconX,
} from "./icons";
import { InlineCode } from "./Markdown";
import { EnterContext, useEnter, useFollowBottom, useSettled } from "./motion";
import { JUMP_AFTER_TURNS, nearTop, restoreAnchor, takeAnchor, underfilled, type Anchor } from "./scrollAnchor";
import { SettlerCard, useStumble } from "./SettlerCard";
import { Spinner, cx, formatDuration, store, stored } from "./ui";
import "../cockpit/turnFocus.css";

/** The harness sends the session's initial prompt as a user message with this id. */
const BRIEF_ID = "initial";

const AskUserToolUI = makeAssistantToolUI<AskUserArgs, AskUserResult>({
  toolName: ASK_USER_TOOL,
  display: "standalone",
  render: AskUserCard,
});

type RenderedMessage = { id: string; content: readonly { type: string; text?: string }[]; createdAt?: Date };

/** A command this panel queued with the service worker while the colony's socket was down. */
interface QueuedEntry {
  id: string;
  kind: "message" | "answer";
  /** The message text, for the queued bubble. */
  text?: string;
}

function messageText(message: RenderedMessage): string {
  return message.content.map((part) => (part.type === "text" ? (part.text ?? "") : "")).join("");
}

export function ChatPanel({
  stream,
  state,
  live,
  onOpenMemory,
  onAnswerFocus,
}: {
  stream: SessionStream | null;
  state: StreamState;
  live: boolean;
  onOpenMemory?: () => void;
  /** The answer box got focus; a suspended colony's open question may want warming (issue #701). */
  onAnswerFocus?: () => void;
}) {
  const toast = useToast();
  const thread = useMemo(() => buildThread(state), [state]);
  const connected = state.connection === "open";
  const isRunning = state.agentState === "working";
  // What this panel has handed to the worker's outbox while offline: shown as queued bubbles and
  // notes until the worker reports each item delivered — or dropped, which is said out loud.
  const outbox = useOutbox();
  const [queued, setQueued] = useState<QueuedEntry[]>([]);
  const queuedRef = useRef<QueuedEntry[]>([]);
  const addQueued = (entry: QueuedEntry) => {
    queuedRef.current = [...queuedRef.current, entry];
    setQueued(queuedRef.current);
  };
  useEffect(() => {
    const { delivered, dropped } = outbox;
    if (delivered.length === 0 && dropped.length === 0) return;
    const gone = new Set<string>([...delivered, ...dropped.map((drop) => drop.id)]);
    for (const drop of dropped) {
      const entry = queuedRef.current.find((queued) => queued.id === drop.id);
      if (entry) toast(droppedText(entry.kind, drop.status), "error");
    }
    const next = queuedRef.current.filter((entry) => !gone.has(entry.id));
    if (next.length !== queuedRef.current.length) {
      queuedRef.current = next;
      setQueued(next);
    }
  }, [outbox, toast]);
  // Plain language by default: most people watching a colony work are not reading the commands.
  const [simple, setSimple] = useState(() => stored("colonizer.chat-simple") !== "0");
  const toggleView = () => {
    const next = !simple;
    setSimple(next);
    store("colonizer.chat-simple", next ? "1" : "0");
  };

  const settled = useSettled(connected, state.lastSeq);
  const { viewportRef, contentRef, stickToBottom } = useFollowBottom();
  const loadEarlier = useLoadEarlier(stream, state, viewportRef);
  // Sending a message brings the reader back to the foot of the thread, wherever they had scrolled to.
  const lastUserId = [...thread.messages].reverse().find((m) => m.role === "user")?.id;
  useEffect(() => {
    if (lastUserId) stickToBottom();
  }, [lastUserId, stickToBottom]);

  // A transcript-search hit lands on its exact turn (issue #739). The request can arrive before the
  // stream has replayed the turn, so the effect re-runs as the thread grows and scrolls only once
  // the message is on screen. `renderOf` maps a bare message id to the bubble it folded into; the
  // request's counter lets the same turn be focused again.
  const focus = usePendingTurn();
  const handledFocus = useRef(0);
  useEffect(() => {
    if (!focus.id || handledFocus.current === focus.n) return;
    const rendered = thread.renderOf[focus.id] ?? focus.id;
    const el = document.getElementById(`turn-${rendered}`);
    if (!el) return;
    handledFocus.current = focus.n;
    el.scrollIntoView({ block: "center", behavior: "smooth" });
    el.classList.remove("turn-flash");
    void el.offsetWidth;
    el.classList.add("turn-flash");
  }, [focus, thread]);

  const runtime = useExternalStoreRuntime<ThreadMessageLike>({
    messages: thread.messages,
    isRunning,
    isDisabled: !live,
    convertMessage: (message) => message,
    onNew: async (message: AppendMessage) => {
      const text = message.content
        .map((part) => (part.type === "text" ? part.text : ""))
        .join("\n")
        .trim();
      if (!text) return;
      const outcome = sendOrQueue(stream, { type: "user_message", text });
      if (outcome.status === "failed") toast("Not connected to the colony — your message was not sent.", "error");
      if (outcome.status === "queued") addQueued({ id: outcome.id, kind: "message", text });
    },
    onCancel: async () => {
      stream?.send({ type: "interrupt" });
    },
  });

  const questionActions = useMemo<QuestionActions>(() => {
    // Offline is no longer a wall (issue #746): with the worker's outbox the answer queues and
    // sends on reconnect, so the card stays open — and owns up to the queueing.
    const queueing = !connected && live && canQueue();
    return {
      answer: (questionId, answers, response, questions) => {
        try {
          const outcome = sendOrQueue(stream, { type: "answer", question_id: questionId, answers, response }, questions);
          if (outcome.status === "failed") toast("Not connected to the colony — try again in a moment.", "error");
          if (outcome.status === "queued") addQueued({ id: outcome.id, kind: "answer" });
          return outcome.status !== "failed";
        } catch (error) {
          toast(errorMessage(error), "error");
          return false;
        }
      },
      submitting: state.submitting,
      canAnswer: (connected || queueing) && live,
      willQueue: queueing,
      blockedBy: !live ? "ended" : !connected && !queueing ? "disconnected" : null,
    };
  }, [stream, state.submitting, connected, live, toast]);

  return (
    <QuestionActionsContext.Provider value={questionActions}>
      <EnterContext.Provider value={settled}>
      <SimpleViewContext.Provider value={simple}>
      <AssistantRuntimeProvider runtime={runtime}>
        <AskUserToolUI />
        <ThreadPrimitive.Root className="flex h-full min-h-0 flex-col">
          {state.connection === "reconnecting" && (
            <div className="flex items-center gap-2 border-b border-border bg-warn-soft px-4 py-1.5 text-small-lg text-warn">
              <Spinner /> Reconnecting to the colony…
            </div>
          )}
          {queued.length > 0 && (
            <div role="status" className="flex items-center gap-2 border-b border-border bg-panel-2 px-4 py-1.5 text-small-lg text-muted">
              <Spinner className="text-faint" /> Queued — sends when you're back online
            </div>
          )}
          <div className="flex shrink-0 items-center justify-end gap-2 border-b border-border px-4 py-1.5">
            <span className="text-small text-faint">{simple ? "Described in plain language" : "Raw commands and output"}</span>
            <button
              type="button"
              onClick={toggleView}
              title={simple ? "Show the exact commands the agent ran" : "Describe each step in plain language"}
              className="cursor-pointer rounded-md border border-border px-2 py-0.5 text-small text-muted hover:bg-panel-2 hover:text-text"
            >
              {simple ? "Show detail" : "Simple view"}
            </button>
          </div>
          {/* useFollowBottom does the following, eased, so the viewport's own instant scrolling is off. */}
          <ThreadPrimitive.Viewport
            ref={(el) => {
              viewportRef.current = el;
            }}
            autoScroll={false}
            scrollToBottomOnRunStart={false}
            tabIndex={0}
            role="log"
            aria-label="Colony chat thread"
            className="scroll-thin min-h-0 flex-1 overflow-y-auto px-4 pb-4 pt-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent-ring)]"
          >
            {thread.messages.length === 0 && <EmptyChat connection={state.connection} live={live} />}
            <div
              ref={(el) => {
                contentRef.current = el;
              }}
            >
              {state.history.hasMore && <EarlierRow state={state} onLoad={loadEarlier.load} onJump={loadEarlier.jumpToStart} />}
              <ThreadPrimitive.Messages>
                {({ message }) => (
                  <div id={`turn-${message.id}`}>
                    {message.role !== "user" ? (
                      thread.subagents[message.id] ? (
                        <SubagentMessage view={thread.subagents[message.id]} subagents={thread.subagents} live={live} />
                      ) : (
                        <AssistantMessage />
                      )
                    ) : message.id === BRIEF_ID ? (
                      <SessionBrief text={messageText(message)} origin={thread.origins[message.id]} />
                    ) : thread.origins[message.id] === "watchdog" || isWatchdogMessageId(message.id) ? (
                      <WatchdogNotice text={messageText(message)} at={message.createdAt} />
                    ) : (
                      <UserMessage origin={thread.origins[message.id]} />
                    )}
                    {thread.turns[message.id]?.map((turn, i) => <TurnNotice key={i} turn={turn} />)}
                    {thread.notices[message.id]?.map((notice) => (
                      <MemoryNoticeRow key={notice.proposal.id} notice={notice} onOpen={onOpenMemory} />
                    ))}
                    {thread.boundaries[message.id]?.map((notice, i) => <BoundaryRow key={i} record={notice.record} />)}
                  </div>
                )}
              </ThreadPrimitive.Messages>
              {thread.turns[END_OF_THREAD]?.map((turn, i) => <TurnNotice key={i} turn={turn} />)}
              {thread.notices[END_OF_THREAD]?.map((notice) => (
                <MemoryNoticeRow key={notice.proposal.id} notice={notice} onOpen={onOpenMemory} />
              ))}
              {thread.boundaries[END_OF_THREAD]?.map((notice, i) => <BoundaryRow key={i} record={notice.record} />)}
              <ActivityLine state={state} hasOpenQuestion={thread.hasOpenQuestion} live={live} />
              {/* What the outbox is still holding from this panel: the messages as queued bubbles,
                  the answer as a note at the foot, until the worker reports them delivered. */}
              {queued.map((entry) => (entry.kind === "message" ? <QueuedMessage key={entry.id} text={entry.text ?? ""} /> : <QueuedAnswerNote key={entry.id} />))}
            </div>
          </ThreadPrimitive.Viewport>
          <Composer
            isRunning={isRunning}
            live={live}
            waiting={thread.hasOpenQuestion}
            picker={<ModelPicker stream={stream} state={state} enabled={connected && live} />}
            onFocusAnswer={onAnswerFocus}
          />
        </ThreadPrimitive.Root>
      </AssistantRuntimeProvider>
      </SimpleViewContext.Provider>
      </EnterContext.Provider>
    </QuestionActionsContext.Provider>
  );
}

/**
 * Loads the page of events before the oldest one held when the reader scrolls near the top (or the thread is
 * too short to scroll), and keeps their place: the thread's height before the prepend is remembered and the
 * view moved by what was added above it, so nothing jumps (issue #1210).
 */
function useLoadEarlier(stream: SessionStream | null, state: StreamState, viewportRef: { current: HTMLElement | null }) {
  const anchor = useRef<Anchor | null>(null);
  const { hasMore, loading, error, loaded } = state.history;
  const load = useCallback(() => {
    const el = viewportRef.current;
    if (!el || !stream || !hasMore || loading) return;
    anchor.current = takeAnchor(el);
    void stream.loadOlder();
  }, [stream, hasMore, loading, viewportRef]);

  // An older page just went in above the reader: put the view back where it was, and once more a frame
  // later for content (markdown, code) that settles its height after the first layout.
  useLayoutEffect(() => {
    const el = viewportRef.current;
    const held = anchor.current;
    if (!el || !held || loaded === 0) return;
    restoreAnchor(el, held);
    const frame = requestAnimationFrame(() => {
      if (anchor.current === held) {
        restoreAnchor(el, held);
        anchor.current = null;
      }
    });
    return () => cancelAnimationFrame(frame);
  }, [loaded, viewportRef]);

  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const onScroll = () => {
      if (nearTop(el.scrollTop)) load();
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [load, viewportRef]);

  // A thread too short to scroll never fires a scroll event, so older pages are fetched until it fills.
  useEffect(() => {
    const el = viewportRef.current;
    if (el && hasMore && !loading && !error && underfilled(el)) load();
  }, [hasMore, loading, error, loaded, state.messages.length, load, viewportRef]);

  const jumpToStart = useCallback(async () => {
    if (!stream) return;
    await stream.loadAllOlder();
    requestAnimationFrame(() => {
      if (viewportRef.current) viewportRef.current.scrollTop = 0;
    });
  }, [stream, viewportRef]);

  return { load, jumpToStart };
}

/** The row above the first loaded message while older ones are still on record. */
function EarlierRow({ state, onLoad, onJump }: { state: StreamState; onLoad: () => void; onJump: () => void }) {
  const { loading, error } = state.history;
  const earlier = earlierTotals(state);
  const detail = [
    earlier.turns > 0 ? `${earlier.turns} earlier ${earlier.turns === 1 ? "turn" : "turns"}` : null,
    earlier.costUsd != null ? `$${earlier.costUsd.toFixed(2)} so far` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <div role="status" className="flex items-center justify-center gap-3 py-2 text-small-lg text-muted">
      {loading ? (
        <>
          <Spinner className="text-faint" /> Loading earlier…
        </>
      ) : (
        <>
          <button
            type="button"
            onClick={onLoad}
            className="cursor-pointer rounded-md border border-border px-2 py-0.5 text-small text-muted hover:bg-panel-2 hover:text-text"
          >
            {error ? "Could not load earlier messages. Retry" : "Load earlier messages"}
          </button>
          {detail && <span className="text-small text-faint">{detail}</span>}
          {earlier.turns >= JUMP_AFTER_TURNS && (
            <button type="button" onClick={onJump} className="cursor-pointer text-small text-accent hover:underline">
              Jump to start
            </button>
          )}
        </>
      )}
    </div>
  );
}

function EmptyChat({ connection, live }: { connection: StreamState["connection"]; live: boolean }) {
  return (
    <div className="grid h-full min-h-48 place-items-center text-center text-body-sm text-muted">
      <div className="flex flex-col items-center gap-2">
        {connection === "open" || !live ? (
          <>
            <IconSpark size={20} className="text-faint" />
            {live ? "Waiting for the agent to start…" : "No conversation was recorded for this colony."}
          </>
        ) : (
          <>
            <Spinner className="text-accent" />
            Connecting to the session…
          </>
        )}
      </div>
    </div>
  );
}

/** The brief's label by the origin that launched the colony (issue #312): the scheduler's and the red
 *  team's briefs are not the operator's words. */
const BRIEF_LABEL: Partial<Record<Origin, string>> = { burn_down: "Burn-down brief", redteam: "Red-team brief" };

function SessionBrief({ text, origin }: { text: string; origin?: Origin }) {
  const enter = useEnter();
  const trimmed = text.trim();
  const firstLine = trimmed.split("\n").find((line) => line.trim()) ?? "";
  return (
    <MessagePrimitive.Root className={cx("my-3", enter)}>
      <details className="group rounded-xl border border-border bg-panel text-body-sm">
        <summary className="flex cursor-pointer list-none items-center gap-2 rounded-xl px-3 py-2 hover:bg-panel-2 [&::-webkit-details-marker]:hidden">
          <IconChevron size={13} className="shrink-0 text-faint transition-transform group-open:rotate-90" />
          <span className="shrink-0 text-meta font-semibold uppercase tracking-wide text-muted">
            {(origin && BRIEF_LABEL[origin]) || "Colony brief"}
          </span>
          <span className="min-w-0 flex-1 truncate text-muted">{firstLine}</span>
        </summary>
        <div className="scroll-thin max-h-96 overflow-y-auto whitespace-pre-wrap border-t border-border px-3 py-2.5 leading-relaxed text-muted [overflow-wrap:anywhere]">
          {trimmed}
        </div>
      </details>
    </MessagePrimitive.Root>
  );
}

/** A watchdog nudge (§6.3) is an operator notice, never shown as something the user said. */
function WatchdogNotice({ text, at }: { text: string; at?: Date }) {
  const enter = useEnter();
  return (
    <MessagePrimitive.Root className={cx("my-3", enter)}>
      <details className="group rounded-lg border border-warn/25 bg-warn-soft text-small-lg text-warn">
        <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-3 py-1.5 [&::-webkit-details-marker]:hidden">
          <IconAlert size={13} className="shrink-0" />
          <span className="min-w-0 flex-1 truncate font-medium">Watchdog nudged the agent after no progress</span>
          {at && (
            <span className="shrink-0 text-meta-lg opacity-75">{at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>
          )}
          <IconChevron size={13} className="shrink-0 opacity-75 transition-transform group-open:rotate-90" />
        </summary>
        <div className="whitespace-pre-wrap border-t border-warn/20 px-3 py-2 leading-relaxed text-muted [overflow-wrap:anywhere]">
          {text.trim()}
        </div>
      </details>
    </MessagePrimitive.Root>
  );
}

const SCOPE_WORD: Record<MemoryScope, string> = { global: "global", org: "org", repo: "repository" };

function MemoryNoticeRow({ notice, onOpen }: { notice: MemoryNotice; onOpen?: () => void }) {
  const enter = useEnter();
  const { proposal } = notice;
  return (
    <div className={cx(enter, "my-2 ml-10 flex flex-wrap items-center gap-x-2 gap-y-1 rounded-lg border border-border bg-panel-2/60 px-3 py-1.5 text-small-lg text-muted")}>
      <IconMemory size={13} className="shrink-0 text-accent" />
      <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">
        Proposed a {SCOPE_WORD[proposal.scope] ?? proposal.scope} memory: <span className="font-medium text-text"><InlineCode text={proposal.title} /></span>
      </span>
      {onOpen && (
        <button type="button" onClick={onOpen} className="shrink-0 cursor-pointer font-medium text-accent hover:underline">
          Review in Memory
        </button>
      )}
    </div>
  );
}

/** The tag a non-operator turn carries above its bubble (issue #312); the operator's own need none. */
const ORIGIN_LABEL: Partial<Record<Origin, string>> = {
  autonomy: "autonomy judge",
  burn_down: "burn-down",
  redteam: "red-team",
  notify: "notification",
  system: "system",
};

function UserMessage({ origin }: { origin?: Origin }) {
  const enter = useEnter();
  const label = origin && origin !== "user" ? (ORIGIN_LABEL[origin] ?? origin) : null;
  return (
    <MessagePrimitive.Root className={cx("my-4 flex justify-end", enter)}>
      <div className="max-w-[85%]">
        {label && (
          <div className="mb-1 text-right text-meta font-semibold uppercase tracking-wide text-faint">{label}</div>
        )}
        <div className="whitespace-pre-wrap break-words rounded-2xl rounded-br-md bg-accent-soft px-3.5 py-2 text-body-lg leading-relaxed">
          <MessagePrimitive.Parts />
        </div>
      </div>
    </MessagePrimitive.Root>
  );
}

/** A message the socket could not take, drawn where it will sit once the worker delivers it — dashed until then. */
function QueuedMessage({ text }: { text: string }) {
  return (
    <div className="my-4 flex justify-end">
      <div className="max-w-[85%]">
        <div className="mb-1 text-right text-meta font-semibold uppercase tracking-wide text-faint">Queued</div>
        <div className="whitespace-pre-wrap break-words rounded-2xl rounded-br-md border border-dashed border-border-strong bg-accent-soft/60 px-3.5 py-2 text-body-lg leading-relaxed text-muted">
          {text}
        </div>
      </div>
    </div>
  );
}

/** The note under an answer queued while offline: the card above stays open until the worker's POST lands. */
function QueuedAnswerNote() {
  return (
    <div className="flex flex-wrap items-center gap-2 py-2 pl-10 text-body-sm font-medium text-accent">
      <IconQuestion size={15} /> Your answer was queued — it sends when you're back online
    </div>
  );
}

/**
 * Streamed text is revealed at a steady pace rather than as it arrives. Colonies' deltas come in bursts, 150–250
 * characters at once and then up to half a second of nothing; the default reveal (250 ms to catch up) typed each burst
 * out in a rush. Spreading the backlog over 700 ms evens that out, and 8 ms per character at most keeps the end of a
 * message from trailing on after the stream has finished.
 */
const SMOOTH_TEXT = { drainMs: 700, maxCharIntervalMs: 8 };

function MarkdownText() {
  return <MarkdownTextPrimitive smooth={SMOOTH_TEXT} className="md break-words text-body-lg leading-relaxed" />;
}

/** A settler's report, which its card already shows in the report box, so the transcript under it leaves it out. */
const SettlerReportContext = createContext("");

function SettlerText({ text }: TextMessagePartProps) {
  const report = useContext(SettlerReportContext);
  return report && text.trim() && report.includes(text.trim()) ? null : <MarkdownText />;
}

function ReasoningPart({ text }: ReasoningMessagePartProps) {
  if (!text?.trim()) return null;
  return (
    <details className="group rounded-lg text-body-sm text-muted">
      <summary className="flex cursor-pointer list-none items-center gap-1.5 py-0.5 [&::-webkit-details-marker]:hidden">
        <IconChevron size={13} className="transition-transform group-open:rotate-90" />
        Thinking
      </summary>
      <div className="whitespace-pre-wrap border-l-2 border-border pl-3">{text}</div>
    </details>
  );
}

/** Simple view: one sentence per step, with the exact command one click away. Technical view: the raw call. */
const SimpleViewContext = createContext(true);

const ACTIVITY_ICONS: Record<ActivityIcon, typeof IconTerminal> = {
  run: IconTerminal,
  read: IconSearch,
  edit: IconPencil,
  search: IconSearch,
  download: IconRefresh,
  web: IconNetwork,
  memory: IconMemory,
  test: IconCpu,
  git: IconBranch,
  clean: IconTrash,
  list: IconMenu,
  agent: IconSpark,
};

function toolSummary(input: Record<string, unknown>): string {
  const pick = input.command ?? input.file_path ?? input.pattern ?? input.url ?? input.query ?? input.description ?? input.prompt;
  const text = typeof pick === "string" ? pick : JSON.stringify(input);
  return text.length > 180 ? `${text.slice(0, 180)}…` : text;
}

function ToolCallCard({ toolName, args, result, isError }: ToolCallMessagePartProps) {
  const simple = useContext(SimpleViewContext);
  const enter = useEnter();
  const payload = result as ToolResultPayload | undefined;
  const done = payload !== undefined;
  const failed = Boolean(isError || payload?.is_error);
  const input = (args ?? {}) as Record<string, unknown>;
  // A step that says nothing to a reader, and didn't fail, is just noise in the simple view.
  if (simple && !failed && isNoiseTool(toolName, input)) return null;
  const activity = describeTool(toolName, input);
  const ActivityGlyph = ACTIVITY_ICONS[activity.icon];
  return (
    <details className={cx("group my-1.5 rounded-xl border border-border bg-panel-2/50 text-body-sm open:bg-panel-2", enter)}>
      <summary className="flex cursor-pointer list-none items-center gap-2 rounded-xl px-3 py-2 [&::-webkit-details-marker]:hidden">
        <span
          className={cx(
            "grid size-5 shrink-0 place-items-center rounded-md",
            failed ? "bg-err-soft text-err" : done ? "bg-ok-soft text-ok" : "bg-info-soft text-info",
          )}
        >
          {done ? failed ? <IconX size={12} strokeWidth={3} /> : <IconCheck size={12} strokeWidth={3} /> : <Spinner className="size-3" />}
        </span>
        {simple ? (
          <>
            <ActivityGlyph size={13} className="shrink-0 text-faint" />
            <span className="min-w-0 flex-1 truncate">
              {activity.label}
              {failed && <span className="ml-1.5 text-err">— that didn't work</span>}
            </span>
          </>
        ) : (
          <>
            <span className="shrink-0 font-mono text-small-lg font-semibold text-accent">{toolName}</span>
            <span className="min-w-0 flex-1 truncate font-mono text-small-lg text-muted">{toolSummary(input)}</span>
          </>
        )}
        <IconChevron size={14} className="shrink-0 text-faint transition-transform group-open:rotate-90" />
      </summary>
      <div className="space-y-2 border-t border-border px-3 py-2">
        {simple && <p className="font-mono text-small font-semibold text-accent">{toolName}</p>}
        <pre className="scroll-thin max-h-48 overflow-auto whitespace-pre-wrap break-words font-mono text-small text-muted">
          {JSON.stringify(input, null, 2)}
        </pre>
        {done && (
          <pre
            className={cx(
              "scroll-thin max-h-72 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-border bg-panel p-2 font-mono text-small",
              failed ? "text-err" : "text-text",
            )}
          >
            {payload?.output || "(no output)"}
          </pre>
        )}
      </div>
    </details>
  );
}

/** What a settler is doing as its card shows it: a colony that is no longer running has no settler still at work. */
function settlerState(view: SubagentView, live: boolean): SubagentState | "paused" {
  return !live && view.state !== "done" && view.state !== "continued" ? "paused" : view.state;
}

/** What the ant carries: only read while it is working. */
function settlerActivity(view: SubagentView): AntActivity {
  return view.current ? antActivity(describeTool(view.current.name, view.current.input)) : "run";
}

/** The line under a settler's name: what it is doing right now, in plain words. */
function subagentStatus(view: SubagentView, state: SubagentState | "paused"): string {
  const describe = (tool: { name: string; input: Record<string, unknown> } | null) => (tool ? describeTool(tool.name, tool.input).label : null);
  switch (state) {
    case "working":
      return `${describe(view.current) ?? "Working"}…`;
    case "thinking":
      return view.steps === 0 ? "Getting its bearings…" : "Thinking about the next step…";
    case "writing":
      return "Writing its report…";
    case "done":
      return "Done";
    case "continued":
      return "Carried on further down";
    case "paused":
      return view.last ? `Stopped while ${describe(view.last)?.toLowerCase()}` : "Stopped";
  }
}

/**
 * A settler — a subagent — as one compact card: an ant animated by what it is doing, its settler name and task, and
 * one live status line. Its report and steps open on request, so a colony that delegates reads as a few busy settlers
 * rather than pages of their output. Settlers sent out together hang off one rail under a strip of their ants.
 */
function SubagentMessage({ view, subagents, live }: { view: SubagentView; subagents: Record<string, SubagentView>; live: boolean }) {
  const simple = useContext(SimpleViewContext);
  const enter = useEnter();
  const state = settlerState(view, live);
  const error = useStumble(view.errors);
  const crew = view.crew;
  // The simple view lists the steps in plain words; the technical view shows the raw calls and everything said between.
  const stepList = simple
    ? view.tools
        .filter((tool) => tool.failed || !isNoiseTool(tool.name, tool.input))
        .map((tool) => ({
          label: describeTool(tool.name, tool.input).label,
          detail: toolDetail(tool.name, tool.input),
          failed: tool.failed,
          running: tool.running,
        }))
    : [];
  const card = (
    <SettlerCard
      state={state}
      role={view.role}
      activity={settlerActivity(view)}
      error={error}
      name={view.name}
      task={view.agent.description ?? undefined}
      status={subagentStatus(view, state)}
      steps={view.steps}
      report={view.report}
      stepList={stepList}
      phase={crew?.index ?? 0}
    >
      {!simple && (
        <SettlerReportContext.Provider value={view.report}>
          <MessagePrimitive.Parts
            components={{
              Text: SettlerText,
              Reasoning: ReasoningPart,
              tools: { Fallback: ToolCallCard },
            }}
          />
        </SettlerReportContext.Provider>
      )}
    </SettlerCard>
  );
  if (!crew) return <MessagePrimitive.Root className={cx("my-3 ml-4", enter)}>{card}</MessagePrimitive.Root>;
  const first = crew.index === 0;
  const last = crew.index === crew.ids.length - 1;
  return (
    <MessagePrimitive.Root className={cx("ml-4", first && "mt-3", last && "mb-3", enter)}>
      {first && <CrewStrip views={crew.ids.map((id) => subagents[id]).filter(Boolean)} live={live} />}
      <div className="ml-3.5 border-l-2 border-border pl-3.5 pt-2.5">{card}</div>
    </MessagePrimitive.Root>
  );
}

/** The head of a crew: its ants side by side on one trail, so parallel work reads as one crew, not blinking cards. */
function CrewStrip({ views, live }: { views: SubagentView[]; live: boolean }) {
  const states = views.map((view) => settlerState(view, live));
  const count = (match: (state: SubagentState | "paused") => boolean) => states.filter(match).length;
  const atWork = count((state) => state === "working" || state === "thinking" || state === "writing");
  const done = count((state) => state === "done");
  const stopped = count((state) => state === "paused");
  const summary = [
    `${views.length} settlers`,
    atWork > 0 && `${atWork} at work`,
    done > 0 && (done === views.length ? "all done" : `${done} done`),
    stopped > 0 && `${stopped} stopped`,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <div className="relative flex h-11 items-end gap-1.5 px-3.5 pt-2">
      <svg className="settler-crew-ground" aria-hidden="true">
        <line x1="0" y1="1" x2="100%" y2="1" />
      </svg>
      {views.map((view, index) => (
        <CrewAnt key={view.agent.id} view={view} state={states[index]} index={index} />
      ))}
      <span className="ml-auto pb-2 font-mono text-meta-lg text-muted">{summary}</span>
    </div>
  );
}

function CrewAnt({ view, state, index }: { view: SubagentView; state: SubagentState | "paused"; index: number }) {
  const error = useStumble(view.errors);
  return (
    <span className="block h-8" title={view.name}>
      <AntAvatar state={state} role={view.role} activity={settlerActivity(view)} error={error} phase={index} ground={false} framed={false} size={40} />
    </span>
  );
}

function AssistantMessage() {
  const enter = useEnter();
  return (
    <MessagePrimitive.Root className={cx("my-4 flex gap-3", enter)}>
      <div className="mt-0.5 grid size-7 shrink-0 place-items-center rounded-lg bg-panel-3 text-accent">
        <IconSpark size={15} />
      </div>
      <div className="min-w-0 flex-1 space-y-1.5">
        <MessagePrimitive.Parts
          components={{
            Text: MarkdownText,
            Reasoning: ReasoningPart,
            tools: { Fallback: ToolCallCard },
          }}
        />
      </div>
    </MessagePrimitive.Root>
  );
}

function TurnNotice({ turn }: { turn: TurnSummary }) {
  const enter = useEnter();
  // The model that did the substantive work leads; "+N" says others were involved, and the tooltip names them.
  const [primary] = turn.models;
  const model = primary == null ? null : turn.models.length > 1 ? `${primary} +${turn.models.length - 1}` : primary;
  const modelTitle = turn.models.length > 1 ? turn.models.join(" · ") : undefined;
  const meta = [turn.durationMs != null ? formatDuration(turn.durationMs) : null, turn.costUsd != null ? `$${turn.costUsd.toFixed(2)} total` : null]
    .filter(Boolean)
    .join(" · ");
  if (turn.isError) {
    const result = turn.result?.trim();
    return (
      <div role="alert" className={cx("my-3 ml-10 rounded-lg border border-err/30 bg-err-soft px-3 py-2 text-body-sm text-err", enter)}>
        <div className="flex flex-wrap items-center gap-x-2">
          <IconX size={13} strokeWidth={3} />
          <span className="font-semibold">Turn failed</span>
          {model && (
            <span className="min-w-0 max-w-full truncate font-mono text-small opacity-75" title={modelTitle}>
              · {model}
            </span>
          )}
          {meta && <span className="text-small opacity-75">{model && "· "}{meta}</span>}
        </div>
        {result && (
          <div className="mt-1 whitespace-pre-wrap [overflow-wrap:anywhere]">{result.length > 600 ? `${result.slice(0, 600)}…` : result}</div>
        )}
      </div>
    );
  }
  return (
    <div className={cx("-mt-2 mb-3 ml-10 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-small text-faint", enter)}>
      <span className="font-medium text-ok">Turn complete</span>
      {model && (
        <span className="min-w-0 max-w-full truncate font-mono" title={modelTitle}>
          · {model}
        </span>
      )}
      {meta && <span>· {meta}</span>}
    </div>
  );
}

const OPEN_QUESTION = "[data-open-question]";

/**
 * The foot of the thread while a question is open. The card itself sits wherever the agent asked,
 * which can be tens of thousands of characters up if the agent kept writing afterwards, so this
 * offers the way back to it — but only when it is actually out of sight.
 */
function WaitingForAnswer() {
  const enter = useEnter();
  const [offscreen, setOffscreen] = useState(false);

  useEffect(() => {
    let observer: IntersectionObserver | null = null;
    // The status event can arrive before the card has rendered, so keep looking for a moment.
    const attach = () => {
      const card = document.querySelector(OPEN_QUESTION);
      if (!card) return false;
      observer = new IntersectionObserver(([entry]) => setOffscreen(!entry.isIntersecting), { threshold: 0.15 });
      observer.observe(card);
      return true;
    };
    if (attach()) return () => observer?.disconnect();
    const timer = setInterval(() => {
      if (attach()) clearInterval(timer);
    }, 250);
    return () => {
      clearInterval(timer);
      observer?.disconnect();
    };
  }, []);

  const jump = () => {
    const card = document.querySelector<HTMLElement>(OPEN_QUESTION);
    if (!card) return;
    card.scrollIntoView({ behavior: "smooth", block: "center" });
    card.focus({ preventScroll: true });
  };

  return (
    <div className={cx("flex flex-wrap items-center gap-2 py-2 pl-10 text-body-sm font-medium text-accent", enter)}>
      <IconQuestion size={15} /> Waiting for your answer
      {offscreen && (
        <button
          type="button"
          onClick={jump}
          className="cursor-pointer rounded-md border border-accent/40 px-2 py-0.5 text-small font-medium hover:bg-accent-soft"
        >
          Jump to the question
        </button>
      )}
    </div>
  );
}

function ActivityLine({ state, hasOpenQuestion, live }: { state: StreamState; hasOpenQuestion: boolean; live: boolean }) {
  if (!live) return null;
  if (hasOpenQuestion || state.agentState === "waiting_for_answer") {
    return <WaitingForAnswer />;
  }
  if (state.agentState === "working") return <WorkingLine detail={state.agentDetail} />;
  if (state.agentState === "error" || state.agentState === "exited") {
    return (
      <div className="py-2 pl-10 text-body-sm text-err">
        Agent {state.agentState === "error" ? "reported an error" : "exited"}
        {state.agentDetail ? `: ${state.agentDetail}` : ""}
      </div>
    );
  }
  return null;
}

function WorkingLine({ detail }: { detail: string | null | undefined }) {
  const enter = useEnter();
  return (
    <div className={cx("flex items-center gap-2 py-2 pl-10 text-body-sm text-muted", enter)}>
      <Spinner className="text-accent" /> {detail || "Working in the microVM…"}
    </div>
  );
}

/**
 * Switches the model a live colony uses for its next turns, keeping the conversation. The shown model is only ever
 * the runner's latest `model_changed`, never the choice itself; a runner that never reports one can't switch, so
 * the picker stays hidden.
 */
function ModelPicker({ stream, state, enabled }: { stream: SessionStream | null; state: StreamState; enabled: boolean }) {
  const toast = useToast();
  const models = useModels();
  const { model, switchingModel, refusedModel } = state;
  // Toast a refusal seen while mounted; one already in the state when the panel mounts was shown before.
  const shownRefusal = useRef(refusedModel);
  useEffect(() => {
    if (refusedModel && refusedModel !== shownRefusal.current) toast(`Could not switch to ${refusedModel} — see the logs.`, "error");
    shownRefusal.current = refusedModel;
  }, [refusedModel, toast]);
  if (!model) return null;
  // The runner may report an id the suggestions don't list, such as a routed model.
  const options = models.some((m) => m.id === model) ? models : [{ id: model, label: model }, ...models];
  const choose = (next: string) => {
    if (next !== model && !stream?.send({ type: "set_model", model: next })) {
      toast("Not connected to the colony — the model was not switched.", "error");
    }
  };
  return (
    <span
      className="flex shrink-0 items-center gap-1.5"
      title={switchingModel ? `Switching to ${switchingModel}…` : "Switch the model for the next turns; the conversation is kept."}
    >
      {switchingModel && <Spinner className="text-accent" />}
      <span role="status" className="sr-only">
        {switchingModel && `Switching to ${switchingModel}…`}
      </span>
      <select
        value={model}
        onChange={(e) => choose(e.target.value)}
        disabled={!enabled || switchingModel !== null}
        aria-busy={switchingModel !== null}
        aria-label="Model for the next turns"
        className="h-9 max-w-40 cursor-pointer truncate rounded-xl border border-border bg-panel-2 px-2 text-small-lg text-muted outline-none hover:text-text focus:border-accent disabled:cursor-not-allowed disabled:opacity-60"
      >
        {options.map((m) => (
          <option key={m.id} value={m.id}>
            {m.label}
          </option>
        ))}
      </select>
    </span>
  );
}

function Composer({
  isRunning,
  live,
  waiting,
  picker,
  onFocusAnswer,
}: {
  isRunning: boolean;
  live: boolean;
  waiting: boolean;
  picker: ReactNode;
  onFocusAnswer?: () => void;
}) {
  return (
    <div className="border-t border-border bg-panel/60 p-3">
      <ComposerPrimitive.Root className="flex items-end gap-2 rounded-2xl border border-border bg-panel p-1.5 pl-3 shadow-[var(--shadow)] focus-within:border-accent">
        <ComposerPrimitive.Input
          rows={1}
          onFocus={onFocusAnswer}
          placeholder={
            !live
              ? "This colony's microVM is not running"
              : waiting
                ? "Answer the card above, or send a note to the agent…"
                : isRunning
                  ? "Send a follow-up — the agent reads it next…"
                  : "Message the agent…"
          }
          className="scroll-thin max-h-40 min-h-9 min-w-0 flex-1 resize-none bg-transparent py-2 text-body-lg leading-5 outline-none placeholder:text-faint"
        />
        {picker}
        {isRunning ? (
          <ComposerPrimitive.Cancel
            className="grid size-9 shrink-0 cursor-pointer place-items-center rounded-xl border border-border bg-panel-2 text-text hover:bg-panel-3"
            aria-label="Stop the agent"
            title="Stop (interrupt)"
          >
            <IconStop size={15} />
          </ComposerPrimitive.Cancel>
        ) : (
          <ComposerPrimitive.Send
            className="grid size-9 shrink-0 cursor-pointer place-items-center rounded-xl bg-accent text-on-accent hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-40"
            aria-label="Send message"
          >
            <IconSend size={16} />
          </ComposerPrimitive.Send>
        )}
      </ComposerPrimitive.Root>
    </div>
  );
}
