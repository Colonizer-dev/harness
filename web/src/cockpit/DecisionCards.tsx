// The decisions inbox's cards (issue #1036): a repo decision looks like a colony's question card
// (AskUserCard's frame, option rows and "Other…" field), and a pull request that needs a person says
// why, links to it, and offers the quick actions that are safe from here. Nothing reaches GitHub
// until the operator clicks, and with external writes blocked the buttons are off and say why.
import { useState, type ReactElement } from "react";

import { Indicator } from "../components/AskUserCard";
import { IconCheck, IconExternal, IconGitPR, IconQuestion, IconRefresh, IconRepeat } from "../components/icons";
import { Badge, Button, Spinner, cx, type Tone } from "../components/ui";
import type { DecisionAnswerReply, DecisionAnswerRequest, DecisionCard, DecisionsView, PrAction, PrActionReply, PrActionRequest, PrCard, PrReason } from "../types";

/** The words and tone of each reason a pull request needs a person. */
export const PR_REASON: Record<PrReason, { label: string; tone: Tone }> = {
  commits_not_merged: { label: "Commits not merged", tone: "err" },
  policy_hold: { label: "Held by policy", tone: "err" },
  needs_redo: { label: "Needs a redo", tone: "warn" },
  conflicted: { label: "Conflicted", tone: "warn" },
  red_ci: { label: "CI red", tone: "err" },
  review_requested: { label: "Review requested", tone: "info" },
  awaiting_merge: { label: "Waiting for a merge", tone: "ok" },
};

export const PR_ACTION_LABEL: Record<PrAction, string> = {
  rerun: "Re-run failed jobs",
  redo: "Dispatch redo colony",
  dismiss: "Dismiss",
};

/** The free-text option, beside the parsed ones. */
export const OTHER = "\u0000other";

/** The answer a card's form sends, or null while it is not complete. */
export function decisionAnswer(card: DecisionCard, picked: string | null, other: string, note: string): DecisionAnswerRequest | null {
  const choice = picked === OTHER || card.options.length === 0 ? other.trim() : (picked ?? "").trim();
  if (!choice) return null;
  const trimmed = note.trim();
  return trimmed ? { id: card.id, choice, note: trimmed } : { id: card.id, choice };
}

/** `acme/web#12`, the line a card's header names. */
export function cardRef(repo: string, number: number | null): string {
  return number != null ? `${repo}#${number}` : repo;
}

/** The "Decisions" part of the inbox: its header, the repo decisions, then the pull requests. */
export function DecisionsSection({
  view,
  onAnswer,
  onPrAction,
  onOpenColony,
}: {
  view: DecisionsView | null;
  onAnswer: (body: DecisionAnswerRequest) => Promise<unknown>;
  onPrAction: (card: PrCard, action: PrAction) => Promise<unknown>;
  onOpenColony: (id: string) => void;
}): ReactElement | null {
  if (!view || view.decisions.length + view.prs.length === 0) return null;
  const blocked = view.writes_blocked ? (view.writes_blocked_reason ?? "external writes are blocked") : null;
  return (
    <section aria-label="Decisions" className="flex min-w-0 flex-col gap-3">
      <div className="flex items-baseline gap-2.5">
        <h2 className="m-0 text-[14px] font-medium">Decisions</h2>
        <span className="text-[12px] text-muted">
          {view.decisions.length + view.prs.length} from GitHub · not colony questions
        </span>
      </div>
      {blocked && (
        <div role="note" className="rounded-xl border border-border bg-panel-2/60 px-3 py-2 text-[12.5px] text-muted">
          Read-only: {blocked}.
        </div>
      )}
      {view.decisions.map((card) => (
        <DecisionCardView key={card.id} card={card} blocked={blocked} onAnswer={onAnswer} />
      ))}
      {view.prs.length > 0 && <h3 className="m-0 mt-1 text-[13px] font-medium text-muted">Pull requests that need you</h3>}
      {view.prs.map((card) => (
        <PrCardView key={card.id} card={card} blocked={blocked} onAction={onPrAction} onOpenColony={onOpenColony} />
      ))}
    </section>
  );
}

/** One repo decision, in the frame of a colony's question card. */
export function DecisionCardView({
  card,
  blocked,
  onAnswer,
}: {
  card: DecisionCard;
  /** Why answering is off, when it is. */
  blocked: string | null;
  onAnswer: (body: DecisionAnswerRequest) => Promise<unknown>;
}): ReactElement {
  const [picked, setPicked] = useState<string | null>(null);
  const [other, setOther] = useState("");
  const [note, setNote] = useState("");
  const [sending, setSending] = useState(false);
  const body = decisionAnswer(card, picked, other, note);
  const freeText = card.options.length === 0;
  const name = `decision-${card.id}`;
  const submit = async () => {
    if (!body || sending || blocked) return;
    setSending(true);
    try {
      await onAnswer(body);
    } finally {
      setSending(false);
    }
  };
  return (
    <form
      className="overflow-hidden rounded-2xl border border-border bg-panel shadow-[var(--shadow)]"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div className="flex items-center gap-2 border-b border-border bg-accent-soft/60 px-4 py-2.5">
        <span className="grid size-6 place-items-center rounded-full bg-accent text-on-accent">
          <IconQuestion size={14} />
        </span>
        <span className="text-[13px] font-semibold">A decision for you</span>
        <a
          href={card.url}
          target="_blank"
          rel="noreferrer"
          className="ml-auto truncate font-mono text-[12px] text-muted hover:text-text"
          title={card.title}
        >
          {cardRef(card.repo, card.number)}
        </a>
      </div>
      <fieldset className="min-w-0 px-4 py-4" disabled={sending || blocked !== null}>
        <div className="mb-2 flex flex-wrap items-center gap-2">
          <span className="rounded-md bg-panel-3 px-2 py-0.5 text-[11px] font-semibold uppercase tracking-wide text-muted">
            {card.labelled ? "needs-decision" : card.source === "comment" ? "from a comment" : "from the issue"}
          </span>
          <span className="truncate text-[11.5px] text-faint">{card.title}</span>
        </div>
        <h3 className="mb-3 text-[15px] font-semibold leading-snug [text-wrap:pretty]">{card.question}</h3>
        {card.more > 0 && (
          <p className="-mt-1.5 mb-3 text-[12px] text-muted">
            The issue asks {card.more} more {card.more === 1 ? "question" : "questions"}; answer them on GitHub.
          </p>
        )}
        <div className="grid gap-2" role="radiogroup" aria-label={card.question}>
          {card.options.map((option) => (
            <label
              key={option}
              className={cx(
                "flex min-h-14 cursor-pointer gap-3 rounded-xl border p-3 transition-colors has-[input:focus-visible]:ring-2 has-[input:focus-visible]:ring-[var(--accent-ring)]",
                picked === option ? "border-accent bg-accent-soft/50" : "border-border hover:border-border-strong hover:bg-panel-2",
              )}
            >
              <input type="radio" name={name} className="sr-only" checked={picked === option} onChange={() => setPicked(option)} />
              <Indicator multi={false} checked={picked === option} />
              <span className="min-w-0 flex-1 font-medium leading-snug">{option}</span>
            </label>
          ))}
          <label
            className={cx(
              "flex min-h-14 cursor-pointer gap-3 rounded-xl border p-3 transition-colors",
              picked === OTHER || freeText ? "border-accent bg-accent-soft/50" : "border-dashed border-border-strong hover:bg-panel-2",
            )}
          >
            {!freeText && (
              <input type="radio" name={name} className="sr-only" checked={picked === OTHER} onChange={() => setPicked(OTHER)} />
            )}
            {!freeText && <Indicator multi={false} checked={picked === OTHER} />}
            <span className="min-w-0 flex-1">
              <span className="block font-medium">{freeText ? "Your decision" : "Other…"}</span>
              {(picked === OTHER || freeText) && (
                <input
                  value={other}
                  onChange={(e) => setOther(e.target.value)}
                  placeholder="Your answer"
                  aria-label={`Answer for: ${card.question}`}
                  className="mt-2 w-full rounded-lg border border-border bg-panel px-3 py-2 text-sm outline-none focus:border-accent focus:ring-2 focus:ring-[var(--accent-ring)]"
                />
              )}
            </span>
          </label>
          <textarea
            value={note}
            onChange={(e) => setNote(e.target.value)}
            rows={2}
            placeholder="A note for the comment (optional)"
            aria-label="Note"
            className="w-full resize-y rounded-lg border border-border bg-panel px-3 py-2 text-[13px] outline-none focus:border-accent focus:ring-2 focus:ring-[var(--accent-ring)]"
          />
        </div>
      </fieldset>
      <div className="flex flex-wrap items-center gap-3 border-t border-border bg-panel-2/60 px-4 py-3">
        <p className="min-w-0 flex-1 text-[12.5px] text-muted">
          {blocked
            ? `Answering is off: ${blocked}.`
            : sending
              ? "Posting your decision on GitHub…"
              : body
                ? `Posts one "Decision (maintainer):" comment${card.labelled ? " and removes needs-decision" : ""}.`
                : "Pick an option or write your answer."}
        </p>
        <Button type="submit" variant="primary" disabled={!body || sending || blocked !== null}>
          {sending ? <Spinner /> : <IconCheck size={15} />}
          Post decision
        </Button>
      </div>
    </form>
  );
}

/** One pull request that needs a person: why, where, and the safe quick actions. */
export function PrCardView({
  card,
  blocked,
  onAction,
  onOpenColony,
}: {
  card: PrCard;
  blocked: string | null;
  onAction: (card: PrCard, action: PrAction) => Promise<unknown>;
  onOpenColony: (id: string) => void;
}): ReactElement {
  const [running, setRunning] = useState<PrAction | null>(null);
  const reason = PR_REASON[card.reason];
  const run = async (action: PrAction) => {
    if (running || blocked) return;
    setRunning(action);
    try {
      await onAction(card, action);
    } finally {
      setRunning(null);
    }
  };
  return (
    <div className="overflow-hidden rounded-2xl border border-border bg-panel shadow-[var(--shadow)]">
      <div className="flex items-center gap-2 border-b border-border bg-panel-2/60 px-4 py-2.5">
        <span className="grid size-6 place-items-center rounded-full bg-panel-3 text-muted">
          <IconGitPR size={14} />
        </span>
        <Badge tone={reason.tone}>{reason.label}</Badge>
        <span className="ml-auto truncate font-mono text-[12px] text-muted">{cardRef(card.repo, card.number)}</span>
      </div>
      <div className="px-4 py-3.5">
        <div className="text-[15px] font-semibold leading-snug [text-wrap:pretty]">{card.title}</div>
        <p className="mt-1 text-[13px] text-muted [text-wrap:pretty]">Why it needs you: {card.why}.</p>
      </div>
      <div className="flex flex-wrap items-center gap-2 border-t border-border bg-panel-2/60 px-4 py-3">
        {card.actions.map((action) => (
          <Button
            key={action}
            size="sm"
            variant="secondary"
            disabled={running !== null || blocked !== null}
            title={blocked ? `Off: ${blocked}` : undefined}
            onClick={() => void run(action)}
          >
            {running === action ? (
              <Spinner />
            ) : action === "rerun" ? (
              <IconRefresh size={13} />
            ) : action === "dismiss" ? (
              <IconCheck size={13} />
            ) : (
              <IconRepeat size={13} />
            )}
            {PR_ACTION_LABEL[action]}
          </Button>
        ))}
        {card.colony && (
          <Button size="sm" variant="ghost" onClick={() => onOpenColony(card.colony!)}>
            Open colony
          </Button>
        )}
        {card.url && (
          <a
            href={card.url}
            target="_blank"
            rel="noreferrer"
            className="ml-auto inline-flex items-center gap-1.5 text-[12.5px] text-muted no-underline hover:text-text"
          >
            Open on GitHub <IconExternal size={12} />
          </a>
        )}
      </div>
    </div>
  );
}

/** The toast after an action, in plain words. */
export function prActionSummary(action: PrAction, reply: { rerun?: number[]; colony?: string } | null | undefined): string {
  if (action === "dismiss") return "Dismissed";
  if (action === "redo") return reply?.colony ? `Redo colony ${reply.colony} dispatched` : "Redo colony dispatched";
  const n = reply?.rerun?.length ?? 0;
  return n === 1 ? "Re-running the failed jobs of 1 run" : `Re-running the failed jobs of ${n} runs`;
}

/** Posts an answer and says how it went; the error goes to the toast, never thrown at the card. */
export async function runDecisionAnswer(
  send: (body: DecisionAnswerRequest) => Promise<DecisionAnswerReply>,
  say: (message: string, tone?: "error") => void,
  body: DecisionAnswerRequest,
): Promise<DecisionAnswerReply | null> {
  try {
    const reply = await send(body);
    if (reply.label_error) say(`Decision posted on ${body.id}, but needs-decision could not be removed: ${reply.label_error}`, "error");
    else say(`Decision posted on ${body.id}${reply.label_removed ? " and needs-decision removed" : ""}`);
    return reply;
  } catch (e) {
    say(e instanceof Error ? e.message : String(e), "error");
    return null;
  }
}

/** Runs a pull-request card's quick action and says how it went. */
export async function runPrAction(
  send: (body: PrActionRequest) => Promise<PrActionReply>,
  say: (message: string, tone?: "error") => void,
  card: PrCard,
  action: PrAction,
): Promise<PrActionReply | null> {
  try {
    const reply = await send({ id: card.id, action });
    say(prActionSummary(action, reply));
    return reply;
  } catch (e) {
    say(e instanceof Error ? e.message : String(e), "error");
    return null;
  }
}
