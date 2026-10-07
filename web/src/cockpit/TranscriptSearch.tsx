// Transcript search (issue #739): find a phrase across every colony conversation and open the exact
// turn it came from. GET /api/history/search answers at most fifty hits and 400s on an empty query,
// so the form refuses a blank search rather than round-tripping a failure. Snippets are drawn as
// plain text — never HTML — and each row is a button that hands its colony and turn to the caller.
import { useEffect, useMemo, useRef, useState, type ReactElement, type ReactNode } from "react";

import { errorMessage, useApi } from "../context";
import { Badge, SESSION_STATUS, Spinner, cx, inputClass } from "../components/ui";
import type { HistoryHit, Session, SessionStatus } from "../types";
import { SearchBox } from "./ListControls";
import {
  EMPTY_TRANSCRIPT_FILTERS,
  activeFilters,
  clearFilters,
  datePreset,
  distinctValues,
  matchValues,
  parseTranscriptFilters,
  searchQuery,
  serializeTranscriptFilters,
  type TranscriptFilters,
} from "./transcriptFilters";

/** The statuses a searched colony can carry, in the mothership's own order (web/src/types.ts). */
export const TRANSCRIPT_STATUSES = Object.keys(SESSION_STATUS) as SessionStatus[];

export { EMPTY_TRANSCRIPT_FILTERS, searchQuery, type TranscriptFilters } from "./transcriptFilters";

/** Open a hit at its colony and turn through the caller's callback — the row's click handler, pinned by its test. */
export function openHit(hit: Pick<HistoryHit, "colony" | "turn">, open: (colony: string, turn: string | null) => void): void {
  open(hit.colony, hit.turn ?? null);
}

function HitRow({ hit, onOpen }: { hit: HistoryHit; onOpen: (colony: string, turn: string | null) => void }): ReactElement {
  const status = SESSION_STATUS[hit.status as SessionStatus];
  return (
    <li>
      <button
        type="button"
        onClick={() => openHit(hit, onOpen)}
        className="block w-full cursor-pointer border-0 bg-transparent px-3.5 py-2.5 text-left hover:bg-panel-2"
      >
        <span className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-meta-lg text-faint">
          <span className="font-mono text-muted">{hit.repo}</span>
          <span aria-hidden="true">·</span>
          <span>{hit.agent}</span>
          <span aria-hidden="true">·</span>
          {status ? <Badge tone={status.tone}>{status.label}</Badge> : <span>{hit.status}</span>}
          <span aria-hidden="true">·</span>
          <time dateTime={hit.ts} title={new Date(hit.ts).toLocaleString()}>{hit.ts}</time>
          <span className="ml-auto rounded-full border border-border px-1.5 py-px text-meta leading-4 text-muted">{hit.role === "user" ? "You" : "Agent"}</span>
        </span>
        <span className="mt-1 block text-body-sm text-text [overflow-wrap:anywhere]">{hit.snippet}</span>
      </button>
    </li>
  );
}

/** Debounce between the last keystroke or filter change and the request. */
const SEARCH_DEBOUNCE_MS = 300;

const chipClass = (active: boolean) =>
  cx(
    "inline-flex shrink-0 cursor-pointer items-center gap-1 rounded-full border bg-transparent px-2.5 py-1 text-small-lg text-text hover:border-border-strong",
    active ? "border-accent" : "border-border",
  );

/** One filter chip and its popover: a dropdown beside the row, a bottom sheet on phone widths. */
function FilterChip({ label, active, children }: { label: string; active: boolean; children: (close: () => void) => ReactNode }): ReactElement {
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const close = () => {
    setOpen(false);
    button.current?.focus();
  };
  return (
    <div className="relative shrink-0" onKeyDown={(e) => { if (e.key === "Escape" && open) { e.stopPropagation(); close(); } }}>
      <button ref={button} type="button" aria-haspopup="dialog" aria-expanded={open} onClick={() => setOpen((o) => !o)} className={chipClass(active)}>
        {label}
        <span aria-hidden="true" className="text-meta text-faint">▾</span>
      </button>
      {open && (
        <>
          <div aria-hidden="true" className="fixed inset-0 z-40 bg-black/30 sm:bg-transparent" onClick={close} />
          <div
            role="dialog"
            aria-label={`${label} filter`}
            className="fixed inset-x-0 bottom-0 z-50 flex max-h-[70vh] animate-[ck-in_160ms_ease-out_both] flex-col gap-2 overflow-y-auto rounded-t-2xl border border-border-strong bg-panel p-3 pb-[calc(0.75rem+env(safe-area-inset-bottom))] shadow-[0_16px_48px_rgb(0_0_0/0.4)] sm:absolute sm:inset-x-auto sm:bottom-auto sm:left-0 sm:top-full sm:mt-1 sm:w-64 sm:rounded-xl sm:pb-3"
          >
            {children(close)}
          </div>
        </>
      )}
    </div>
  );
}

const optionClass = (selected: boolean) =>
  cx("w-full cursor-pointer rounded-md border-0 bg-transparent px-2 py-1 text-left text-small-lg text-text hover:bg-panel-2", selected && "bg-panel-2 font-semibold");

/** A searchable list of the values that occur; picking one applies it and closes the popover. */
function ValueList({ label, values, selected, onPick }: { label: string; values: string[]; selected: string; onPick: (v: string) => void }): ReactElement {
  const [needle, setNeedle] = useState("");
  const shown = matchValues(values, needle);
  return (
    <>
      <input autoFocus aria-label={`Find ${label.toLowerCase()}`} placeholder={`Find ${label.toLowerCase()}…`} value={needle} onChange={(e) => setNeedle(e.target.value)} className={cx(inputClass, "w-full text-small-lg")} />
      <ul className="m-0 flex max-h-56 list-none flex-col gap-0.5 overflow-y-auto p-0">
        {shown.map((v) => (
          <li key={v}>
            <button type="button" aria-pressed={v === selected} onClick={() => onPick(v === selected ? "" : v)} className={cx(optionClass(v === selected), "font-mono [overflow-wrap:anywhere]")}>{v}</button>
          </li>
        ))}
        {shown.length === 0 && <li className="px-2 py-1 text-small-lg text-muted">{values.length === 0 ? `No ${label.toLowerCase()} yet.` : "No match."}</li>}
      </ul>
    </>
  );
}

function readUrl(): TranscriptFilters {
  return typeof window === "undefined" ? EMPTY_TRANSCRIPT_FILTERS : parseTranscriptFilters(window.location.search, TRANSCRIPT_STATUSES);
}

export function TranscriptSearch({ org, sessions = [], onOpen, initial = null }: {
  /** The workspace in view; every search is scoped to it (there is no org field). */
  org?: string | null;
  /** The colonies in view: the repositories and agents that occur in them fill the pickers. */
  sessions?: Pick<Session, "repo" | "agent">[];
  onOpen: (colony: string, turn: string | null) => void;
  /** Hits for tests, which run no effects; the live form fetches. */
  initial?: HistoryHit[] | null;
}): ReactElement {
  const api = useApi();
  const [filters, setFilters] = useState<TranscriptFilters>(readUrl);
  const [hits, setHits] = useState<HistoryHit[] | null>(initial);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const patch = (p: Partial<TranscriptFilters>) => setFilters((f) => ({ ...f, ...p }));
  const repos = useMemo(() => distinctValues(sessions, (s) => s.repo), [sessions]);
  const agents = useMemo(() => distinctValues(sessions, (s) => s.agent), [sessions]);
  const active = activeFilters(filters, (s) => SESSION_STATUS[s as SessionStatus]?.label ?? s);

  // Keep the filters in the address bar, replacing the entry so Back does not step through keystrokes.
  useEffect(() => {
    const search = serializeTranscriptFilters(filters, window.location.search);
    if (search !== window.location.search) window.history.replaceState(null, "", window.location.pathname + search + window.location.hash);
  }, [filters]);

  // Search as you type or filter, once the typing pauses. The mothership 400s on a blank query, so
  // a blank box shows no results rather than a failure.
  useEffect(() => {
    const query = searchQuery(filters, org);
    if (!query.q) {
      setHits(null);
      setError(null);
      setBusy(false);
      return;
    }
    let stale = false;
    setBusy(true);
    const timer = setTimeout(async () => {
      try {
        const answer = await api.historySearch(query);
        if (stale) return;
        setHits(answer.hits);
        setError(null);
      } catch (err) {
        if (stale) return;
        setError(errorMessage(err));
        setHits(null);
      } finally {
        if (!stale) setBusy(false);
      }
    }, SEARCH_DEBOUNCE_MS);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  }, [api, filters, org]);

  const dateInput = "w-full text-small-lg";

  return (
    <section aria-label="Search colony transcripts" className="flex flex-col gap-3">
      <div className="min-w-0">
        <h2 className="m-0 text-lead font-semibold">Search transcripts</h2>
        <p className="mt-0.5 text-small-lg text-muted">Find a phrase in what you and your colonies said, and open the turn it came from.</p>
      </div>
      <div role="search" className="flex flex-col gap-2">
        <div className="flex items-center gap-2">
          <SearchBox value={filters.q} onChange={(q) => patch({ q })} placeholder="Search conversations…" label="search colony transcripts" className="w-full sm:w-64" />
          {busy && <Spinner className="size-3" />}
        </div>
        <div role="group" aria-label="Filters" className="-mx-1 flex flex-nowrap items-center gap-2 overflow-x-auto px-1 pb-1 sm:flex-wrap sm:overflow-visible sm:pb-0">
          <FilterChip label="Repository" active={!!filters.repo}>
            {(close) => <ValueList label="Repositories" values={repos} selected={filters.repo} onPick={(repo) => { patch({ repo }); close(); }} />}
          </FilterChip>
          <FilterChip label="Agent" active={!!filters.agent}>
            {(close) => <ValueList label="Agents" values={agents} selected={filters.agent} onPick={(agent) => { patch({ agent }); close(); }} />}
          </FilterChip>
          <FilterChip label="Status" active={!!filters.status}>
            {() => (
              <div className="flex flex-wrap gap-1.5">
                {TRANSCRIPT_STATUSES.map((s) => (
                  <button key={s} type="button" aria-pressed={filters.status === s} onClick={() => patch({ status: filters.status === s ? "" : s })} className={cx(chipClass(filters.status === s), "py-0.5")}>
                    {SESSION_STATUS[s].label}
                  </button>
                ))}
              </div>
            )}
          </FilterChip>
          <FilterChip label="Date" active={!!(filters.since || filters.until)}>
            {(close) => (
              <>
                <div className="flex flex-wrap gap-1.5">
                  {([["today", "Today"], ["7d", "Last 7 days"], ["30d", "Last 30 days"]] as const).map(([p, text]) => (
                    <button key={p} type="button" onClick={() => { patch(datePreset(p)); close(); }} className={cx(chipClass(false), "py-0.5")}>{text}</button>
                  ))}
                </div>
                <label className="flex flex-col gap-0.5 text-meta-lg text-muted">From
                  <input type="date" value={filters.since} max={filters.until || undefined} onChange={(e) => patch({ since: e.target.value })} className={cx(inputClass, dateInput)} />
                </label>
                <label className="flex flex-col gap-0.5 text-meta-lg text-muted">To
                  <input type="date" value={filters.until} min={filters.since || undefined} onChange={(e) => patch({ until: e.target.value })} className={cx(inputClass, dateInput)} />
                </label>
              </>
            )}
          </FilterChip>
        </div>
        {active.length > 0 && (
          <ul aria-label="Active filters" className="m-0 flex list-none flex-wrap items-center gap-1.5 p-0">
            {active.map((a) => (
              <li key={a.id}>
                <button type="button" aria-label={`Remove filter ${a.label}`} onClick={() => patch(a.clear)} className="inline-flex cursor-pointer items-center gap-1 rounded-full border border-border bg-panel-2 px-2 py-0.5 text-meta-lg text-text hover:border-border-strong">
                  {a.label}<span aria-hidden="true" className="text-faint">×</span>
                </button>
              </li>
            ))}
            <li>
              <button type="button" onClick={() => setFilters(clearFilters)} className="cursor-pointer border-0 bg-transparent px-1 text-meta-lg text-muted underline hover:text-text">Clear all</button>
            </li>
          </ul>
        )}
      </div>

      {error && <p role="status" className="text-small-lg text-warn">Couldn’t search transcripts: {error}</p>}
      {hits && hits.length === 0 && !error && (
        <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-muted">No turn matches this search.</p>
      )}
      {hits && hits.length > 0 && (
        <ul className="flex flex-col divide-y divide-border overflow-hidden rounded-xl border border-border">
          {hits.map((hit) => (
            <HitRow key={`${hit.colony}:${hit.seq}`} hit={hit} onOpen={onOpen} />
          ))}
        </ul>
      )}
    </section>
  );
}
