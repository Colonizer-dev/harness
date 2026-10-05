// The inbox: what is waiting on a person, then what the colonies have said — the events from the
// activity log that need or needed you, each at its own time, answered or resolved ones marked.
//
// The cards stop at "this colony is waiting, here is the way in". The question itself and its
// options live in the colony's chat, which is the only place that has them — the mothership streams
// a question to one open colony, not to the list. Sending someone to the question beats showing a
// hollow copy of it here.
import { useContext, useEffect, useState, type ReactElement, type ReactNode } from "react";
import { Page } from "./Page";

import { ApiContext } from "../context";
import { store, stored } from "../components/ui";
import { needsYou } from "../notifications";
import type { ActivityEntry, DecisionAnswerRequest, DecisionsView, PrAction, PrCard, QuotaActionReply, QuotaActionRequest, QuotaCard, Session } from "../types";
import { ProviderQuotaCard, QuotaChangeSummary, isQuotaReply, quotaCardColonyIds } from "./ProviderQuotaCard";
import { useOpenQuestions, watchdogFlagged } from "./questions";
import { inboxEntries, type FeedKind } from "./feed";
import { taskLine, taskTooltip } from "../summary";
import { LoopBadge } from "./LoopsView";
import { DecisionsSection } from "./DecisionCards";
import { decisionsCount, prCardColonyIds } from "./decisions";

/** The local "read up to" mark, shared by the inbox and the header's notifications panel. */
export const READ_AT = "colonizer.inboxReadAt";

/** How many activity-log lines the notifications list reads, and how often it re-reads them. */
const FETCH_LIMIT = 500;
const REFRESH_MS = 15_000;

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
  decisions = null,
  onAnswerDecision,
  onDecisionPrAction,
  notice = null,
}: {
  /** Every colony in the workspace, filtered by the caller. */
  sessions: Session[];
  onOpenColony: (id: string) => void;
  onOpenNotificationSettings: () => void;
  /** "Provider out of quota" cards (issue #767), shown first: one per provider, not one per colony. */
  quotaCards?: QuotaCard[];
  onQuotaAction?: (provider: string, body: QuotaActionRequest) => Promise<unknown>;
  /** The decisions inbox (issue #1036): repo decisions and pull requests that need a person. */
  decisions?: DecisionsView | null;
  onAnswerDecision?: (body: DecisionAnswerRequest) => Promise<unknown>;
  onDecisionPrAction?: (card: PrCard, action: PrAction) => Promise<unknown>;
  /** A phone-only card at the top of the list (the live-map prompt), inside the scroll root so it
   *  scrolls away with the page instead of sitting fixed over the cards below it. */
  notice?: ReactNode;
}): ReactElement {
  // Nothing server-side records a read; this is a local high-water mark, so "read" is per browser.
  const [readAt, setReadAt] = useState<number>(() => Number(stored(READ_AT) ?? 0));
  // The last switch's "was X → now Y" summary, kept after its card goes (issue #767).
  const [switched, setSwitched] = useState<QuotaActionReply | null>(null);
  const quotaAction = async (provider: string, body: QuotaActionRequest) => {
    const reply = await onQuotaAction?.(provider, body);
    if (isQuotaReply(reply) && (reply.changes?.length ?? 0) > 0) setSwitched(reply);
    return reply;
  };

  // A colony a quota card covers is answered on the card, not listed again as a question.
  // A colony a pull-request card covers (a policy hold) is listed there, with why, not twice.
  const onCards = quotaCardColonyIds(quotaCards);
  const onPrCards = prCardColonyIds(decisions);
  const needing = sessions.filter(needsYou);
  const waiting = needing.filter((session) => !onCards.has(session.id) && !onPrCards.has(session.id));
  const decisionCards = (decisions?.decisions.length ?? 0) + (decisions?.prs.length ?? 0);
  const needCount = new Set([...needing.map((session) => session.id), ...onCards]).size + decisionsCount(decisions, sessions);
  const questions = useOpenQuestions(sessions);

  // The activity log, one line per event at its own time. Read through the context directly, not
  // useApi, so the panel still renders where there is none (a static render, a test); a log that
  // will not load leaves the inbox reading the colony list, as it always did.
  const api = useContext(ApiContext);
  const [log, setLog] = useState<ActivityEntry[]>([]);
  useEffect(() => {
    if (!api) return;
    let stop = false;
    const refresh = async () => {
      try {
        const page = await api.activity({ limit: FETCH_LIMIT });
        if (!stop) setLog(page.entries);
      } catch {
        if (!stop) setLog([]);
      }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), REFRESH_MS);
    return () => {
      stop = true;
      window.clearInterval(timer);
    };
  }, [api]);

  const entries = inboxEntries(log, sessions);

  const markAllRead = () => {
    const now = Date.now();
    setReadAt(now);
    store(READ_AT, String(now));
  };

  return (
    <Page frameClassName="flex flex-col gap-4">
      {notice ? <div className="sm:hidden">{notice}</div> : null}
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

          <QuotaChangeSummary reply={switched} onDismiss={() => setSwitched(null)} />
          {quotaCards.map((card) => (
            <ProviderQuotaCard key={card.provider} card={card} onOpenColony={onOpenColony} onAction={quotaAction} />
          ))}

          {waiting.length === 0 && (quotaCards.length > 0 || decisionCards > 0) ? null : waiting.length === 0 ? (
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

          <DecisionsSection
            view={decisions}
            onAnswer={(body) => onAnswerDecision?.(body) ?? Promise.resolve()}
            onPrAction={(card, action) => onDecisionPrAction?.(card, action) ?? Promise.resolve()}
            onOpenColony={onOpenColony}
          />
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
              {sessions.length === 0 ? "no colonies in this workspace yet" : "nothing else from your colonies"}
            </div>
          ) : (
            <div className="overflow-hidden border-y border-border">
              {entries.map((entry) => {
                // A handled entry is never "unread": it wants nothing from you any more.
                const unread = entry.handled == null && Date.parse(entry.at) > readAt;
                return (
                  <button
                    key={entry.id}
                    type="button"
                    disabled={!entry.colonyId}
                    onClick={() => entry.colonyId && onOpenColony(entry.colonyId)}
                    className={`-mt-px grid w-full grid-cols-[10px_minmax(0,1fr)_auto] items-center gap-3 border-0 border-t border-solid border-border bg-transparent py-3 text-left text-text ${
                      entry.colonyId ? "cursor-pointer hover:bg-panel-2" : "cursor-default"
                    } ${unread ? "opacity-100" : "opacity-60"}`}
                  >
                    <span aria-hidden="true" className="h-[7px] w-[7px] rounded-full" style={{ background: KIND_DOT[entry.kind] }} />
                    <span className="min-w-0">
                      <span className={`block truncate text-[13.5px] ${unread ? "font-semibold" : "font-normal"}`}>{entry.text}</span>
                      <span className="mt-0.5 flex items-center gap-2 font-mono text-[11px] text-faint">
                        <span className="truncate">{entry.label}</span>
                        {entry.handled && (
                          <span className="shrink-0 rounded-full border border-border px-1.5 text-[10px] leading-4 text-muted">{entry.handled}</span>
                        )}
                      </span>
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
