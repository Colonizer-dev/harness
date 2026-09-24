// The pane that slides in beside the nest when you pick the mothership or a chamber: what it is,
// what it costs, what it is waiting for, and the one or two things worth doing about it.
//
// Everything here comes from the colony list and /api/status, plus live findings from hunter runs.
// There is deliberately no other per-colony event history: the mothership keeps none for the browser, and a made-up one would read as fact.
import { useEffect, useState, type ReactElement } from "react";

import { AntAvatar } from "../components/AntAvatar";
import { AskUserCard, QuestionActionsContext, type QuestionActions } from "../components/AskUserCard";
import { Avatar } from "../components/Avatar";
import type { SectionId } from "../components/SettingsDialog";
import { SESSION_STATUS, type Tone, cx, isLive, timeAgo } from "../components/ui";
import { useBehind } from "../behind";
import { useApi } from "../context";
import { needsYou } from "../notifications";
import { useSessionDiagnosis } from "../sessionDiagnosis";
import { formatCost } from "../spend";
import type { StreamState, SubagentView } from "../sessionStream";
import { parentOf } from "../stack";
import type { FindingRecord, HarnessStatus, Question, Session, UpdateStatus } from "../types";
import { bootMedians, bootView } from "./bootTiming";
import { chains, type FindingChain } from "./findings";

const TONE_VAR: Record<Tone, string> = {
  neutral: "var(--faint)",
  info: "var(--info)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  err: "var(--err)",
  accent: "var(--accent)",
};

export type InspectorTarget = { kind: "mothership" } | { kind: "colony"; session: Session };

/** A question the colony asked that the operator has not answered yet. */
export interface PendingQuestion {
  id: string;
  questions: Question[];
  /** The frame's `ts` when the question opened; null when the replay omitted one. */
  asked_at: string | null;
}

/**
 * The questions still waiting in a stream, in the order they were asked. An answered block carries
 * its `.answer`, so it drops out here and the pane clears on its own as `question_answered` arrives.
 */
export function pendingQuestionsOf(state: StreamState): PendingQuestion[] {
  const pending: PendingQuestion[] = [];
  for (const message of state.messages) {
    for (const block of message.blocks) {
      if (block.kind === "question" && !block.answer) {
        pending.push({ id: block.id, questions: block.questions, asked_at: block.asked_at ?? null });
      }
    }
  }
  return pending;
}

/** AskUserCard declares assistant-ui's injected part props; the chat runtime supplies the rest,
    and the card itself only reads the three fields set here. Cast like the stream adapter does. */
type AskUserCardProps = Parameters<typeof AskUserCard>[0];

function Fact({ label, value, className, title }: { label: string; value: string; className?: string; title?: string }): ReactElement {
  return (
    <div className={cx("bg-bg px-3 py-2.5", className)} title={title}>
      <div className="text-[12px] text-muted lowercase first-letter:uppercase">{label}</div>
      <div className="mt-0.5 truncate text-[15px] font-semibold tracking-[-0.01em] text-text tabular-nums">{value}</div>
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactElement | ReactElement[] }): ReactElement {
  return (
    <div>
      <div className="mb-2 text-[13px] font-medium text-text lowercase first-letter:uppercase">{title}</div>
      {children}
    </div>
  );
}

const money = (amount: number | null | undefined) => formatCost(amount ?? null);

/** `#149` from a pull-request URL; null when it is not shaped like one. */
function prNumber(url: string): string | null {
  const match = /\/pull\/(\d+)/.exec(url);
  return match ? `#${match[1]}` : null;
}

/** How far the last publish got, in the mothership's own words. */
const STAGE: Record<string, string> = {
  committed: "committed",
  pushed: "pushed",
  pr_opened: "opened",
};

// ---------------------------------------------------------------------------
// Findings, folded from the colony's ledger (see findings.ts)
// ---------------------------------------------------------------------------

/** A stage on a finding's card: its chip label and how strong the color should be. */
interface FindingStage {
  label: string;
  tone: Tone;
}

/** The order a finding's stages read in; lines the ledger lacks are simply skipped. */
const STAGE_ORDER: FindingRecord["state"][] = [
  "validated",
  "filed",
  "fix_colony",
  "review",
  "merged",
  "blocked",
  "duplicate",
  "rejected",
  "error",
];

/** The chip a record line earns on its own; the fix colony and the verdict read off the fold. */
const STAGE_LABEL: Record<FindingRecord["state"], string> = {
  validated: "validated",
  filed: "filed",
  fix_colony: "fix",
  review: "review",
  merged: "merged",
  blocked: "merge blocked",
  duplicate: "duplicate",
  rejected: "rejected",
  error: "error",
};

/** The trail a chain actually walked, judged by the record lines that built it. */
function trail(chain: FindingChain): FindingStage[] {
  const seen = new Set(chain.records.map((r) => r.state));
  const stages: FindingStage[] = [{ label: "found", tone: "neutral" }];
  for (const state of STAGE_ORDER) {
    if (!seen.has(state)) continue;
    let label = STAGE_LABEL[state];
    let tone: Tone = "neutral";
    if (state === "fix_colony" && chain.fix_session) label = `fix ${chain.fix_session}`;
    if (state === "review") {
      label = chain.verdict ? `review ${chain.verdict}` : "review";
      tone = chain.verdict === "fail" ? "err" : "neutral";
    }
    if (state === "merged") tone = "ok";
    if (state === "blocked" || state === "duplicate" || state === "rejected") tone = "warn";
    if (state === "error") tone = "err";
    stages.push({ label, tone });
  }
  return stages;
}

const isUrl = (value: string) => /^https?:\/\//.test(value);

/** The one line under the trail that explains a terminal: why it was rejected or its merge blocked, the error, or the finding it duplicated. */
function noteFor(chain: FindingChain): { text: string; tone: Tone; href: string | null } | null {
  const states = new Set(chain.records.map((r) => r.state));
  if (states.has("rejected") && chain.reason) return { text: chain.reason, tone: "warn", href: null };
  if (states.has("blocked") && chain.reason) return { text: chain.reason, tone: "warn", href: null };
  if (states.has("error") && chain.reason) return { text: chain.reason, tone: "err", href: null };
  if (states.has("duplicate") && chain.duplicate_of)
    return { text: chain.duplicate_of, tone: "warn", href: isUrl(chain.duplicate_of) ? chain.duplicate_of : null };
  return null;
}

/** One finding's whole career, folded; a status readout, not a log viewer. */
function FindingRow({ chain }: { chain: FindingChain }): ReactElement {
  const stages = trail(chain);
  const note = noteFor(chain);
  const links = [
    ...(chain.issue && isUrl(chain.issue) ? [{ href: chain.issue, label: "issue" }] : []),
    ...(chain.pr && isUrl(chain.pr) ? [{ href: chain.pr, label: "pr" }] : []),
  ];
  return (
    <div className="rounded-md bg-panel-2 px-3 py-2">
      <div className="text-[12.5px] font-semibold leading-snug" title={chain.title}>
        {chain.title}
      </div>
      <div className="mt-1.5 flex flex-wrap items-center gap-x-1.5 gap-y-1">
        {stages.map((stage, i) => (
          <span key={`${i}-${stage.label}`} className="flex items-center gap-1.5">
            {i > 0 && (
              <span aria-hidden="true" className="font-mono text-[10px] text-faint">
                →
              </span>
            )}
            <span
              className="rounded-[5px] border border-border px-1.5 py-px font-mono text-[10px] tracking-wide"
              style={{ color: TONE_VAR[stage.tone] }}
            >
              {stage.label}
            </span>
          </span>
        ))}
      </div>
      {note && (
        <div className={`mt-1.5 text-[11px] leading-snug ${note.tone === "err" ? "text-err" : "text-warn"}`}>
          {note.href ? (
            <a href={note.href} target="_blank" rel="noreferrer" className="no-underline hover:underline">
              duplicate of {note.href}
            </a>
          ) : (
            note.text
          )}
        </div>
      )}
      {links.length > 0 && (
        <div className="mt-1 flex gap-3">
          {links.map((link) => (
            <a
              key={link.label}
              href={link.href}
              target="_blank"
              rel="noreferrer"
              className="text-[11.5px] font-semibold text-accent no-underline hover:underline"
            >
              {link.label} ↗
            </a>
          ))}
        </div>
      )}
    </div>
  );
}

export function Inspector({
  target,
  avatarUrl,
  settlers,
  pendingQuestions,
  questionActions,
  sessions,
  status,
  liveCount,
  queuedCount,
  needCount,
  spend,
  maxParallel,
  update,
  onClose,
  onOpenColony,
  onStop,
  onResume,
  onLaunch,
  onOpenSettings,
}: {
  /** Null while nothing is picked: the pane stays mounted and explains itself instead of blinking out. */
  target: InspectorTarget | null;
  /** The colony's org avatar; null when nothing knows one and the initial stands in. */
  avatarUrl: string | null;
  /** Real settlers, present only while this colony's stream is open; empty otherwise. */
  settlers: SubagentView[];
  /** The colony's unanswered questions, straight from its stream; empty until one opens. */
  pendingQuestions: PendingQuestion[];
  /** How to answer from here — the same actions the chat card uses. */
  questionActions: QuestionActions;
  /** Every colony the mothership knows; the stack fact resolves the parent against it. */
  sessions: Session[];
  status: HarnessStatus | null;
  liveCount: number;
  queuedCount: number;
  needCount: number;
  /** What this workspace's colonies have spent in total; the API keeps no daily history. */
  spend: number | null;
  maxParallel: number | null;
  update: UpdateStatus | null;
  onClose: () => void;
  onOpenColony: (id: string) => void;
  onStop: (id: string) => void;
  onResume: (id: string) => void;
  onLaunch: () => void;
  onOpenSettings: (section: SectionId) => void;
}): ReactElement {
  if (!target) {
    return (
      <aside
        className="cockpit nest-inspector flex w-[360px] shrink-0 flex-col overflow-hidden border-l border-border bg-bg"
        style={{ animation: "ck-slide 0.28s cubic-bezier(.2,.7,.2,1) both" }}
        aria-label="nothing selected"
      >
        <div className="flex items-center gap-3 border-b border-border px-4 pb-3 pt-4">
          <div className="min-w-0 flex-1">
            <div className="font-mono text-[11.5px] text-muted">inspector</div>
            <div className="mt-0.5 text-[15px] font-semibold tracking-[-0.01em] leading-tight">nothing selected</div>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="close"
            className="grid h-[26px] w-[26px] shrink-0 cursor-pointer place-items-center rounded-full text-faint hover:bg-panel-2 hover:text-text"
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" aria-hidden="true">
              <path d="M6 6l12 12M18 6 6 18" />
            </svg>
          </button>
        </div>
        <div className="grid flex-1 place-items-center px-8 text-center text-[12.5px] leading-relaxed text-muted">
          pick a chamber in the nest and its question, status and cost land here.
        </div>
      </aside>
    );
  }
  const mothership = target.kind === "mothership";
  const session = target.kind === "colony" ? target.session : null;
  const tone = session ? (SESSION_STATUS[session.status]?.tone ?? "neutral") : "neutral";
  const edge = TONE_VAR[tone];

  // What the colony is stacked on. A parent that has left the list falls back to the raw id, which
  // still says more than leaving the fact out of a stacked colony's card.
  const stackParent = session ? parentOf(sessions, session) : null;
  const stackedOn = stackParent
    ? `${stackParent.repo}${stackParent.issue != null ? `#${stackParent.issue}` : ""} · ${stackParent.branch}`
    : (session?.parent ?? null);
  const boot = session ? bootView(session.boot_timing, session.status === "starting") : null;
  // A queued colony waiting on another colony names it instead of reading as a generic queue
  // entry; a queued colony with no link still reads as plain "Queued".
  const queuedBehind = session?.queued_behind ?? null;
  const statusLabel =
    session?.status === "queued" && queuedBehind
      ? `Queued behind ${queuedBehind}`
      : (session ? (SESSION_STATUS[session.status]?.label ?? session.status) : "");
  // Cross-colony boot medians for the mothership pane; the colony branch never reads it.
  const medianBoot = mothership ? bootMedians(sessions) : null;

  // The finding ledger is a separate call, keyed by colony: the event stream does not carry it, and
  // a colony that never validated a finding has none, so an error reads as "nothing yet".
  const api = useApi();
  // How far the colony branch lags origin/{base} (issue #173); display-only, like everything here.
  const { behind } = useBehind(session);
  // Why the colony is not progressing, polled from the single-session route (issue #230).
  const { diagnosis, recentEvents } = useSessionDiagnosis(session);
  const [findings, setFindings] = useState<FindingChain[]>([]);
  useEffect(() => {
    if (!session) return;
    let active = true;
    api.findings(session.id).then(
      (records) => {
        if (active) setFindings(chains(records));
      },
      () => {
        if (active) setFindings([]);
      },
    );
    return () => {
      active = false;
    };
  }, [api, session?.id]);

  // Only rows the mothership actually reported: an absent fact is left out rather than guessed at.
  const connections: { label: string; dot: string; section: SectionId }[] = [];
  if (status) {
    connections.push({
      label: status.github.connected ? `@${status.github.login ?? "github"}` : "github · not connected",
      dot: status.github.connected ? "var(--ok)" : "var(--warn)",
      section: "connections",
    });
    connections.push({
      label: status.claude.configured ? `claude · ${status.claude.kind ?? "configured"}` : "claude · not configured",
      dot: status.claude.configured ? "var(--ok)" : "var(--warn)",
      section: "connections",
    });
    if (status.sandbox.msb_version) {
      connections.push({ label: `microvms · msb ${status.sandbox.msb_version}`, dot: "var(--ok)", section: "runtime" });
    }
    if (status.mesh?.enabled) {
      connections.push({
        label: `mesh · ${status.mesh.nodes ?? 0} nodes`,
        dot: status.mesh.error ? "var(--err)" : "var(--ok)",
        section: "module:mesh",
      });
    }
  }

  return (
    <aside
      className="cockpit nest-inspector flex w-[360px] shrink-0 flex-col overflow-hidden border-l border-border bg-bg"
      style={{ animation: "ck-slide 0.28s cubic-bezier(.2,.7,.2,1) both" }}
      aria-label={mothership ? "mothership" : session ? "colony" : "unknown target"}
    >
      <div className="flex items-start gap-3 border-b border-border px-4 pb-3 pt-4">
        {mothership ? (
          <span className="grid h-[34px] w-[34px] shrink-0 place-items-center rounded-full border border-border text-accent">
            <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 2.8 20 7.4v9.2L12 21.2 4 16.6V7.4z" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round" />
              <circle cx="12" cy="12" r="2.6" fill="currentColor" />
            </svg>
          </span>
        ) : (
          session && <Avatar name={session.repo.split("/")[0]} src={avatarUrl} size={34} rounded="lg" />
        )}
        <div className="min-w-0 flex-1">
          <div className="truncate font-mono text-[11.5px] text-muted">
            {mothership ? "mothership" : `${session?.repo}${session?.issue != null ? `#${session.issue}` : ""}`}
          </div>
          <div className="mt-0.5 text-[15px] font-semibold tracking-[-0.01em] leading-tight text-pretty">
            {mothership ? "the colonizer app on this machine" : session?.issue_title || "no title yet"}
          </div>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="close"
          className="grid h-[26px] w-[26px] shrink-0 cursor-pointer place-items-center rounded-full text-faint hover:bg-panel-2 hover:text-text"
        >
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" aria-hidden="true">
            <path d="M6 6l12 12M18 6 6 18" />
          </svg>
        </button>
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-4 pb-4 pt-3.5">
        {mothership ? (
          <>
            <div className="grid shrink-0 grid-cols-2 gap-px overflow-hidden border-y border-border bg-border">
              <Fact label="LIVE" value={maxParallel != null ? `${liveCount} / ${maxParallel}` : String(liveCount)} />
              <Fact label="QUEUED" value={String(queuedCount)} />
              <Fact label="NEED YOU" value={String(needCount)} />
              <Fact label="SPENT" value={money(spend)} />
            </div>

            {connections.length > 0 && (
              <Section title="CONNECTIONS">
                <div className="flex flex-col gap-0.5">
                  {connections.map((row) => (
                    <button
                      key={row.label}
                      type="button"
                      onClick={() => onOpenSettings(row.section)}
                      className="flex cursor-pointer items-center gap-2.5 rounded-md bg-panel-2 px-2.5 py-2 text-left font-mono text-[12px] text-muted hover:text-text"
                    >
                      <span aria-hidden="true" className="h-1.5 w-1.5 rounded-full" style={{ background: row.dot }} />
                      <span className="flex-1 truncate">{row.label}</span>
                      <span aria-hidden="true" className="text-faint">
                        ›
                      </span>
                    </button>
                  ))}
                </div>
              </Section>
            )}

            {medianBoot && (
              // Per-phase medians across recent finished boots, in the colony BOOT rows' shape; after
              // CONNECTIONS so the update chip stays last as the call to action.
              <Section title={`BOOT · MEDIAN OF ${medianBoot.count}`}>
                <div className="flex flex-col gap-1.5">
                  {medianBoot.rows.map((row) => (
                    <div
                      key={row.name}
                      className={cx("flex gap-2.5 text-[12px]", row.slowest ? "font-semibold text-text" : "text-muted")}
                    >
                      <span className="min-w-0 flex-1 truncate font-mono text-[11px]">{row.name}</span>
                      {row.slowest && <span className="font-mono text-[10px] tracking-[0.1em] text-warn">SLOWEST</span>}
                      <span className="shrink-0 font-mono text-[11px] tabular-nums">{row.duration}</span>
                    </div>
                  ))}
                  <div className="text-[12px] text-faint">{medianBoot.summary}</div>
                </div>
              </Section>
            )}

            {update && (
              <button
                type="button"
                onClick={() => onOpenSettings("updates")}
                className={`flex cursor-pointer items-center gap-2.5 rounded-md border px-3 py-2.5 transition-colors hover:bg-panel-2 text-left ${
                  update.available ? "border-accent" : "border-border"
                }`}
              >
                <span className="min-w-0 flex-1">
                  <span className="block text-[13px] font-semibold">colonizer {update.installed.version}</span>
                  <span className="block truncate text-[12px] text-muted">
                    {update.available && update.latest ? `${update.latest.version} is out` : "up to date"}
                  </span>
                </span>
                <span className="text-[12.5px] font-semibold text-accent">{update.available ? "update" : "about"}</span>
              </button>
            )}
          </>
        ) : (
          session ? (
            <>
              <div className="flex items-center gap-2 font-mono text-[11px] tracking-[0.1em]" style={{ color: edge }}>
                <span aria-hidden="true" className="h-[7px] w-[7px] rounded-full" style={{ background: edge }} />
                {statusLabel}
                <span className="tracking-normal text-faint">· {timeAgo(session.created_at)}</span>
              </div>

              {needsYou(session) && (
                <div className="border-y border-border border-l-2 border-l-warn bg-panel-2 px-3.5 py-3">
                  <div className="text-[12.5px] font-medium text-warn">Waiting on you</div>
                  <div className="mt-1.5 text-[13.5px] font-semibold">
                    {session.attention ? "the watchdog flagged this colony" : "the colony is waiting on your answer"}
                  </div>

                  {/* The question itself, answered from the pane. The frame's `ts` is when it was asked;
                      an attention row stands in when the stream had no timestamp to carry. */}
                  {pendingQuestions.length > 0 && (
                    <div className="mt-3 flex flex-col gap-4">
                      {pendingQuestions.map((q) => {
                        const askedAt =
                          q.asked_at ?? (session.attention?.reason === "waiting_for_answer" ? session.attention.since : null);
                        return (
                          <div key={q.id} className="flex flex-col gap-1.5">
                            <div className="text-[11.5px] text-muted">
                              {askedAt ? `asked ${timeAgo(askedAt)}` : "waiting for your answer"}
                            </div>
                            <QuestionActionsContext.Provider value={questionActions}>
                              <AskUserCard
                                {...({ toolCallId: q.id, args: { questions: q.questions }, result: undefined } as unknown as AskUserCardProps)}
                              />
                            </QuestionActionsContext.Provider>
                          </div>
                        );
                      })}
                    </div>
                  )}

                  {/* The list says this colony needs you but the stream has not shown the question yet (or
                      anymore): name what is happening instead of leaving the box blank. */}
                  {pendingQuestions.length === 0 && (
                    <div className="mt-3 rounded-md border border-border bg-bg px-3 py-2.5 text-[12.5px] text-muted">
                      {questionActions.blockedBy === "disconnected" ? "loading the question…" : "this colony has no pending question"}
                    </div>
                  )}

                  {/* The full chat is still the deeper answer: the card is the quick one. */}
                  <button
                    type="button"
                    onClick={() => onOpenColony(session.id)}
                    className="mt-2.5 w-full cursor-pointer rounded-md bg-text px-3 py-2 text-left text-[13px] font-medium text-bg transition-opacity hover:opacity-85"
                  >
                    open the colony and answer →
                  </button>
                </div>
              )}

              {diagnosis && (
                // Why this colony is not progressing, in the mothership's own words (issue #230).
                <Section title="STATUS">
                  <div
                    role="status"
                    className="rounded-md bg-panel-2 px-3 py-2 text-[12.5px] leading-snug [overflow-wrap:anywhere]"
                  >
                    {diagnosis.text}
                  </div>
                </Section>
              )}

              {recentEvents.length > 0 && (
                <Section title="RECENT EVENTS">
                  <details className="rounded-md bg-panel-2 px-3 py-2">
                    <summary className="cursor-pointer text-[12px] font-semibold text-muted">
                      {recentEvents.length} recent event{recentEvents.length === 1 ? "" : "s"}
                    </summary>
                    <ol className="mt-1.5 flex flex-col gap-1">
                      {recentEvents.map((event) => (
                        <li key={event.seq} className="text-[12px] leading-snug [overflow-wrap:anywhere]">
                          <span className="font-mono text-[11px] text-faint">{event.type}</span>{" "}
                          <span className="text-muted">{event.summary}</span>
                        </li>
                      ))}
                    </ol>
                  </details>
                </Section>
              )}

              <div className="grid shrink-0 grid-cols-2 gap-px overflow-hidden border-y border-border bg-border">
                <Fact label="SETTLERS" value={settlers.length > 0 ? String(settlers.length) : "—"} />
                <Fact label="COST" value={money(session.cost_usd)} />
                <Fact label="MESH" value={session.mesh?.name ?? "—"} />
                <Fact label="AGENT" value={session.agent} />
                {stackedOn && (
                  <Fact
                    className="col-span-2"
                    label="STACKED ON"
                    value={stackedOn}
                    title={stackParent?.issue_title}
                  />
                )}
                {queuedBehind && (
                  <Fact
                    className="col-span-2"
                    label="QUEUED BEHIND"
                    value={queuedBehind}
                    title={sessions.find((s) => s.id === queuedBehind)?.issue_title}
                  />
                )}
                {session.needs_rebase && (
                  <Fact
                    label="REBASE"
                    value="needs rebase"
                    title="this colony's branch has diverged from its base"
                  />
                )}
              </div>

              {session.pr_url && (
                // What came back. The prototype lists per-file +/- counts; the API reports no diff
                // stats, so this carries the address and the branch it came from instead of inventing them.
                <div className="flex flex-col gap-1.5 border-y border-border px-0 py-3">
                  <div className="flex items-center justify-between gap-2.5">
                    <span className="text-[12.5px] font-medium text-ok">Pull request</span>
                    <span className="font-mono text-[11px] text-faint">
                      {[prNumber(session.pr_url), session.publish_stage ? STAGE[session.publish_stage] : null]
                        .filter(Boolean)
                        .join(" · ")}
                    </span>
                  </div>
                  <div className="text-[13.5px] font-semibold leading-snug">{session.issue_title || "no title yet"}</div>
                  {session.summary && session.summary !== session.issue_title && <div className="mt-0.5 text-[12.5px] leading-snug text-muted">{session.summary}</div>}
                  <div className="truncate font-mono text-[11.5px] text-muted">
                    {session.branch}
                    {session.base ? ` → ${session.base}` : ""}
                    {session.base && typeof behind === "number" && behind > 0 ? ` · behind by ${behind}` : ""}
                    {session.base && behind === 0 ? " · up to date" : ""}
                  </div>
                  <a
                    href={session.pr_url}
                    target="_blank"
                    rel="noreferrer"
                    className="text-[12.5px] font-semibold text-accent no-underline hover:underline"
                  >
                    open on github ↗
                  </a>
                </div>
              )}

              {settlers.length > 0 && (
                <Section title="SETTLERS">
                  <div className="flex flex-col gap-0.5">
                    {settlers.map((settler, i) => (
                      <div
                        key={settler.agent.id}
                        className="grid grid-cols-[40px_minmax(0,1fr)_auto] items-center gap-2.5 rounded-md bg-panel-2 px-2 py-1.5"
                      >
                        <span
                          className="grid h-[30px] w-10 place-items-center rounded-lg transition-colors duration-700"
                          style={{ background: settler.state === "done" ? "var(--ok-soft)" : "var(--accent-soft)" }}
                        >
                          <AntAvatar state={settler.state} role={settler.role} size={32} phase={i} ground={false} framed={false} />
                        </span>
                        <span className="min-w-0">
                          <span className="block text-[12.5px] font-semibold text-accent">{settler.name}</span>
                          <span className="block truncate font-mono text-[11px] text-muted">
                            {settler.current?.name ?? settler.last?.name ?? "—"}
                          </span>
                        </span>
                        <span className="whitespace-nowrap font-mono text-[10px] text-faint tabular-nums">
                          {settler.steps} steps
                        </span>
                      </div>
                    ))}
                  </div>
                </Section>
              )}

              <Section title="TIMELINE">
                <div className="flex flex-col gap-1.5">
                  <div className="flex gap-2.5 text-[12px] text-muted">
                    <span className="w-14 shrink-0 font-mono text-[11px] text-faint">started</span>
                    <span className="min-w-0">{timeAgo(session.created_at)}</span>
                  </div>
                  <div className="flex gap-2.5 text-[12px] text-muted">
                    <span className="w-14 shrink-0 font-mono text-[11px] text-faint">last move</span>
                    <span className="min-w-0">{timeAgo(session.last_activity_at ?? session.updated_at)}</span>
                  </div>
                  {session.error && (
                    <div className="flex gap-2.5 text-[12px] text-err">
                      <span className="w-14 shrink-0 font-mono text-[11px]">error</span>
                      <span className="min-w-0 [overflow-wrap:anywhere]">{session.error}</span>
                    </div>
                  )}
                </div>
              </Section>

              {boot && (
                // Where the launch's time went, phase by phase in boot order. The slowest phase is the
                // one worth a look, so it reads in full ink while the rest stay muted.
                <Section title="BOOT">
                  <div className="flex flex-col gap-1.5">
                    {boot.rows.map((row, i) => (
                      <div
                        key={`${i}:${row.name}`}
                        className={cx("flex gap-2.5 text-[12px]", row.slowest ? "font-semibold text-text" : "text-muted")}
                      >
                        <span className="min-w-0 flex-1 truncate font-mono text-[11px]">{row.name}</span>
                        {row.slowest && <span className="font-mono text-[10px] tracking-[0.1em] text-warn">SLOWEST</span>}
                        <span className="shrink-0 font-mono text-[11px] tabular-nums">{row.duration}</span>
                      </div>
                    ))}
                    <div className={cx("text-[12px] text-faint", boot.rows.length === 0 && "rounded-md bg-panel-2 px-3 py-2")}>
                      {boot.summary}
                    </div>
                  </div>
                </Section>
              )}

              <Section title="FINDINGS">
                <div className="flex flex-col gap-2">
                  {findings.length === 0 ? (
                    <div className="rounded-md bg-panel-2 px-3 py-2 text-[12px] text-faint">
                      nothing validated into findings yet
                    </div>
                  ) : (
                    findings.map((chain) => <FindingRow key={chain.title} chain={chain} />)
                  )}
                </div>
              </Section>
            </>
          ) : (
            <div className="grid flex-1 place-items-center px-8 text-center text-[12.5px] leading-relaxed text-muted">
              a target the cockpit doesn't know — nothing to show here yet.
            </div>
          )
        )}
      </div>

      <div className="flex gap-2 border-t border-border px-4 py-3">
        {mothership ? (
          <>
            <button
              type="button"
              onClick={onLaunch}
              className="flex-1 cursor-pointer rounded-md bg-text px-3 py-2 text-[13px] font-medium text-bg transition-opacity hover:opacity-85"
            >
              launch a colony
            </button>
            <button
              type="button"
              onClick={() => onOpenSettings("setup")}
              className="cursor-pointer rounded-md border border-border px-3 py-2 text-[13px] transition-colors hover:border-border-strong"
            >
              settings
            </button>
          </>
        ) : (
          session && (
            <>
              <button
                type="button"
                onClick={() => onOpenColony(session.id)}
                className="flex-1 cursor-pointer rounded-md bg-text px-3 py-2 text-[13px] font-medium text-bg transition-opacity hover:opacity-85"
              >
                open colony
              </button>
              {isLive(session.status) && (
                <button
                  type="button"
                  onClick={() => onStop(session.id)}
                  title="stop the microvm; the worktree is kept"
                  className="cursor-pointer rounded-md border border-border px-3 py-2 text-[13px] transition-colors text-muted hover:border-border-strong hover:text-text"
                >
                  stop
                </button>
              )}
              {/* The same condition the colony view's Resume button uses, so the two cannot disagree
                  about whether a colony can be picked back up. */}
              {!isLive(session.status) && !session.cleaned_up && (session.status === "stopped" || session.status === "failed") && (
                <button
                  type="button"
                  onClick={() => onResume(session.id)}
                  className="cursor-pointer rounded-md border border-border px-3 py-2 text-[13px] transition-colors hover:border-border-strong"
                >
                  resume
                </button>
              )}
              {session.pr_url && (
                <a
                  href={session.pr_url}
                  target="_blank"
                  rel="noreferrer"
                  className="cursor-pointer rounded-md border border-border px-3 py-2 text-[13px] transition-colors hover:border-border-strong font-medium no-underline"
                >
                  pr ↗
                </a>
              )}
            </>
          )
        )}
      </div>
    </aside>
  );
}
