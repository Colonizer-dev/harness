import {
  AssistantRuntimeProvider,
  ComposerPrimitive,
  MessagePrimitive,
  ThreadPrimitive,
  makeAssistantToolUI,
  useExternalStoreRuntime,
  type AppendMessage,
  type ReasoningMessagePartProps,
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
  type ToolResultPayload,
  type TurnSummary,
} from "../sessionStream";
import type { AgentRef, MemoryScope } from "../types";
import { describeTool, isNoiseTool, type ActivityIcon } from "./activity";
import { AskUserCard, QuestionActionsContext, type QuestionActions } from "./AskUserCard";
import {
  IconAlert,
  IconAnt,
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
    }),
    [stream, state.submitting, connected, live, toast],
  );

  return (
    <QuestionActionsContext.Provider value={questionActions}>
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
          <ThreadPrimitive.Viewport className="scroll-thin min-h-0 flex-1 overflow-y-auto px-4 pb-4 pt-2">
            {thread.messages.length === 0 && <EmptyChat connection={state.connection} live={live} />}
            <ThreadPrimitive.Messages>
              {({ message }) => (
                <>
                  {message.role !== "user" ? (
                    thread.agents[message.id] ? (
                      <SubagentMessage agent={thread.agents[message.id]} />
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
          </ThreadPrimitive.Viewport>
          <Composer isRunning={isRunning} live={live} waiting={thread.hasOpenQuestion} />
        </ThreadPrimitive.Root>
      </AssistantRuntimeProvider>
      </SimpleViewContext.Provider>
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
  const trimmed = text.trim();
  const firstLine = trimmed.split("\n").find((line) => line.trim()) ?? "";
  return (
    <MessagePrimitive.Root className="my-3">
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
  return (
    <MessagePrimitive.Root className="my-3">
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
  const { proposal } = notice;
  return (
    <div className="my-2 ml-10 flex flex-wrap items-center gap-x-2 gap-y-1 rounded-lg border border-border bg-panel-2/60 px-3 py-1.5 text-[12.5px] text-muted">
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
  return (
    <MessagePrimitive.Root className="my-4 flex justify-end">
      <div className="max-w-[85%] whitespace-pre-wrap break-words rounded-2xl rounded-br-md bg-accent-soft px-3.5 py-2 text-[14px] leading-relaxed">
        <MessagePrimitive.Parts />
      </div>
    </MessagePrimitive.Root>
  );
}

function MarkdownText() {
  return <MarkdownTextPrimitive className="md break-words text-[14px] leading-relaxed" />;
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
  const payload = result as ToolResultPayload | undefined;
  const done = payload !== undefined;
  const failed = Boolean(isError || payload?.is_error);
  const input = (args ?? {}) as Record<string, unknown>;
  // A step that says nothing to a reader, and didn't fail, is just noise in the simple view.
  if (simple && !failed && isNoiseTool(toolName, input)) return null;
  const activity = describeTool(toolName, input);
  const ActivityGlyph = ACTIVITY_ICONS[activity.icon];
  return (
    <details className="group my-1.5 rounded-xl border border-border bg-panel-2/50 text-[13px] open:bg-panel-2">
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

/**
 * A subagent's turn. The orchestrator and each subagent are different speakers in the same thread,
 * so a subagent gets its own avatar, its name, and an indented column — the shape of a group chat
 * rather than one long monologue.
 */
function SubagentMessage({ agent }: { agent: AgentRef }) {
  return (
    <MessagePrimitive.Root className="my-4 ml-4 flex gap-3 border-l-2 border-accent/25 pl-4">
      <div
        className="mt-0.5 grid size-7 shrink-0 place-items-center rounded-lg bg-accent-soft text-accent"
        title={agent.description ?? undefined}
      >
        <IconAnt size={16} />
      </div>
      <div className="min-w-0 flex-1 space-y-1.5">
        <div className="flex flex-wrap items-baseline gap-x-2">
          <span className="text-[12.5px] font-semibold text-accent">{agent.name}</span>
          <span className="text-[11.5px] text-faint">subagent</span>
        </div>
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

function AssistantMessage() {
  return (
    <MessagePrimitive.Root className="my-4 flex gap-3">
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
  const meta = [turn.durationMs != null ? formatDuration(turn.durationMs) : null, turn.costUsd != null ? `$${turn.costUsd.toFixed(2)} total` : null]
    .filter(Boolean)
    .join(" · ");
  if (turn.isError) {
    const result = turn.result?.trim();
    return (
      <div role="alert" className="my-3 ml-10 rounded-lg border border-err/30 bg-err-soft px-3 py-2 text-[13px] text-err">
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
    <div className="-mt-2 mb-3 ml-10 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[12px] text-faint">
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
    <div className="flex flex-wrap items-center gap-2 py-2 pl-10 text-[13px] font-medium text-accent">
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
  if (state.agentState === "working") {
    return (
      <div className="flex items-center gap-2 py-2 pl-10 text-[13px] text-muted">
        <Spinner className="text-accent" /> {state.agentDetail || "Working in the microVM…"}
      </div>
    );
  }
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
