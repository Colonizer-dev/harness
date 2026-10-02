// Settings → Fleet → Fleet history (issue #762, docs/fleet.md): on the owner, the colonies its
// members finished and synced with history sync on — read from GET /api/fleet/history, filtered and
// paged there, with totals per member and repository at the top and a drawer for one colony's
// record and logs. The cockpit's fleet panel lists hosts, not colonies, so the history lives here.
import { useEffect, useState, type ReactElement } from "react";

import { errorMessage, useApi } from "../context";
import { formatBytes } from "../cockpit/host";
import { formatCost } from "../spend";
import type { FleetHistoryDetail, FleetHistoryEntry, FleetHistoryPage, FleetHistoryQuery, FleetHistoryTotals, SessionStatus } from "../types";
import { Badge, Button, SESSION_STATUS, Spinner, cx, inputClass, timeAgo } from "./ui";

/** The statuses a synced (finished) colony can carry, for the status filter. */
export const HISTORY_STATUSES: SessionStatus[] = ["merged", "pr_opened", "closed", "no_changes", "stopped", "failed"];

export type HistoryFilters = Pick<FleetHistoryQuery, "member" | "repo" | "status" | "since" | "until">;

/** "5 colonies · 3 merged · $3.50": a totals row; the cost only when a row carried one. */
export function totalsText(t: FleetHistoryTotals): string {
  const parts = [`${t.colonies} ${t.colonies === 1 ? "colony" : "colonies"}`, `${t.merged} merged`];
  if (t.cost_usd != null) parts.push(formatCost(t.cost_usd));
  return parts.join(" · ");
}

/** "finished on studio-2", with "(removed)" when the member has left the fleet. */
export function finishedOn(entry: Pick<FleetHistoryEntry, "member_name" | "member_removed">): string {
  return `finished on ${entry.member_name}${entry.member_removed ? " (removed)" : ""}`;
}

const selectClass = cx(inputClass, "w-auto");

/** The filter row: member, repository, status and the finish-date range. */
export function HistoryFilterBar({ page, filters, onChange }: { page: FleetHistoryPage | null; filters: HistoryFilters; onChange: (next: HistoryFilters) => void }): ReactElement {
  const set = (key: keyof HistoryFilters) => (e: { target: { value: string } }) => onChange({ ...filters, [key]: e.target.value || undefined });
  return (
    <div className="flex flex-wrap items-center gap-2" role="group" aria-label="Filter fleet history">
      <select aria-label="Member" className={selectClass} value={filters.member ?? ""} onChange={set("member")}>
        <option value="">All members</option>
        {page?.members.map((m) => (
          <option key={m.id} value={m.id}>{m.name}{m.removed ? " (removed)" : ""}</option>
        ))}
      </select>
      <select aria-label="Repository" className={selectClass} value={filters.repo ?? ""} onChange={set("repo")}>
        <option value="">All repositories</option>
        {page?.repos.map((r) => <option key={r} value={r}>{r}</option>)}
      </select>
      <select aria-label="Status" className={selectClass} value={filters.status ?? ""} onChange={set("status")}>
        <option value="">Any status</option>
        {HISTORY_STATUSES.map((s) => <option key={s} value={s}>{SESSION_STATUS[s].label}</option>)}
      </select>
      <input aria-label="Finished from" type="date" className={selectClass} value={filters.since ?? ""} onChange={set("since")} />
      <input aria-label="Finished until" type="date" className={selectClass} value={filters.until ?? ""} onChange={set("until")} />
    </div>
  );
}

/** The totals at the top: everything filtered, then per member and per repository. */
export function HistoryStats({ stats }: { stats: FleetHistoryPage["stats"] }): ReactElement {
  return (
    <div className="space-y-1 rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px]">
      <p className="font-semibold">{totalsText(stats.total)}</p>
      {stats.members.map((m) => (
        <p key={m.member_id} className="text-muted">
          {m.name}{m.removed ? " (removed)" : ""}: {totalsText(m)}
        </p>
      ))}
      {stats.repos.map((r) => (
        <p key={r.repo} className="text-faint"><span className="font-mono">{r.repo}</span>: {totalsText(r)}</p>
      ))}
    </div>
  );
}

/** One synced colony in the list. */
export function HistoryRow({ entry, open, onOpen }: { entry: FleetHistoryEntry; open: boolean; onOpen: () => void }): ReactElement {
  const r = entry.record;
  const status = SESSION_STATUS[r.status];
  return (
    <button
      type="button"
      aria-expanded={open}
      onClick={onOpen}
      className={cx("block w-full border-b border-border px-3.5 py-2.5 text-left last:border-b-0 hover:bg-panel-2", open && "bg-panel-2")}
    >
      <div className="flex min-w-0 items-center gap-2">
        <span className="truncate text-[13px] font-medium">{r.issue_title || r.branch || r.original_id}</span>
        {status && <Badge tone={status.tone}>{status.label}</Badge>}
        {entry.member_removed && <Badge tone="warn">Member removed</Badge>}
      </div>
      <div className="truncate text-[11.5px] text-faint">
        <span className="font-mono">{r.repo}</span> · {finishedOn(entry)} · {timeAgo(r.updated_at)}
        {r.cost_usd != null && ` · ${formatCost(r.cost_usd)}`}
      </div>
    </button>
  );
}

/** The detail drawer: the record's fields and its logs, one opened at a time. */
export function HistoryDrawer({ detail, log, logName, logError, onLog, onClose }: {
  detail: FleetHistoryDetail;
  log: string | null;
  logName: string | null;
  logError?: string | null;
  onLog: (name: string) => void;
  onClose: () => void;
}): ReactElement {
  const r = detail.record;
  const fields: [string, string | null][] = [
    ["Member", `${detail.member_name}${detail.member_removed ? " (removed from the fleet)" : ""}`],
    ["Repository", r.repo],
    ["Issue", r.issue != null ? `#${r.issue}` : null],
    ["Branch", r.branch || null],
    ["Pull request", r.pr_url],
    ["Agent", r.agent || null],
    ["Cost", r.cost_usd != null ? formatCost(r.cost_usd) : null],
    ["Finished", new Date(r.updated_at).toLocaleString()],
    ["Synced", new Date(detail.received_at).toLocaleString()],
    ["Summary", r.summary],
    ["Error", r.error],
  ];
  return (
    <section aria-label="Fleet history entry" className="space-y-2 rounded-xl border border-border bg-panel-2 px-3.5 py-3">
      <div className="flex items-center justify-between gap-2">
        <p className="truncate text-[13px] font-semibold">{r.issue_title || r.original_id}</p>
        <Button size="sm" onClick={onClose}>Close</Button>
      </div>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-[12px]">
        {fields.filter(([, v]) => v).map(([k, v]) => (
          <div key={k} className="contents">
            <dt className="text-faint">{k}</dt>
            <dd className="min-w-0 break-words">{v}</dd>
          </div>
        ))}
      </dl>
      <div className="flex flex-wrap gap-2">
        {detail.logs.map((l) => (
          <Button key={l.name} size="sm" variant={logName === l.name ? "primary" : "secondary"} disabled={!l.stored} onClick={() => onLog(l.name)}
            title={l.omitted ? "too large to sync" : l.stored ? undefined : "not held on this owner"}>
            {l.name} · {formatBytes(l.bytes)}
          </Button>
        ))}
        {detail.logs.length === 0 && <p className="text-[11.5px] text-faint">No logs were synced for this colony.</p>}
      </div>
      {logError && <p className="text-[12px] text-warn">Couldn’t read the log: {logError}</p>}
      {logName && log != null && (
        <pre aria-label={`${logName} contents`} className="max-h-72 overflow-auto rounded-lg border border-border bg-panel px-2.5 py-2 font-mono text-[11px] whitespace-pre-wrap break-all">{log}</pre>
      )}
    </section>
  );
}

/** The whole tab: filters, totals, the list with its "Load more", and the drawer. */
export function FleetHistory({ initial, hideWhenEmpty = false }: {
  /** Pre-seeded page for tests, which run no effects; the live section fetches. */
  initial?: FleetHistoryPage | null;
  /** On a mothership that is no owner now: render nothing unless some history is stored. */
  hideWhenEmpty?: boolean;
}): ReactElement | null {
  const api = useApi();
  const [filters, setFilters] = useState<HistoryFilters>({});
  const [page, setPage] = useState<FleetHistoryPage | null>(initial ?? null);
  const [rows, setRows] = useState<FleetHistoryEntry[]>(initial?.colonies ?? []);
  const [error, setError] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [openKey, setOpenKey] = useState<string | null>(null);
  const [detail, setDetail] = useState<FleetHistoryDetail | null>(null);
  const [logName, setLogName] = useState<string | null>(null);
  const [log, setLog] = useState<string | null>(null);
  const [logError, setLogError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .fleetHistory(filters)
      .then((answer) => {
        if (cancelled) return;
        setPage(answer);
        setRows(answer.colonies);
        setError(null);
      })
      .catch((e) => !cancelled && setError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api, filters]);

  const more = async () => {
    if (!page?.next_cursor) return;
    setLoadingMore(true);
    try {
      const next = await api.fleetHistory({ ...filters, cursor: page.next_cursor });
      setPage(next);
      setRows((prev) => [...prev, ...next.colonies]);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setLoadingMore(false);
    }
  };

  const open = async (entry: FleetHistoryEntry) => {
    setLog(null);
    setLogName(null);
    setLogError(null);
    if (openKey === entry.key) {
      setOpenKey(null);
      setDetail(null);
      return;
    }
    setOpenKey(entry.key);
    setDetail(null);
    try {
      setDetail(await api.fleetHistoryEntry(entry.member_id, entry.id));
    } catch (e) {
      setLogError(errorMessage(e));
    }
  };

  const readLog = async (name: string) => {
    if (!detail) return;
    setLogName(name);
    setLog(null);
    setLogError(null);
    try {
      setLog(await api.fleetHistoryLog(detail.member_id, detail.id, name));
    } catch (e) {
      setLogError(errorMessage(e));
    }
  };

  if (hideWhenEmpty && (!page || page.members.length === 0)) return null;
  const filtered = Object.values(filters).some(Boolean);

  return (
    <div className="space-y-2 border-t border-border pt-4">
      <h4 className="text-[12.5px] font-semibold">Fleet history</h4>
      <p className="text-[11.5px] text-faint">
        Colonies your members finished and synced with history sync on{page ? `, kept ${page.retention_days} days after they arrive` : ""}. A removed member’s history stays.
      </p>
      <HistoryFilterBar page={page} filters={filters} onChange={setFilters} />
      {error && <p className="text-[12.5px] text-warn">Couldn’t read the fleet history: {error}</p>}
      {!page && !error && <p className="flex items-center gap-2 text-[12.5px] text-muted"><Spinner className="size-3" /> Loading…</p>}
      {page && <HistoryStats stats={page.stats} />}
      {page && rows.length === 0 && (
        <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px] text-muted">
          {filtered ? "No synced colony matches these filters." : "Nothing synced yet. A member sends its finished colonies once its operator turns history sync on."}
        </p>
      )}
      {rows.length > 0 && (
        <div className="overflow-hidden rounded-xl border border-border">
          {rows.map((entry) => (
            <div key={entry.key}>
              <HistoryRow entry={entry} open={openKey === entry.key} onOpen={() => void open(entry)} />
              {openKey === entry.key && (
                <div className="border-b border-border p-2.5 last:border-b-0">
                  {detail ? (
                    <HistoryDrawer detail={detail} log={log} logName={logName} logError={logError} onLog={(name) => void readLog(name)} onClose={() => void open(entry)} />
                  ) : logError ? (
                    <p className="text-[12px] text-warn">Couldn’t read this colony: {logError}</p>
                  ) : (
                    <p className="flex items-center gap-2 text-[12px] text-muted"><Spinner className="size-3" /> Loading…</p>
                  )}
                </div>
              )}
            </div>
          ))}
        </div>
      )}
      {page?.next_cursor && (
        <Button size="sm" disabled={loadingMore} onClick={() => void more()}>{loadingMore && <Spinner className="size-3" />}Load more</Button>
      )}
    </div>
  );
}
