// Transcript search (issue #739): find a phrase across every colony conversation and open the exact
// turn it came from. GET /api/history/search answers at most fifty hits and 400s on an empty query,
// so the form refuses a blank search rather than round-tripping a failure. Snippets are drawn as
// plain text — never HTML — and each row is a button that hands its colony and turn to the caller.
import { useState, type FormEvent, type ReactElement } from "react";

import { errorMessage, useApi } from "../context";
import { Badge, SESSION_STATUS, Spinner, cx, inputClass } from "../components/ui";
import type { HistoryHit, HistorySearchQuery, SessionStatus } from "../types";
import { SearchBox } from "./ListControls";

/** The statuses a searched colony can carry, in the mothership's own order (web/src/types.ts). */
export const TRANSCRIPT_STATUSES = Object.keys(SESSION_STATUS) as SessionStatus[];

/** The search form's fields; only `q` is required. */
export interface TranscriptFilters {
  q: string;
  repo: string;
  org: string;
  agent: string;
  status: string;
  since: string;
  until: string;
}

export const EMPTY_TRANSCRIPT_FILTERS: TranscriptFilters = { q: "", repo: "", org: "", agent: "", status: "", since: "", until: "" };

/**
 * The query the mothership takes from the form: `q` trimmed, every other field dropped when blank.
 * The limit is deliberately left to the mothership's own cap (fifty hits).
 */
export function searchQuery(filters: TranscriptFilters): HistorySearchQuery {
  return {
    q: filters.q.trim(),
    repo: filters.repo.trim() || undefined,
    org: filters.org.trim() || undefined,
    agent: filters.agent.trim() || undefined,
    status: filters.status || undefined,
    since: filters.since || undefined,
    until: filters.until || undefined,
  };
}

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

export function TranscriptSearch({ org, onOpen, initial = null }: {
  /** The workspace in view, seeded into the org filter; the searcher can still widen it. */
  org?: string | null;
  onOpen: (colony: string, turn: string | null) => void;
  /** Hits for tests, which run no effects; the live form fetches. */
  initial?: HistoryHit[] | null;
}): ReactElement {
  const api = useApi();
  const [filters, setFilters] = useState<TranscriptFilters>(() => ({ ...EMPTY_TRANSCRIPT_FILTERS, org: org ?? "" }));
  const [hits, setHits] = useState<HistoryHit[] | null>(initial);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const set = (key: keyof TranscriptFilters) => (e: { target: { value: string } }) => setFilters((f) => ({ ...f, [key]: e.target.value }));

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const query = searchQuery(filters);
    if (!query.q) return;
    setBusy(true);
    setError(null);
    try {
      const answer = await api.historySearch(query);
      setHits(answer.hits);
    } catch (err) {
      setError(errorMessage(err));
      setHits(null);
    } finally {
      setBusy(false);
    }
  };

  const field = cx(inputClass, "w-auto text-small-lg");

  return (
    <section aria-label="Search colony transcripts" className="flex flex-col gap-3">
      <div className="min-w-0">
        <h2 className="m-0 text-lead font-semibold">Search transcripts</h2>
        <p className="mt-0.5 text-small-lg text-muted">Find a phrase in what you and your colonies said, and open the turn it came from.</p>
      </div>
      <form onSubmit={submit} className="flex flex-wrap items-center gap-2" role="search">
        <SearchBox value={filters.q} onChange={(q) => setFilters((f) => ({ ...f, q }))} placeholder="Search conversations…" label="search colony transcripts" className="w-full sm:w-64" />
        <input aria-label="Repository" placeholder="owner/repo" value={filters.repo} onChange={set("repo")} className={field} />
        <input aria-label="Workspace" placeholder="org" value={filters.org} onChange={set("org")} className={field} />
        <input aria-label="Agent" placeholder="agent" value={filters.agent} onChange={set("agent")} className={field} />
        <select aria-label="Status" value={filters.status} onChange={set("status")} className={field}>
          <option value="">Any status</option>
          {TRANSCRIPT_STATUSES.map((s) => (
            <option key={s} value={s}>{SESSION_STATUS[s].label}</option>
          ))}
        </select>
        <input aria-label="Since" type="date" value={filters.since} onChange={set("since")} className={field} />
        <input aria-label="Until" type="date" value={filters.until} onChange={set("until")} className={field} />
        <button
          type="submit"
          disabled={busy || filters.q.trim() === ""}
          className="inline-flex cursor-pointer items-center gap-1.5 rounded-lg border border-border bg-panel px-3 py-1.5 text-small-lg text-text hover:border-border-strong disabled:cursor-default disabled:opacity-50"
        >
          {busy && <Spinner className="size-3" />}
          Search
        </button>
      </form>

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
