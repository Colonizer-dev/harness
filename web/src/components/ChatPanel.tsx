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
import { createContext, useContext, useEffect, useMemo, useState } from "react";
import { errorMessage, useToast } from "../context";
import {
  ASK_USER_TOOL,
  END_OF_THREAD,
  buildThread,
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
import type { MemoryScope } from "../types";
import { AntAvatar, type AntActivity } from "./AntAvatar";
import { antActivity, describeTool, isNoiseTool, toolDetail, type ActivityIcon } from "./activity";
import { AskUserCard, QuestionActionsContext, type QuestionActions } from "./AskUserCard";
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
import { SettlerCard, useStumble } from "./SettlerCard";
import { Spinner, cx, formatDuration, store, stored } from "./ui";

/** The harness sends the session's initial prompt as a user message with this id. */
const BRIEF_ID = "initial";

const AskUserToolUI = makeAssistantToolUI<AskUserArgs, AskUserResult>({
  toolName: ASK_USER_TOOL,
  display: "standalone",
  render: AskUserCard,
});

type RenderedMessage = { id: string; content: readonly { type: string; text?: string }[]; createdAt?: Date };

function messageText(message: RenderedMessage): string {
  return message.content.map((part) => (part.type === "text" ? (part.text ?? "") : "")).join("");
}

export function ChatPanel({
  stream,
  state,
  live,
  onOpenMemory,
}: {
  stream: SessionStream | null;
  state: StreamState;
  live: boolean;
  onOpenMemory?: () => void;
}) {
  const toast = useToast();
  const thread = useMemo(() => buildThread(state), [state]);
  const connected = state.connection === "open";
  const isRunning = state.agentState === "working";
  // Plain language by default: most people watching a colony work are not reading the commands.
  const [simple, setSimple] = useState(() => stored("colonizer.chat-simple") !== "0");
  const toggleView = () => {
    const next = !simple;
    setSimple(next);
    store("colonizer.chat-simple", next ? "1" : "0");
  };

  const settled = useSettled(connected, state.lastSeq);
  const { viewportRef, contentRef, stickToBottom } = useFollowBottom();
  // Sending a message brings the reader back to the foot of the thread, wherever they had scrolled to.
  const lastUserId = [...thread.messages].reverse().find((m) => m.role === "user")?.id;
  useEffect(() => {
    if (lastUserId) stickToBottom();
  }, [lastUserId, stickToBottom]);

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
      if (!stream?.send({ type: "user_message", text })) {
        toast("Not connected to the colony — your message was not sent.", "error");
      }
    },
    onCancel: async () => {
      stream?.send({ type: "interrupt" });
    },
  });

  const questionActions = useMemo<QuestionActions>(
    () => ({
      answer: (questionId, answers, response) => {
        try {
          const sent = stream?.send({ type: "answer", question_id: questionId, answers, response }) ?? false;
          if (!sent) toast("Not connected to the colony — try again in a moment.", "error");
          return sent;
        } catch (error) {
          toast(errorMessage(error), "error");
          return false;
        }
      },
      submitting: state.submitting,
      canAnswer: connected && live,
      blockedBy: !live ? "ended" : !connected ? "disconnected" : null,
    }),
    [stream, state.submitting, connected, live, toast],
  );

  return (
    <QuestionActionsContext.Provider value={questionActions}>
      <EnterContext.Provider value={settled}>
      <SimpleViewContext.Provider value={simple}>
      <AssistantRuntimeProvider runtime={runtime}>
        <AskUserToolUI />
        <ThreadPrimitive.Root className="flex h-full min-h-0 flex-col">
          {state.connection === "reconnecting" && (
            <div className="flex items-center gap-2 border-b border-border bg-warn-soft px-4 py-1.5 text-[12.5px] text-warn">
              <Spinner /> Reconnecting to the colony…
            </div>
          )}
          <div className="flex shrink-0 items-center justify-end gap-2 border-b border-border px-4 py-1.5">
            <span className="text-[12px] text-faint">{simple ? "Described in plain language" : "Raw commands and output"}</span>
            <button
              type="button"
              onClick={toggleView}
              title={simple ? "Show the exact commands the agent ran" : "Describe each step in plain language"}
              className="cursor-pointer rounded-md border border-border px-2 py-0.5 text-[12px] text-muted hover:bg-panel-2 hover:text-text"
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
            className="scroll-thin min-h-0 flex-1 overflow-y-auto px-4 pb-4 pt-2"
          >
            {thread.messages.length === 0 && <EmptyChat connection={state.connection} live={live} />}
            <div
              ref={(el) => {
                contentRef.current = el;
              }}
            >
              <ThreadPrimitive.Messages>
                {({ message }) => (
                  <>
                    {message.role !== "user" ? (
                      thread.subagents[message.id] ? (
                        <SubagentMessage view={thread.subagents[message.id]} subagents={thread.subagents} live={live} />
                      ) : (
                        <AssistantMessage />
                      )
                    ) : message.id === BRIEF_ID ? (
                      <SessionBrief text={messageText(message)} />
                    ) : isWatchdogMessageId(message.id) ? (
                      <WatchdogNotice text={messageText(message)} at={message.createdAt} />
                    ) : (
                      <UserMessage />
                    )}
                    {thread.turns[message.id]?.map((turn, i) => <TurnNotice key={i} turn={turn} />)}
                    {thread.notices[message.id]?.map((notice) => (
                      <MemoryNoticeRow key={notice.proposal.id} notice={notice} onOpen={onOpenMemory} />
                    ))}
                  </>
                )}
              </ThreadPrimitive.Messages>
              {thread.turns[END_OF_THREAD]?.map((turn, i) => <TurnNotice key={i} turn={turn} />)}
              {thread.notices[END_OF_THREAD]?.map((notice) => (
                <MemoryNoticeRow key={notice.proposal.id} notice={notice} onOpen={onOpenMemory} />
              ))}
              <ActivityLine state={state} hasOpenQuestion={thread.hasOpenQuestion} live={live} />
            </div>
          </ThreadPrimitive.Viewport>
          <Composer isRunning={isRunning} live={live} waiting={thread.hasOpenQuestion} />
        </ThreadPrimitive.Root>
      </AssistantRuntimeProvider>
      </SimpleViewContext.Provider>
      </EnterContext.Provider>
    </QuestionActionsContext.Provider>
  );
}

function EmptyChat({ connection, live }: { connection: StreamState["connection"]; live: boolean }) {
  return (
    <div className="grid h-full min-h-48 place-items-center text-center text-[13px] text-muted">
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

function SessionBrief({ text }: { text: string }) {
  const enter = useEnter();
  const trimmed = text.trim();
  const firstLine = trimmed.split("\n").find((line) => line.trim()) ?? "";
  return (
    <MessagePrimitive.Root className={cx("my-3", enter)}>
      <details className="group rounded-xl border border-border bg-panel text-[13px]">
        <summary className="flex cursor-pointer list-none items-center gap-2 rounded-xl px-3 py-2 hover:bg-panel-2 [&::-webkit-details-marker]:hidden">
          <IconChevron size={13} className="shrink-0 text-faint transition-transform group-open:rotate-90" />
          <span className="shrink-0 text-[11px] font-semibold uppercase tracking-wide text-muted">Colony brief</span>
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
      <details className="group rounded-lg border border-warn/25 bg-warn-soft text-[12.5px] text-warn">
        <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-3 py-1.5 [&::-webkit-details-marker]:hidden">
          <IconAlert size={13} className="shrink-0" />
          <span className="min-w-0 flex-1 truncate font-medium">Watchdog nudged the agent after no progress</span>
          {at && (
            <span className="shrink-0 text-[11.5px] opacity-75">{at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>
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
    <div className={cx(enter, "my-2 ml-10 flex flex-wrap items-center gap-x-2 gap-y-1 rounded-lg border border-border bg-panel-2/60 px-3 py-1.5 text-[12.5px] text-muted")}>
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

function UserMessage() {
  const enter = useEnter();
  return (
    <MessagePrimitive.Root className={cx("my-4 flex justify-end", enter)}>
      <div className="max-w-[85%] whitespace-pre-wrap break-words rounded-2xl rounded-br-md bg-accent-soft px-3.5 py-2 text-[14px] leading-relaxed">
        <MessagePrimitive.Parts />
      </div>
    </MessagePrimitive.Root>
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
  return <MarkdownTextPrimitive smooth={SMOOTH_TEXT} className="md break-words text-[14px] leading-relaxed" />;
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
    <details className="group rounded-lg text-[13px] text-muted">
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
    <details className={cx("group my-1.5 rounded-xl border border-border bg-panel-2/50 text-[13px] open:bg-panel-2", enter)}>
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
            <span className="shrink-0 font-mono text-[12.5px] font-semibold text-accent">{toolName}</span>
            <span className="min-w-0 flex-1 truncate font-mono text-[12.5px] text-muted">{toolSummary(input)}</span>
          </>
        )}
        <IconChevron size={14} className="shrink-0 text-faint transition-transform group-open:rotate-90" />
      </summary>
      <div className="space-y-2 border-t border-border px-3 py-2">
        {simple && <p className="font-mono text-[12px] font-semibold text-accent">{toolName}</p>}
        <pre className="scroll-thin max-h-48 overflow-auto whitespace-pre-wrap break-words font-mono text-[12px] text-muted">
          {JSON.stringify(input, null, 2)}
        </pre>
        {done && (
          <pre
            className={cx(
              "scroll-thin max-h-72 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-border bg-panel p-2 font-mono text-[12px]",
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
      <span className="ml-auto pb-2 font-mono text-[11.5px] text-muted">{summary}</span>
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
  const meta = [turn.durationMs != null ? formatDuration(turn.durationMs) : null, turn.costUsd != null ? `$${turn.costUsd.toFixed(2)} total` : null]
    .filter(Boolean)
    .join(" · ");
  if (turn.isError) {
    const result = turn.result?.trim();
    return (
      <div role="alert" className={cx("my-3 ml-10 rounded-lg border border-err/30 bg-err-soft px-3 py-2 text-[13px] text-err", enter)}>
        <div className="flex flex-wrap items-center gap-x-2">
          <IconX size={13} strokeWidth={3} />
          <span className="font-semibold">Turn failed</span>
          {meta && <span className="text-[12px] opacity-75">{meta}</span>}
        </div>
        {result && (
          <div className="mt-1 whitespace-pre-wrap [overflow-wrap:anywhere]">{result.length > 600 ? `${result.slice(0, 600)}…` : result}</div>
        )}
      </div>
    );
  }
  return (
    <div className={cx("-mt-2 mb-3 ml-10 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[12px] text-faint", enter)}>
      <span className="font-medium text-ok">Turn complete</span>
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
    <div className={cx("flex flex-wrap items-center gap-2 py-2 pl-10 text-[13px] font-medium text-accent", enter)}>
      <IconQuestion size={15} /> Waiting for your answer
      {offscreen && (
        <button
          type="button"
          onClick={jump}
          className="cursor-pointer rounded-md border border-accent/40 px-2 py-0.5 text-[12px] font-medium hover:bg-accent-soft"
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
      <div className="py-2 pl-10 text-[13px] text-err">
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
    <div className={cx("flex items-center gap-2 py-2 pl-10 text-[13px] text-muted", enter)}>
      <Spinner className="text-accent" /> {detail || "Working in the microVM…"}
    </div>
  );
}

function Composer({ isRunning, live, waiting }: { isRunning: boolean; live: boolean; waiting: boolean }) {
  return (
    <div className="border-t border-border bg-panel/60 p-3">
      <ComposerPrimitive.Root className="flex items-end gap-2 rounded-2xl border border-border bg-panel p-1.5 pl-3 shadow-[var(--shadow)] focus-within:border-accent">
        <ComposerPrimitive.Input
          rows={1}
          placeholder={
            !live
              ? "This colony's microVM is not running"
              : waiting
                ? "Answer the card above, or send a note to the agent…"
                : isRunning
                  ? "Send a follow-up — the agent reads it next…"
                  : "Message the agent…"
          }
          className="scroll-thin max-h-40 min-h-9 min-w-0 flex-1 resize-none bg-transparent py-2 text-[14px] leading-5 outline-none placeholder:text-faint"
        />
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
