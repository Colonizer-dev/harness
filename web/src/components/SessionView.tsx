import { lazy, Suspense, useEffect, useRef, useState, type ReactNode } from "react";
import type { Api } from "../api";
import { errorMessage, useApi, useToast } from "../context";
import { useSessionStream, type LogEntry } from "../sessionStream";
import type { Session } from "../types";
import { ChatPanel } from "./ChatPanel";
import {
  IconAlert,
  IconBranch,
  IconChat,
  IconChevronDown,
  IconExternal,
  IconGitPR,
  IconMenu,
  IconNetwork,
  IconPower,
  IconTerminal,
  IconTrash,
} from "./icons";
import { AttentionBadge, Badge, Button, Spinner, StatusBadge, attentionText, buttonClass, cx, isLive, minutesAgo, orgOf } from "./ui";

// xterm is the largest dependency; load it only when a session view opens.
const TerminalPanel = lazy(() => import("./TerminalPanel").then((m) => ({ default: m.TerminalPanel })));

export interface InterfaceFlags {
  chat: boolean;
  terminal: boolean;
}

type Action = "publish" | "resume" | "stop" | "cleanup" | "delete";

export function SessionView({
  sessionId,
  fallback,
  interfaces,
  narrow,
  showOrg,
  onSessionChanged,
  onSessionDeleted,
  onOpenSidebar,
  onOpenMemory,
  onMemoryProposed,
}: {
  sessionId: string;
  fallback: Session | null;
  interfaces: InterfaceFlags;
  narrow: boolean;
  /** Show the org chip (the sidebar isn't filtered to one org). */
  showOrg: boolean;
  onSessionChanged: (session: Session) => void;
  onSessionDeleted: (id: string) => void;
  onOpenSidebar: () => void;
  onOpenMemory: () => void;
  onMemoryProposed: () => void;
}) {
  const api = useApi();
  const toast = useToast();
  const { stream, state } = useSessionStream(api, sessionId);
  const [tab, setTab] = useState<"chat" | "terminal">("chat");
  const [busy, setBusy] = useState<Action | null>(null);

  // The stream sees session changes immediately; keep the sidebar list in step instead of waiting for its poll.
  useEffect(() => {
    if (state.session) onSessionChanged(state.session);
  }, [state.session, onSessionChanged]);

  // A colony proposed a memory note: refresh the sidebar's review count right away.
  const noticeCount = state.memoryNotices.length;
  const seenNotices = useRef(0);
  useEffect(() => {
    if (noticeCount > seenNotices.current) onMemoryProposed();
    seenNotices.current = noticeCount;
  }, [noticeCount, onMemoryProposed]);

  const session = state.session ?? fallback;
  if (!session) {
    return (
      <div className="grid h-full place-items-center text-muted">
        <Spinner />
      </div>
    );
  }

  const live = isLive(session.status);
  const showTerminal = interfaces.terminal;
  const showChat = interfaces.chat || !showTerminal;
  const split = showChat && showTerminal;
  const waiting = state.agentState === "waiting_for_answer" || session.status === "waiting_for_answer";
  const attention = session.attention ?? null;

  /** What deleting this colony takes with it, in the words the confirmation uses. */
  const deleteWarning =
    session.status === "queued"
      ? "Remove this colony from the queue and the list? It never started, so nothing else is lost."
      : ["pr_opened", "merged", "closed"].includes(session.status)
        ? "Delete this colony? Its chat, logs and local worktree are removed. The pull request and its pushed branch stay on GitHub."
        : session.cleaned_up
          ? "Delete this colony's chat and logs? Its worktree was already cleaned up. This cannot be undone."
          : "Delete this colony? Its worktree — including any changes that were never published — its chat and its logs are removed. This cannot be undone.";

  const remove = async () => {
    if (!window.confirm(deleteWarning)) return;
    setBusy("delete");
    try {
      const result = (await api.deleteSession(session.id)) as { leftover?: string | null } | null;
      if (result?.leftover) toast(`Colony deleted, but some files could not be removed: ${result.leftover}`, "error");
      onSessionDeleted(session.id);
    } catch (error) {
      toast(errorMessage(error), "error");
      setBusy(null);
    }
  };

  const act = async (action: Action, call: (api: Api, id: string) => Promise<Session>, confirmText?: string) => {
    if (confirmText && !window.confirm(confirmText)) return;
    setBusy(action);
    try {
      onSessionChanged(await call(api, session.id));
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="shrink-0 border-b border-border bg-panel px-4 py-3">
        <div className="flex flex-wrap items-start gap-x-4 gap-y-3">
          {narrow && (
            <button
              type="button"
              onClick={onOpenSidebar}
              aria-label="Open sidebar"
              className="-ml-1.5 grid size-9 shrink-0 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
            >
              <IconMenu size={18} />
            </button>
          )}
          <div className="min-w-0 flex-1 basis-64">
            <div className="flex flex-wrap items-center gap-2">
              {showOrg && (
                <span className="rounded-md bg-panel-3 px-1.5 py-px text-[11.5px] font-medium text-muted">{orgOf(session)}</span>
              )}
              <span className="font-mono text-[12.5px] text-muted">
                {session.repo}
                {session.issue != null && `#${session.issue}`}
              </span>
              <StatusBadge status={session.status} />
              <AttentionBadge attention={attention} />
              {session.issue == null && <Badge>No issue</Badge>}
              {session.autopilot && <Badge>Autopilot</Badge>}
            </div>
            <h1 className="mt-1 text-[17px] font-semibold leading-snug [overflow-wrap:anywhere]">
              {session.issue_title || (session.issue != null ? `Issue #${session.issue}` : "Open colony")}
            </h1>
            <div className="mt-1.5 flex flex-wrap items-center gap-x-3.5 gap-y-1 text-[12.5px] text-muted">
              <span className="inline-flex min-w-0 items-center gap-1">
                <IconBranch size={13} className="shrink-0" />
                <code className="truncate font-mono">{session.branch}</code>
              </span>
              {session.mesh && (
                <span className="inline-flex items-center gap-1" title="Private mesh node">
                  <IconNetwork size={13} />
                  {session.mesh.name}
                  {session.mesh.ip && <span className="font-mono text-faint">{session.mesh.ip}</span>}
                </span>
              )}
              <span>{session.agent}</span>
              <CostSummary session={session} />
              {live && session.last_activity_at && !attention && <span>Last activity {minutesAgo(session.last_activity_at)}</span>}
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            {session.pr_url && (
              <a href={session.pr_url} target="_blank" rel="noopener noreferrer" className={buttonClass("secondary")}>
                <IconGitPR size={15} /> View PR <IconExternal size={12} />
              </a>
            )}
            {!session.pr_url && (
              <Button
                variant="primary"
                disabled={!live || busy !== null}
                onClick={() => act("publish", (a, id) => a.publishSession(id))}
                title="Stop the agent, commit, push and open the pull request"
              >
                {busy === "publish" ? <Spinner /> : <IconGitPR size={15} />} Create PR
              </Button>
            )}
            {!live && !session.cleaned_up && (session.status === "stopped" || session.status === "failed") && (
              <Button
                variant="primary"
                disabled={busy !== null}
                onClick={() => act("resume", (a, id) => a.resumeSession(id))}
                title="Boot a fresh microVM on this colony's worktree and continue where it stopped"
              >
                {busy === "resume" ? <Spinner /> : <IconPower size={15} />} Resume
              </Button>
            )}
            <Button
              disabled={(!live && session.status !== "queued") || busy !== null}
              onClick={() =>
                act(
                  "stop",
                  (a, id) => a.stopSession(id),
                  session.status === "queued" ? undefined : "Stop and remove this colony's microVM? The worktree is kept.",
                )
              }
            >
              {busy === "stop" ? <Spinner /> : <IconPower size={15} />} {session.status === "queued" ? "Leave the queue" : "Stop"}
            </Button>
            <Button
              variant="danger"
              disabled={live || session.status === "publishing" || session.cleaned_up || busy !== null}
              onClick={() =>
                act("cleanup", (a, id) => a.cleanupSession(id), "Delete this colony's worktree and local branch? Pushed branches are not affected.")
              }
              title={session.cleaned_up ? "Already cleaned up" : live ? "Stop the colony first" : "Remove the worktree and local branch"}
            >
              {busy === "cleanup" ? <Spinner /> : <IconTrash size={15} />} Clean up
            </Button>
            <Button
              variant="danger"
              disabled={live || session.status === "publishing" || busy !== null}
              onClick={remove}
              title={live || session.status === "publishing" ? "Stop the colony first" : "Remove this colony from the list, with its chat, logs and worktree"}
            >
              {busy === "delete" ? <Spinner /> : <IconTrash size={15} />} Delete
            </Button>
          </div>
        </div>
        {attention && (
          <div role="status" className="mt-3 flex flex-wrap items-center gap-x-2 gap-y-1 rounded-lg bg-warn-soft px-3 py-2 text-[13px] text-warn">
            <IconAlert size={14} className="shrink-0" />
            <span className="font-semibold">{attentionText(attention)}</span>
            <span className="opacity-80">· last activity {minutesAgo(session.last_activity_at ?? attention.since)}</span>
            {attention.reason === "waiting_for_answer" && <span className="opacity-80">Answer the card in the chat.</span>}
            {attention.reason === "nudges_exhausted" && (
              <span className="opacity-80">Check the terminal, message the agent, or stop the colony.</span>
            )}
            {attention.reason === "autopilot_held" && (
              <span className="opacity-80">The agent's turn ended with an error. Check the chat, then press Create PR or message the agent.</span>
            )}
          </div>
        )}
        {session.error && (
          <div role="alert" className="mt-3 rounded-lg bg-err-soft px-3 py-2 text-[13px] text-err [overflow-wrap:anywhere]">
            {session.error}
          </div>
        )}
      </header>

      <ActivityStrip logs={state.logs} />

      {narrow && split && (
        <div role="tablist" aria-label="Colony panels" className="flex shrink-0 gap-1 border-b border-border bg-panel px-3">
          <TabButton active={tab === "chat"} onClick={() => setTab("chat")}>
            <IconChat size={14} /> Chat
            {waiting && tab !== "chat" && <span className="size-2 rounded-full bg-accent" aria-label="needs your answer" />}
          </TabButton>
          <TabButton active={tab === "terminal"} onClick={() => setTab("terminal")}>
            <IconTerminal size={14} /> Terminal
          </TabButton>
        </div>
      )}

      <div
        className={cx(
          "min-h-0 flex-1",
          split && !narrow ? "grid grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)] grid-rows-[minmax(0,1fr)]" : "flex flex-col",
        )}
      >
        {showChat && (
          <section
            aria-label="Chat"
            className={cx(
              "flex min-h-0 flex-col",
              split && !narrow && "border-r border-border",
              (narrow || !split) && "flex-1",
              narrow && split && tab !== "chat" && "hidden",
            )}
          >
            {split && !narrow && (
              <PanelHeader icon={<IconChat size={14} />} title="Chat">
                <ConnectionLabel connection={state.connection} />
              </PanelHeader>
            )}
            <div className="min-h-0 flex-1">
              <ChatPanel stream={stream} state={state} live={live} onOpenMemory={onOpenMemory} />
            </div>
          </section>
        )}
        {showTerminal && (
          <section
            aria-label="Terminal"
            className={cx("flex min-h-0 flex-col", (narrow || !split) && "flex-1", narrow && split && tab !== "terminal" && "hidden")}
          >
            {split && !narrow && (
              <PanelHeader icon={<IconTerminal size={14} />} title="Terminal">
                {session.sandbox}
              </PanelHeader>
            )}
            <div className="min-h-0 flex-1 bg-[var(--term-bg)]">
              <Suspense fallback={null}>
                <TerminalPanel
                  sessionId={session.id}
                  enabled={live && session.status !== "starting"}
                  starting={session.status === "starting"}
                />
              </Suspense>
            </div>
          </section>
        )}
      </div>
    </div>
  );
}

function TabButton({ active, onClick, children }: { active: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active}
      onClick={onClick}
      className={cx(
        "-mb-px flex cursor-pointer items-center gap-1.5 border-b-2 px-3 py-2.5 text-[13px] font-medium",
        active ? "border-accent text-text" : "border-transparent text-muted hover:text-text",
      )}
    >
      {children}
    </button>
  );
}

function PanelHeader({ icon, title, children }: { icon: ReactNode; title: string; children?: ReactNode }) {
  return (
    <div className="flex h-9 shrink-0 items-center gap-2 border-b border-border bg-panel px-4 text-[11.5px] font-semibold uppercase tracking-wide text-muted">
      {icon}
      {title}
      <span className="ml-auto min-w-0 truncate font-normal normal-case tracking-normal text-faint">{children}</span>
    </div>
  );
}

function ConnectionLabel({ connection }: { connection: string }) {
  if (connection === "open") {
    return (
      <span className="inline-flex items-center gap-1.5">
        <span className="size-1.5 rounded-full bg-ok" /> Live
      </span>
    );
  }
  if (connection === "closed") return <span>Disconnected</span>;
  return (
    <span className="inline-flex items-center gap-1.5">
      <Spinner className="size-3" /> {connection === "reconnecting" ? "Reconnecting" : "Connecting"}
    </span>
  );
}

function ActivityStrip({ logs }: { logs: LogEntry[] }) {
  const [open, setOpen] = useState(false);
  if (logs.length === 0) return null;
  const last = logs[logs.length - 1];
  const dot = (level: string) => (level === "error" ? "bg-err" : level === "warn" ? "bg-warn" : "bg-ok");
  return (
    <div className="shrink-0 border-b border-border bg-panel-2/50 text-[12.5px]">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex w-full cursor-pointer items-center gap-2 px-4 py-1.5 text-left text-muted hover:text-text"
      >
        <span className={cx("size-1.5 shrink-0 rounded-full", dot(last.level))} />
        <span className="shrink-0 font-medium">Activity</span>
        <span className="min-w-0 flex-1 truncate">{last.message}</span>
        <IconChevronDown size={14} className={cx("shrink-0 transition-transform", open && "rotate-180")} />
      </button>
      {open && (
        <ol className="scroll-thin max-h-48 overflow-y-auto px-4 pb-2 font-mono text-[12px]">
          {logs
            .slice()
            .reverse()
            .map((log, i) => (
              <li
                key={`${log.ts}-${i}`}
                className={cx("flex gap-3 py-0.5", log.level === "error" ? "text-err" : log.level === "warn" ? "text-warn" : "text-muted")}
              >
                <span className="shrink-0 text-faint">{log.ts ? new Date(log.ts).toLocaleTimeString() : ""}</span>
                <span className="w-12 shrink-0 text-faint">{log.source}</span>
                <span className="min-w-0 [overflow-wrap:anywhere]">{log.message}</span>
              </li>
            ))}
        </ol>
      )}
    </div>
  );
}

function compactTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}k`;
  return String(n);
}

/**
 * The colony's cost and what it is made of. With per-model usage the dollar figure is Claude's alone, since Claude Code
 * cannot price routed models; the tooltip lists tokens for every model. Older colonies carry only the SDK's total.
 */
function CostSummary({ session }: { session: Session }) {
  const usage = session.model_usage ? Object.entries(session.model_usage) : [];
  if (session.cost_usd == null && usage.length === 0) return null;
  const tokens = usage.reduce(
    (sum, [, u]) => sum + u.input_tokens + u.output_tokens + u.cache_read_tokens + u.cache_write_tokens,
    0,
  );
  const detail = usage
    .map(
      ([model, u]) =>
        `${model}: ${compactTokens(u.input_tokens)} in · ${compactTokens(u.output_tokens)} out · ` +
        `${compactTokens(u.cache_read_tokens)} cache read · ${compactTokens(u.cache_write_tokens)} cache write`,
    )
    .join("\n");
  return (
    <span title={detail || undefined}>
      {session.cost_usd != null && `$${session.cost_usd.toFixed(2)}${usage.length ? " on Claude" : ""}`}
      {usage.length > 0 && `${session.cost_usd != null ? " · " : ""}${compactTokens(tokens)} tokens`}
    </span>
  );
}
