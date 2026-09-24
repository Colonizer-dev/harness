// History: one timeline of what the colonies did and what people did in the app, newest first,
// grouped by day. Built in history.ts from the mothership's activity log (GET /api/activity) plus
// the colony list for outcomes older than the log. Quiet runs ("12 colonies finished with nothing
// to change") fold into one row; the list pages fifty rows at a time and loads older activity on
// request.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement, type ReactNode } from "react";

import { errorMessage, useApi } from "../context";
import { cx } from "../components/ui";
import {
  IconAlert,
  IconChevron,
  IconExternal,
  IconGitPR,
  IconKey,
  IconMap,
  IconMemory,
  IconMerge,
  IconMinus,
  IconOrg,
  IconQuestion,
  IconRepeat,
  IconSettings,
  IconSpark,
  IconStop,
  IconTrash,
  IconX,
} from "../components/icons";
import type { ActivityEntry, Session } from "../types";
import { FilterSelect, Pagination, SearchBox, Segmented, optionsBy } from "./ListControls";
import { pageOf, usePagedFilter } from "./paging";
import {
  ACTOR_LABEL,
  HISTORY_ALL,
  KIND_FILTERS,
  buildTimeline,
  byDay,
  clock,
  collapse,
  groupRepos,
  groupSpan,
  groupText,
  matchesHistory,
  summarize,
  type HistoryItem,
  type HistoryKindFilter,
  type HistoryRow,
  type HistoryTone,
} from "./history";

/** Rows per page on History: more than the ten other lists show, since a row here is one line. */
export const HISTORY_PAGE_SIZE = 50;
/** How many log lines one request asks for. */
const FETCH_LIMIT = 500;
/** How often the newest page is re-read while History is open. */
const REFRESH_MS = 15_000;

const TONE_COLOR: Record<HistoryTone, string> = {
  pr: "var(--out-pr)",
  merged: "var(--out-merged)",
  closed: "var(--muted)",
  failed: "var(--err)",
  question: "var(--warn)",
  stopped: "var(--faint)",
  no_changes: "var(--faint)",
  launch: "var(--accent)",
  action: "var(--muted)",
};

function ToneIcon({ tone, kind, section }: { tone: HistoryTone; kind: string; section?: string | null }): ReactElement {
  const size = 14;
  const glyph: Record<HistoryTone, ReactNode> = {
    pr: <IconGitPR size={size} />,
    merged: <IconMerge size={size} />,
    closed: <IconX size={size} />,
    failed: <IconAlert size={size} />,
    question: <IconQuestion size={size} />,
    stopped: <IconStop size={size} />,
    no_changes: <IconMinus size={size} />,
    launch: <IconSpark size={size} />,
    action: <IconSettings size={size} />,
  };
  const color = TONE_COLOR[tone];
  // A person's own changes share one colour; the glyph says what kind of thing changed.
  const own = tone === "action" ? actionGlyph(kind, section ?? null, size) : null;
  return (
    <span
      aria-hidden="true"
      data-kind={kind}
      className="relative z-[1] inline-flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-lg border border-border"
      style={{ color, background: `color-mix(in oklab, ${color} 13%, var(--bg))` }}
    >
      {own ?? glyph[tone]}
    </span>
  );
}

function actionGlyph(kind: string, section: string | null, size: number): ReactNode {
  if (section === "secrets" || section === "connections") return <IconKey size={size} />;
  if (kind.startsWith("loop.")) return <IconRepeat size={size} />;
  if (kind.startsWith("workspace.")) return <IconOrg size={size} />;
  if (kind.startsWith("memory.")) return <IconMemory size={size} />;
  if (kind === "map.create") return <IconMap size={size} />;
  if (kind === "colony.delete" || kind === "colony.cleanup" || kind === "settings.remove") return <IconTrash size={size} />;
  return null;
}

/** Where a row leads: its colony when the cockpit still has it, else the place that shows its target. */
export type HistoryTarget = { kind: "colony"; id: string } | { kind: "section"; section: string } | null;

export function targetOf(item: HistoryItem, colonyIds: ReadonlySet<string>): HistoryTarget {
  if (item.colonyId && colonyIds.has(item.colonyId)) return { kind: "colony", id: item.colonyId };
  if (item.section) return { kind: "section", section: item.section };
  return null;
}

function ActorChip({ item }: { item: HistoryItem }): ReactElement | null {
  if (item.actor === "colony") return null;
  return (
    <span
      title={item.actor === "api" ? "Done with the API token (the CLI or a script)" : "Done in the cockpit"}
      className="inline-flex items-center rounded-full border border-border px-1.5 py-px text-[11px] leading-4 text-muted"
    >
      {item.actor === "api" ? "API" : "you"}
    </span>
  );
}

function PrLink({ url }: { url: string }): ReactElement {
  const number = url.match(/\/pull\/(\d+)/)?.[1];
  return (
    <a
      href={url}
      target="_blank"
      rel="noreferrer"
      onClick={(e) => e.stopPropagation()}
      className="inline-flex items-center gap-1 rounded-md border border-border px-1.5 py-px font-mono text-[11px] leading-4 text-muted no-underline hover:border-border-strong hover:text-text"
    >
      {number ? `#${number}` : "PR"}
      <IconExternal size={10} />
    </a>
  );
}

function ItemRow({ item, target, onOpen, nested = false }: { item: HistoryItem; target: HistoryTarget; onOpen: (t: HistoryTarget) => void; nested?: boolean }): ReactElement {
  const second = [item.title, item.colonyId ? item.colonyId.slice(0, 8) : null].filter(Boolean).join(" · ");
  const body = (
    <>
      <span className={cx("block truncate", nested ? "text-[13px]" : "text-[13.5px]")}>{item.text}</span>
      {(second || item.detail) && (
        <span className="mt-0.5 block truncate text-[12px] text-faint">
          {second && <span>{second}</span>}
          {item.detail && <span className={cx(second && "before:content-['_·_']", item.tone === "failed" ? "text-err" : "text-muted")}>{item.detail}</span>}
        </span>
      )}
    </>
  );
  return (
    <div
      className={cx(
        "group relative grid items-center gap-3 rounded-lg py-2 pr-2",
        nested ? "grid-cols-[52px_26px_minmax(0,1fr)_auto] pl-0" : "grid-cols-[52px_26px_minmax(0,1fr)_auto]",
        target && "hover:bg-panel-2",
      )}
    >
      <span
        className="text-right font-mono text-[11.5px] tabular-nums text-faint"
        title={item.approximate ? "Approximate: this colony finished before the activity log existed, so the time is its last update" : new Date(item.at).toLocaleString()}
      >
        {item.approximate ? "~" : ""}
        {clock(item.at)}
      </span>
      <ToneIcon tone={item.tone} kind={item.kind} section={item.section} />
      {target ? (
        <button type="button" onClick={() => onOpen(target)} className="min-w-0 cursor-pointer border-0 bg-transparent p-0 text-left text-text after:absolute after:inset-0 after:content-['']">
          {body}
        </button>
      ) : (
        <span className="min-w-0">{body}</span>
      )}
      <span className="relative flex items-center gap-1.5">
        {item.repo && !nested && <span className="hidden max-w-[220px] truncate font-mono text-[11px] text-faint sm:inline">{item.repo}</span>}
        <ActorChip item={item} />
        {item.prUrl && <PrLink url={item.prUrl} />}
      </span>
    </div>
  );
}

function GroupRow({ row, open, onToggle, targetFor, onOpen }: { row: Extract<HistoryRow, { type: "group" }>; open: boolean; onToggle: () => void; targetFor: (item: HistoryItem) => HistoryTarget; onOpen: (t: HistoryTarget) => void }): ReactElement {
  return (
    <div>
      <div className="relative grid grid-cols-[52px_26px_minmax(0,1fr)_auto] items-center gap-3 rounded-lg py-2 pr-2 hover:bg-panel-2">
        <span className="text-right font-mono text-[11.5px] tabular-nums text-faint">{clock(row.items[0].at)}</span>
        <span className="relative">
          <ToneIcon tone={row.tone} kind={row.kind} />
          <span className="absolute -right-1.5 -top-1.5 z-[2] min-w-4 rounded-full bg-panel-3 px-1 text-center text-[10px] font-semibold leading-4 text-text tabular-nums">{row.items.length}</span>
        </span>
        <button
          type="button"
          aria-expanded={open}
          onClick={onToggle}
          className="min-w-0 cursor-pointer border-0 bg-transparent p-0 text-left text-text after:absolute after:inset-0 after:content-['']"
        >
          <span className="flex items-center gap-1.5 text-[13.5px]">
            <IconChevron size={12} className={cx("shrink-0 text-faint transition-transform", open && "rotate-90")} />
            <span className="truncate">{groupText(row)}</span>
          </span>
          <span className="mt-0.5 block truncate pl-[18px] text-[12px] text-faint">
            {groupSpan(row.items)} · {groupRepos(row.items)}
          </span>
        </button>
        <span className="relative flex items-center gap-1.5">
          <ActorChip item={row.items[0]} />
        </span>
      </div>
      {open && (
        <div className="relative mb-1 ml-[78px] border-l border-border pl-3">
          {row.items.map((item) => (
            <ItemRow key={item.key} item={item} target={targetFor(item)} onOpen={onOpen} nested />
          ))}
        </div>
      )}
    </div>
  );
}

function Stat({ label, value, sub, tone, active, onClick }: { label: string; value: number; sub: string; tone: string; active: boolean; onClick: () => void }): ReactElement {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={cx(
        "flex min-w-0 cursor-pointer flex-col gap-1 border-0 px-5 pb-3.5 pt-4 text-left shadow-[-1px_0_0_var(--border),0_-1px_0_var(--border)]",
        active ? "bg-panel-2" : "bg-transparent hover:bg-panel-2",
      )}
    >
      <span className="flex items-center gap-1.5 truncate text-[12.5px] text-muted">
        <span aria-hidden="true" className="h-2 w-2 rounded-[2px]" style={{ background: tone }} />
        {label}
      </span>
      <span className="text-[24px] font-semibold tabular-nums tracking-[-0.03em] text-text">{value}</span>
      <span className="truncate text-[12px] text-faint">{sub}</span>
    </button>
  );
}

interface LogState {
  entries: ActivityEntry[];
  /** The cursor for the next older page; null once the log was read to its start. */
  next: number | null;
  loaded: boolean;
  /** Set when the mothership would not answer (an older build without the log): the page falls back to the colony list. */
  error: string | null;
  skipped: number;
  loadingOlder: boolean;
  /** Whether "Load older" has run: a refresh then keeps the older cursor instead of the first page's. */
  olderLoaded: boolean;
}

const EMPTY_LOG: LogState = { entries: [], next: null, loaded: false, error: null, skipped: 0, loadingOlder: false, olderLoaded: false };

/** Newest first, one line per `seq`, whichever page brought it. */
export function mergeEntries(a: readonly ActivityEntry[], b: readonly ActivityEntry[]): ActivityEntry[] {
  const bySeq = new Map<number, ActivityEntry>();
  for (const e of [...a, ...b]) bySeq.set(e.seq, e);
  return [...bySeq.values()].sort((x, y) => y.seq - x.seq);
}

export function HistoryView({
  sessions,
  org,
  onOpenColony,
  onOpenSection,
}: {
  sessions: Session[];
  org: string | null;
  onOpenColony: (id: string) => void;
  /** Opens where a row's target lives: a settings section, `secrets`, `loops`, `memory`, `redteam`. */
  onOpenSection?: (section: string) => void;
}): ReactElement {
  const api = useApi();
  const [log, setLog] = useState<LogState>(EMPTY_LOG);
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const orgRef = useRef(org);
  orgRef.current = org;

  const refresh = useCallback(async () => {
    const asked = org;
    try {
      const page = await api.activity({ limit: FETCH_LIMIT, org: asked ?? undefined });
      if (orgRef.current !== asked) return;
      // A refresh re-reads the newest page; pages already loaded further back stay, with their cursor.
      setLog((prev) => ({
        ...prev,
        entries: mergeEntries(page.entries, prev.entries),
        next: prev.olderLoaded ? prev.next : page.next_before,
        loaded: true,
        error: null,
        skipped: prev.olderLoaded ? prev.skipped : page.skipped,
      }));
    } catch (e) {
      if (orgRef.current !== asked) return;
      setLog((prev) => ({ ...prev, loaded: true, error: errorMessage(e) }));
    }
  }, [api, org]);

  useEffect(() => {
    setLog(EMPTY_LOG);
    void refresh();
    const timer = window.setInterval(() => void refresh(), REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const loadOlder = useCallback(async () => {
    if (log.next == null) return;
    setLog((prev) => ({ ...prev, loadingOlder: true }));
    try {
      const page = await api.activity({ limit: FETCH_LIMIT, before: log.next, org: org ?? undefined });
      setLog((prev) => ({
        ...prev,
        entries: mergeEntries(prev.entries, page.entries),
        next: page.next_before,
        olderLoaded: true,
        loadingOlder: false,
        skipped: prev.skipped + page.skipped,
      }));
    } catch (e) {
      setLog((prev) => ({ ...prev, loadingOlder: false, error: errorMessage(e) }));
    }
  }, [api, log.next, org]);

  const complete = log.error != null || (log.loaded && log.next == null);
  const items = useMemo(() => buildTimeline(log.error ? [] : log.entries, sessions, complete), [log.entries, log.error, sessions, complete]);
  const list = usePagedFilter(items, { filters: HISTORY_ALL, match: matchesHistory, pageSize: HISTORY_PAGE_SIZE });
  const now = new Date();
  const rows = useMemo(() => collapse(list.matched, new Date()), [list.matched]);
  const view = pageOf(rows, list.page, HISTORY_PAGE_SIZE);
  const days = byDay(view.rows, now);
  const stats = summarize(list.matched);
  const colonyIds = useMemo(() => new Set(sessions.map((s) => s.id)), [sessions]);
  const waitingNow = sessions.filter((s) => s.status === "waiting_for_answer").length;

  const openTarget = (target: HistoryTarget) => {
    if (!target) return;
    if (target.kind === "colony") onOpenColony(target.id);
    else onOpenSection?.(target.section);
  };
  const targetFor = (item: HistoryItem): HistoryTarget => {
    const target = targetOf(item, colonyIds);
    return target?.kind === "section" && !onOpenSection ? null : target;
  };
  const setKind = (kind: HistoryKindFilter) => list.setFilters({ kind: list.filters.kind === kind && kind !== "all" ? "all" : kind });
  const toggle = (key: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const filtered = list.query.trim() !== "" || list.filters.kind !== "all" || list.filters.repo !== "all" || list.filters.actor !== "all";
  const oldest = items.length > 0 ? items[items.length - 1].at : null;
  const approximate = items.some((i) => i.approximate);

  return (
    <main className="cockpit min-h-0 overflow-y-auto px-6 pb-20 pt-10">
      <div className="mx-auto flex w-full max-w-[1080px] flex-col gap-7">
        <div className="flex flex-wrap items-end justify-between gap-4">
          <div className="min-w-0">
            <h1 className="m-0 text-[30px] font-semibold leading-[1.15] tracking-[-0.035em]">History</h1>
            <div className="mt-2 text-[14px] text-muted">
              {org ?? "All workspaces"} · what your colonies did and what you changed
              {oldest && <span className="text-faint"> · since {new Date(oldest).toLocaleDateString(undefined, { day: "numeric", month: "short" })}</span>}
            </div>
          </div>
        </div>

        <div className="overflow-hidden border-y border-border">
          <div className="grid [grid-template-columns:repeat(auto-fit,minmax(150px,1fr))]">
            <Stat label="Pull requests opened" value={stats.prs} sub="colonies that returned work" tone="var(--out-pr)" active={false} onClick={() => list.setFilters({ kind: "outcomes" })} />
            <Stat label="Merged" value={stats.merged} sub="pull requests merged" tone="var(--out-merged)" active={false} onClick={() => list.setFilters({ kind: "outcomes" })} />
            <Stat label="Failed" value={stats.failed} sub="colonies that failed" tone="var(--err)" active={list.filters.kind === "failures"} onClick={() => setKind("failures")} />
            <Stat
              label="Questions"
              value={stats.questions}
              sub={waitingNow > 0 ? `${waitingNow} waiting on you now` : "none waiting now"}
              tone="var(--warn)"
              active={list.filters.kind === "questions"}
              onClick={() => setKind("questions")}
            />
            <Stat label="Your actions" value={stats.yours} sub="launches, stops, settings" tone="var(--accent)" active={list.filters.kind === "yours"} onClick={() => setKind("yours")} />
          </div>
        </div>

        <div className="flex flex-col gap-3">
          <div className="flex flex-wrap items-center gap-2">
            <Segmented label="kind" value={list.filters.kind} onChange={(kind) => list.setFilters({ kind })} options={KIND_FILTERS} />
            <div className="ml-auto flex flex-wrap items-center gap-2">
              <SearchBox value={list.query} onChange={list.setQuery} placeholder="Search history…" label="search history" className="w-full sm:w-56" />
              <FilterSelect label="repository" allLabel="All repositories" value={list.filters.repo} onChange={(repo) => list.setFilters({ repo })} options={optionsBy(items, (i) => i.repo, (r) => r.split("/")[1] ?? r)} />
              <FilterSelect label="actor" allLabel="Anyone" value={list.filters.actor} onChange={(actor) => list.setFilters({ actor })} options={optionsBy(items, (i) => i.actor, (a) => ACTOR_LABEL[a as keyof typeof ACTOR_LABEL] ?? a)} />
            </div>
          </div>
          {(filtered || log.error || log.skipped > 0 || approximate) && (
            <div role="status" className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[12.5px] text-faint">
              {filtered && (
                <span>
                  showing {list.total} of {items.length}
                  <button type="button" onClick={list.reset} className="ml-2 cursor-pointer border-0 bg-transparent p-0 text-muted underline underline-offset-[3px] hover:text-text">
                    clear ×
                  </button>
                </span>
              )}
              {log.error && <span className="text-warn">The activity log did not load ({log.error}); showing colony outcomes only.</span>}
              {log.skipped > 0 && <span className="text-warn">{log.skipped} unreadable {log.skipped === 1 ? "line" : "lines"} in the activity log skipped.</span>}
              {approximate && !filtered && <span>~ marks a time read from a colony that finished before the activity log existed.</span>}
            </div>
          )}
        </div>

        {!log.loaded ? (
          <div className="border-y border-border py-3.5 text-[13px] text-muted">Loading history…</div>
        ) : view.total === 0 ? (
          <div className="border-y border-border py-6 text-center text-[13px] text-muted">
            {filtered ? "Nothing matches these filters." : "Nothing has happened here yet. Launch a colony and its story starts here."}
          </div>
        ) : (
          <div className="flex flex-col">
            {days.map(({ day, rows: dayRows }) => (
              <section key={day} aria-label={day}>
                <div className="sticky top-0 z-10 -mx-2 flex items-baseline gap-2 bg-bg/90 px-2 pb-2 pt-3 backdrop-blur-sm">
                  <h2 className="m-0 text-[12px] font-semibold uppercase tracking-[0.08em] text-muted">{day}</h2>
                  <span className="text-[12px] text-faint">
                    {dayRows.reduce((n, r) => n + (r.type === "group" ? r.items.length : 1), 0)} events
                  </span>
                  <span aria-hidden="true" className="ml-2 h-px flex-1 self-center bg-border" />
                </div>
                <div className="relative pb-2">
                  <div aria-hidden="true" className="absolute bottom-3 left-[77px] top-3 w-px bg-border" />
                  {dayRows.map((row) =>
                    row.type === "group" ? (
                      <GroupRow key={row.key} row={row} open={open.has(row.key)} onToggle={() => toggle(row.key)} targetFor={targetFor} onOpen={openTarget} />
                    ) : (
                      <ItemRow key={row.key} item={row.item} target={targetFor(row.item)} onOpen={openTarget} />
                    ),
                  )}
                </div>
              </section>
            ))}
            <div className="mt-2 flex flex-wrap items-center gap-3 border-t border-border pt-2.5">
              <Pagination view={view} onPage={list.setPage} noun={view.total === 1 ? "row" : "rows"} className="flex-1" />
              {log.next != null && !log.error && (
                <button
                  type="button"
                  disabled={log.loadingOlder}
                  onClick={() => void loadOlder()}
                  className="cursor-pointer rounded-md border border-border bg-transparent px-2.5 py-1 text-[12.5px] text-muted hover:border-border-strong hover:text-text disabled:cursor-default disabled:opacity-50"
                >
                  {log.loadingOlder ? "Loading…" : "Load older activity"}
                </button>
              )}
            </div>
          </div>
        )}
      </div>
    </main>
  );
}
