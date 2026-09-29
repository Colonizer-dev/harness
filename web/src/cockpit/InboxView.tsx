// The inbox: what is waiting on a person, then everything else the colonies have to say.
//
// The cards stop at "this colony is waiting, here is the way in". The question itself and its
// options live in the colony's chat, which is the only place that has them — the mothership streams
// a question to one open colony, not to the list. Sending someone to the question beats showing a
// hollow copy of it here.
import { useState, type ReactElement } from "react";
import { Page } from "./Page";

import { store, stored } from "../components/ui";
import { needsYou } from "../notifications";
import type { QuotaActionRequest, QuotaCard, Session } from "../types";
import { ProviderQuotaCard, quotaCardColonyIds } from "./ProviderQuotaCard";
import { useOpenQuestions, watchdogFlagged } from "./questions";
import { feedEntries, type FeedKind } from "./feed";
import { taskLine, taskTooltip } from "../summary";
import { LoopBadge } from "./LoopsView";

/** The local "read up to" mark, shared by the inbox and the header's notifications panel. */
export const READ_AT = "colonizer.inboxReadAt";

/** The dot beside a line. Kept in step with the timeline's, so one colony reads the same in both. */
export const KIND_DOT: Record<FeedKind, string> = {
  question: "var(--warn)",
  returned: "var(--ok)",
  failed: "var(--err)",
  launched: "var(--accent)",
  queued: "var(--faint)",
  stopped: "var(--faint)",
};

export function InboxView({
  sessions,
  onOpenColony,
  onOpenNotificationSettings,
  quotaCards = [],
  onQuotaAction,
}: {
  /** Every colony in the workspace, filtered by the caller. */
  sessions: Session[];
  onOpenColony: (id: string) => void;
  onOpenNotificationSettings: () => void;
  /** "Provider out of quota" cards (issue #767), shown first: one per provider, not one per colony. */
  quotaCards?: QuotaCard[];
  onQuotaAction?: (provider: string, body: QuotaActionRequest) => Promise<unknown>;
}): ReactElement {
  // Nothing server-side records a read; this is a local high-water mark, so "read" is per browser.
  const [readAt, setReadAt] = useState<number>(() => Number(stored(READ_AT) ?? 0));

  // A colony a quota card covers is answered on the card, not listed again as a question.
  const onCards = quotaCardColonyIds(quotaCards);
  const needing = sessions.filter(needsYou);
  const waiting = needing.filter((session) => !onCards.has(session.id));
  const needCount = new Set([...needing.map((session) => session.id), ...onCards]).size;
  const questions = useOpenQuestions(sessions);
  const entries = feedEntries(sessions);

  const markAllRead = () => {
    const now = Date.now();
    setReadAt(now);
    store(READ_AT, String(now));
  };

  return (
    <Page frameClassName="flex flex-col gap-4">
      <div className="mb-6 flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="m-0 text-[30px] font-semibold leading-[1.15] tracking-[-0.035em]">Inbox</h1>
          <div className="mt-2 text-[14px] text-muted">
            <span className={needCount > 0 ? "text-warn" : undefined}>{needCount} need you</span> · every workspace
          </div>
        </div>
        <button
          type="button"
          onClick={onOpenNotificationSettings}
          className="cursor-pointer border-0 bg-transparent p-0 text-[13px] text-muted hover:text-text max-sm:py-2.5"
        >
          notification settings ›
        </button>
      </div>
      {/* The two lists side by side once the frame has room for both (a wide desktop); stacked below. */}
      <div className="grid items-start gap-4 @min-[1800px]:grid-cols-2 @min-[1800px]:gap-x-12">
        <section aria-label="Needs you" className="flex min-w-0 flex-col gap-4">
          <h2 className="m-0 text-[14px] font-medium">Needs you</h2>

          {quotaCards.map((card) => (
            <ProviderQuotaCard
              key={card.provider}
              card={card}
              onOpenColony={onOpenColony}
              onAction={onQuotaAction ?? (async () => undefined)}
            />
          ))}

          {waiting.length === 0 && quotaCards.length > 0 ? null : waiting.length === 0 ? (
            <div className="flex items-center gap-3 border-y border-border py-3.5 text-[13px] text-muted">
              <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true" className="text-ok">
                <path d="M12 2.8 20 7.4v9.2L12 21.2 4 16.6V7.4z" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinejoin="round" />
                <path d="m8.5 12 2.5 2.5 4.5-5" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
              nothing waits on you
            </div>
          ) : (
            waiting.map((session) => (
              <div
                key={session.id}
                className="-mt-4 border-y border-border py-4 first-of-type:mt-0"
                style={{ animation: "ck-in 0.24s ease-out both" }}
              >
                <div className="flex items-center gap-2.5 font-mono text-[11.5px] text-muted">
                  <span aria-hidden="true" className="h-[7px] w-[7px] rounded-full bg-warn" />
                  <span>
                    {session.repo}
                    {session.issue != null ? `#${session.issue}` : ""}
                  </span>
                  <button
                    type="button"
                    onClick={() => onOpenColony(session.id)}
                    className="ml-auto cursor-pointer rounded-md border-0 bg-text px-3 py-1.5 font-sans text-[13px] font-medium text-bg hover:opacity-85 max-sm:min-h-11 max-sm:px-4"
                  >
                    Answer
                  </button>
                </div>
                <div className="mt-2 text-[15px] font-semibold [text-wrap:pretty]">
                  {questions[session.id] ?? (watchdogFlagged(session) ? "the watchdog flagged this colony" : "the colony asked you a question")}
                </div>
                <div className="mt-1 text-[13px] text-muted" title={taskTooltip(session)}>{taskLine(session, "no title yet")}<LoopBadge session={session} /></div>
              </div>
            ))
          )}

        </section>

        <section aria-label="Notifications" className="flex min-w-0 flex-col gap-4">
          <div className="mt-2.5 flex items-center gap-2.5 @min-[1800px]:mt-0">
            <h2 className="m-0 text-[14px] font-medium">Notifications</h2>
            <div className="flex-1" />
            <button type="button" onClick={markAllRead} className="cursor-pointer border-0 bg-transparent p-0 text-[13px] text-muted hover:text-text max-sm:py-2.5">
              mark all read
            </button>
          </div>

          {entries.length === 0 ? (
            <div className="border-y border-border py-3.5 text-[13px] text-muted">
              no colonies in this workspace yet
            </div>
          ) : (
            <div className="overflow-hidden border-y border-border">
              {entries.map((entry) => {
                const unread = Date.parse(entry.at) > readAt;
                return (
                  <button
                    key={entry.id}
                    type="button"
                    onClick={() => onOpenColony(entry.id)}
                    className={`-mt-px grid w-full cursor-pointer grid-cols-[10px_minmax(0,1fr)_auto] items-center gap-3 border-0 border-t border-solid border-border bg-transparent py-3 text-left text-text hover:bg-panel-2 ${
                      unread ? "opacity-100" : "opacity-60"
                    }`}
                  >
                    <span aria-hidden="true" className="h-[7px] w-[7px] rounded-full" style={{ background: KIND_DOT[entry.kind] }} />
                    <span className="min-w-0">
                      <span className={`block truncate text-[13.5px] ${unread ? "font-semibold" : "font-normal"}`}>{entry.text}</span>
                      <span className="mt-0.5 block truncate font-mono text-[11px] text-faint">{entry.label}</span>
                    </span>
                    <span className="whitespace-nowrap font-mono text-[11px] text-faint">{relative(entry.at)}</span>
                  </button>
                );
              })}
            </div>
          )}
        </section>
      </div>
    </Page>
  );
}

/** Short enough for the right-hand column: "3m", "2h", "4d". */
export function relative(at: string): string {
  const seconds = Math.max(0, (Date.now() - Date.parse(at)) / 1000);
  if (Number.isNaN(seconds)) return "";
  if (seconds < 60) return "now";
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  if (seconds < 86_400) return `${Math.round(seconds / 3600)}h`;
  return `${Math.round(seconds / 86_400)}d`;
}
