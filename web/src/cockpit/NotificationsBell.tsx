// The inbox as the header's notifications: a bell at the top right whose badge counts the colonies
// waiting on a person, and a panel under it with what the inbox view shows — who needs you, then
// what the colonies have said — each line a way into the colony. The full inbox stays one click away.
import { useEffect, useRef, useState, type ReactElement, type Ref } from "react";

import { store, stored } from "../components/ui";
import { needsYou } from "../notifications";
import type { Session } from "../types";
import { feedEntries } from "./feed";
import { KIND_DOT, READ_AT, relative } from "./InboxView";
import { useOpenQuestions, watchdogFlagged } from "./questions";

/** How many notification lines the panel lists before "Open inbox" takes over. */
const PANEL_LINES = 20;

export interface InboxActions {
  /** Every colony, unfiltered: like the inbox, the bell is cross-workspace. */
  sessions: Session[];
  onOpenColony: (id: string) => void;
  onOpenInbox: () => void;
  onOpenNotificationSettings: () => void;
}

export function NotificationsBell({ sessions, onOpenColony, onOpenInbox, onOpenNotificationSettings }: InboxActions): ReactElement {
  const [open, setOpen] = useState(false);
  const [readAt, setReadAt] = useState<number>(() => Number(stored(READ_AT) ?? 0));
  const root = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);

  const waiting = sessions.filter(needsYou).length;
  const unread = feedEntries(sessions).filter((e) => Date.parse(e.at) > readAt).length;

  const close = (refocus: boolean) => {
    setOpen(false);
    if (refocus) button.current?.focus();
  };

  // Click outside closes; Escape closes and hands focus back to the bell. Opening moves focus in.
  useEffect(() => {
    if (!open) return;
    panel.current?.focus();
    const onDown = (event: MouseEvent) => {
      if (root.current && !root.current.contains(event.target as Node)) setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        close(true);
      }
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  // ⌘I / Ctrl+I opens and closes the panel from anywhere but a text field.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey || event.key.toLowerCase() !== "i") return;
      const target = event.target as HTMLElement | null;
      if (target && (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName))) return;
      event.preventDefault();
      setOpen((o) => !o);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const label = `notifications${waiting > 0 ? ` · ${waiting} need you` : ""}${unread > 0 ? ` · ${unread} unread` : ""}`;

  return (
    <div ref={root} className="relative shrink-0">
      <button
        ref={button}
        type="button"
        title={label}
        aria-label={label}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => (open ? close(false) : setOpen(true))}
        className={`relative grid h-8 w-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted transition-colors hover:bg-panel-2 hover:text-text ${open ? "bg-panel-2 text-text" : ""}`}
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M6 16.5V11a6 6 0 0 1 12 0v5.5l1.5 2h-15z" />
          <path d="M10 20.5a2.2 2.2 0 0 0 4 0" />
        </svg>
        {waiting > 0 ? (
          <span
            aria-hidden="true"
            className="absolute -right-1 -top-1 grid h-4 min-w-4 place-items-center rounded-full border-2 border-bg bg-warn px-0.5 font-mono text-[9px] font-semibold leading-none text-bg tabular-nums"
          >
            {waiting > 99 ? "99+" : waiting}
          </span>
        ) : (
          unread > 0 && <span aria-hidden="true" className="absolute right-1 top-1 h-2 w-2 rounded-full border-2 border-bg bg-accent" />
        )}
      </button>
      {open && (
        <InboxPanel
          ref={panel}
          sessions={sessions}
          readAt={readAt}
          onMarkAllRead={() => {
            const now = Date.now();
            setReadAt(now);
            store(READ_AT, String(now));
          }}
          onOpenColony={(id) => {
            close(false);
            onOpenColony(id);
          }}
          onOpenInbox={() => {
            close(false);
            onOpenInbox();
          }}
          onOpenNotificationSettings={() => {
            close(false);
            onOpenNotificationSettings();
          }}
        />
      )}
    </div>
  );
}

/** The panel itself; mounted only while open, so the waiting colonies' questions are fetched then. */
function InboxPanel({
  ref,
  sessions,
  readAt,
  onMarkAllRead,
  onOpenColony,
  onOpenInbox,
  onOpenNotificationSettings,
}: {
  ref: Ref<HTMLDivElement>;
  sessions: Session[];
  readAt: number;
  onMarkAllRead: () => void;
  onOpenColony: (id: string) => void;
  onOpenInbox: () => void;
  onOpenNotificationSettings: () => void;
}): ReactElement {
  const waiting = sessions.filter(needsYou);
  const questions = useOpenQuestions(sessions);
  const entries = feedEntries(sessions);
  const shown = entries.slice(0, PANEL_LINES);

  return (
    <div
      ref={ref}
      role="dialog"
      aria-label="notifications"
      tabIndex={-1}
      className="absolute right-0 top-full z-50 mt-2 flex max-h-[min(560px,calc(100dvh-80px))] w-[min(420px,calc(100vw-24px))] animate-[ck-in_160ms_ease-out_both] flex-col overflow-hidden rounded-xl border border-border-strong bg-panel text-text shadow-[0_16px_48px_rgb(0_0_0/0.4)] focus-visible:outline-none"
    >
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-4 py-3">
        <h2 className="m-0 text-[14px] font-semibold">Notifications</h2>
        <span className={`text-[12.5px] tabular-nums ${waiting.length > 0 ? "text-warn" : "text-faint"}`}>{waiting.length} need you</span>
        <div className="flex-1" />
        <button type="button" onClick={onMarkAllRead} className="cursor-pointer border-0 bg-transparent p-0 text-[12.5px] text-muted hover:text-text">
          mark all read
        </button>
      </div>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto">
        {waiting.length > 0 && (
          <section aria-label="needs you" className="border-b border-border">
            {waiting.map((session) => (
              <div key={session.id} className="border-t border-border px-4 py-3 first:border-t-0">
                <div className="flex items-center gap-2 font-mono text-[11px] text-muted">
                  <span aria-hidden="true" className="h-[7px] w-[7px] shrink-0 rounded-full bg-warn" />
                  <span className="min-w-0 truncate">
                    {session.repo}
                    {session.issue != null ? `#${session.issue}` : ""}
                  </span>
                  <button
                    type="button"
                    onClick={() => onOpenColony(session.id)}
                    className="ml-auto shrink-0 cursor-pointer rounded-md border-0 bg-text px-2.5 py-1 font-sans text-[12.5px] font-medium text-bg hover:opacity-85"
                  >
                    Answer
                  </button>
                </div>
                <div className="mt-1.5 text-[13.5px] font-semibold [text-wrap:pretty]">
                  {questions[session.id] ?? (watchdogFlagged(session) ? "the watchdog flagged this colony" : "the colony asked you a question")}
                </div>
                <div className="mt-0.5 truncate text-[12.5px] text-muted">{session.issue_title || "no title yet"}</div>
              </div>
            ))}
          </section>
        )}

        {shown.length === 0 ? (
          <div className="px-4 py-6 text-center text-[13px] text-muted">{waiting.length === 0 ? "nothing waits on you, and nothing new" : "nothing else new"}</div>
        ) : (
          <ul aria-label="recent" className="m-0 list-none p-0">
            {shown.map((entry) => {
              const unread = Date.parse(entry.at) > readAt;
              return (
                <li key={entry.id}>
                  <button
                    type="button"
                    onClick={() => onOpenColony(entry.id)}
                    className={`grid w-full cursor-pointer grid-cols-[10px_minmax(0,1fr)_auto] items-center gap-3 border-0 border-t border-solid border-border bg-transparent px-4 py-2.5 text-left text-text first:border-t-0 hover:bg-panel-2 focus-visible:bg-panel-2 focus-visible:outline-none ${
                      unread ? "" : "opacity-60"
                    }`}
                  >
                    <span aria-hidden="true" className="h-[7px] w-[7px] rounded-full" style={{ background: KIND_DOT[entry.kind] }} />
                    <span className="min-w-0">
                      <span className={`block truncate text-[13px] ${unread ? "font-semibold" : "font-normal"}`}>{entry.text}</span>
                      <span className="mt-0.5 block truncate font-mono text-[11px] text-faint">{entry.label}</span>
                    </span>
                    <span className="whitespace-nowrap font-mono text-[11px] text-faint">{relative(entry.at)}</span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <div className="flex shrink-0 items-center gap-3 border-t border-border px-4 py-2.5 text-[12.5px]">
        <button type="button" onClick={onOpenInbox} className="cursor-pointer border-0 bg-transparent p-0 font-medium text-text hover:text-accent">
          Open inbox{entries.length > shown.length ? ` · ${entries.length - shown.length} more` : ""} ›
        </button>
        <div className="flex-1" />
        <button type="button" onClick={onOpenNotificationSettings} className="cursor-pointer border-0 bg-transparent p-0 text-muted hover:text-text">
          settings
        </button>
      </div>
    </div>
  );
}
