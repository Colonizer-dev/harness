// The inbox as the header's notifications: a bell at the top right whose badge counts the colonies
// waiting on a person, and a Spotlight panel under it (issue #1228): a search box, then who needs you,
// what the colonies have said, and what is done, each line a way into the colony. The full inbox stays
// one click away in the footer.
import { useEffect, useMemo, useRef, useState, type ReactElement, type RefObject } from "react";

import { store, stored } from "../components/ui";
import { needsYouFeed } from "../notifications";
import { OldQuestionsRow } from "./OldQuestionsRow";
import type { Session } from "../types";
import { feedEntries } from "./feed";
import { KIND_DOT, READ_AT, relative } from "./InboxView";
import { expectsAnswer, needsYouLine, useOpenQuestions } from "./questions";
import { taskLine, taskTooltip } from "../summary";
import { DotTile, SpotlightPanel, type PanelRow, type PanelSection } from "./spotlight/Panel";

/** How many notification lines the panel lists before "Open inbox" takes over. */
const PANEL_LINES = 20;

export interface InboxActions {
  /** Every colony, unfiltered: like the inbox, the bell is cross-workspace. */
  sessions: Session[];
  onOpenColony: (id: string) => void;
  onOpenInbox: () => void;
  onOpenNotificationSettings: () => void;
  /** Decision and pull-request cards (issue #1036) not already counted as a colony: one count. */
  decisionCount?: number;
}

export function NotificationsBell({ sessions, onOpenColony, onOpenInbox, onOpenNotificationSettings, decisionCount = 0 }: InboxActions): ReactElement {
  const [open, setOpen] = useState(false);
  const [readAt, setReadAt] = useState<number>(() => Number(stored(READ_AT) ?? 0));
  const button = useRef<HTMLButtonElement>(null);

  const feed = needsYouFeed(sessions);
  const waiting = feed.rows.length + (feed.oldQuestions.length > 0 ? 1 : 0) + decisionCount;
  const unread = feedEntries(sessions).filter((e) => Date.parse(e.at) > readAt).length;

  const close = (_refocus: boolean) => setOpen(false);

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
    <div className="relative shrink-0">
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
            className="absolute -right-1 -top-1 grid h-4 min-w-4 place-items-center rounded-full border-2 border-bg bg-warn px-0.5 font-mono text-micro-sm font-semibold leading-none text-bg tabular-nums"
          >
            {waiting > 99 ? "99+" : waiting}
          </span>
        ) : (
          unread > 0 && <span aria-hidden="true" className="absolute right-1 top-1 h-2 w-2 rounded-full border-2 border-bg bg-accent" />
        )}
      </button>
      {open && (
        <InboxPanel
          anchor={button}
          onClose={() => close(true)}
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

/** The kinds that are news about a colony still going, and the ones that have ended. */
const DONE_KINDS = new Set(["returned", "failed", "stopped"]);

/** The panel itself; mounted only while open, so the waiting colonies' questions are fetched then. */
export function InboxPanel({
  anchor,
  onClose,
  sessions,
  readAt,
  onMarkAllRead,
  onOpenColony,
  onOpenInbox,
  onOpenNotificationSettings,
}: {
  anchor: RefObject<HTMLElement | null>;
  onClose: () => void;
  sessions: Session[];
  readAt: number;
  onMarkAllRead: () => void;
  onOpenColony: (id: string) => void;
  onOpenInbox: () => void;
  onOpenNotificationSettings: () => void;
}): ReactElement {
  const [query, setQuery] = useState("");
  const feed = needsYouFeed(sessions);
  const waiting = feed.rows;
  const oldQuestions = feed.oldQuestions;
  const questions = useOpenQuestions(sessions);
  const entries = feedEntries(sessions);
  const shown = entries.slice(0, PANEL_LINES);
  const q = query.trim().toLowerCase();
  const fits = (...parts: (string | null | undefined)[]) => q === "" || parts.some((p) => p?.toLowerCase().includes(q));

  const sections = useMemo<PanelSection[]>(() => {
    const need: PanelRow[] = waiting
      .filter((s) => fits(s.repo, taskLine(s, ""), questions[s.id], needsYouLine(s)))
      .map((session) => ({
        id: `need:${session.id}`,
        title: questions[session.id] ?? needsYouLine(session),
        subtitle: (
          <span title={taskTooltip(session)}>
            {session.repo}
            {session.issue != null ? `#${session.issue}` : ""} · {taskLine(session, "no title yet")}
          </span>
        ),
        leading: <DotTile color="var(--warn)" />,
        trailing: <span className="rounded-full bg-warn-soft px-2 py-0.5 text-meta-lg font-medium text-warn">{questions[session.id] || expectsAnswer(session) ? "Answer" : "Open"}</span>,
        verb: questions[session.id] || expectsAnswer(session) ? "answer" : "open",
        onPick: () => onOpenColony(session.id),
      }));
    const row = (entry: (typeof shown)[number]): PanelRow => {
      const unread = Date.parse(entry.at) > readAt;
      return {
        id: `feed:${entry.id}:${entry.at}`,
        title: <span className={unread ? "font-semibold" : "font-normal opacity-75"}>{entry.text}</span>,
        subtitle: <span className="font-mono text-meta">{entry.label}</span>,
        leading: <DotTile color={KIND_DOT[entry.kind]} />,
        trailing: <span className="whitespace-nowrap font-mono text-meta tabular-nums">{relative(entry.at)}</span>,
        verb: "open colony",
        onPick: () => onOpenColony(entry.id),
      };
    };
    const matched = shown.filter((e) => fits(e.text, e.label));
    return [
      { id: "need", title: "Needs you", aside: waiting.length > 0 ? `${waiting.length} waiting` : undefined, rows: need },
      { id: "updates", title: "Updates", rows: matched.filter((e) => !DONE_KINDS.has(e.kind)).map(row) },
      { id: "done", title: "Done", rows: matched.filter((e) => DONE_KINDS.has(e.kind)).map(row) },
    ];
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [waiting, shown, questions, readAt, q, onOpenColony]);

  const nothing = waiting.length === 0 && oldQuestions.length === 0 && shown.length === 0;
  return (
    <SpotlightPanel
      label="notifications"
      placement="anchored"
      anchor={anchor}
      align="end"
      width={460}
      onClose={onClose}
      query={query}
      onQuery={setQuery}
      placeholder="Search notifications…"
      sections={sections}
      empty={q ? `Nothing matches “${query.trim()}”.` : nothing ? "Nothing waits on you, and nothing new." : "Nothing else new."}
      below={oldQuestions.length > 0 && q === "" ? <OldQuestionsRow sessions={oldQuestions} compact /> : undefined}
      footerEnd={
        <>
          <button type="button" onClick={onMarkAllRead} className="cursor-pointer border-0 bg-transparent p-0 text-small text-muted hover:text-text">
            mark all read
          </button>
          <button type="button" onClick={onOpenNotificationSettings} className="cursor-pointer border-0 bg-transparent p-0 text-small text-muted hover:text-text">
            settings
          </button>
          <button type="button" onClick={onOpenInbox} className="cursor-pointer border-0 bg-transparent p-0 text-small font-medium text-text hover:text-accent">
            Open inbox{entries.length > shown.length ? ` · ${entries.length - shown.length} more` : ""} ›
          </button>
        </>
      }
    />
  );
}
