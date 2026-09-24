// The Overview colonies table's header row, where each column title is also that column's filter:
// Colony — a search box; Org — a menu of workspaces with their logos; Status — a multi-select of the
// overview's buckets with counts; Updated — any / 1h / 24h / 7d; Spent — any / >$1 / >$5. A column
// with a filter set carries an accent dot, and "clear filters" resets them all.
import { useEffect, useRef, useState, type ReactElement, type ReactNode } from "react";
import { orgOf, sameOrg } from "../components/ui";
import { sessionCost } from "../spend";
import { taskLine } from "../summary";
import type { Session } from "../types";
import { COLONY_GRID, OrgTile } from "./DashChart";
import { OVERVIEW_FILTERS, matchesOverviewFilter, type OverviewFilter } from "./feed";

export type UpdatedWithin = "any" | "1h" | "24h" | "7d";
export type SpentOver = "any" | "1" | "5";

export interface ColonyFilters {
  query: string;
  org: string | null;
  statuses: ReadonlySet<OverviewFilter>;
  updated: UpdatedWithin;
  spent: SpentOver;
}

export const NO_FILTERS: ColonyFilters = { query: "", org: null, statuses: new Set(), updated: "any", spent: "any" };

const WITHIN_MS: Record<Exclude<UpdatedWithin, "any">, number> = { "1h": 3_600_000, "24h": 86_400_000, "7d": 7 * 86_400_000 };

/** Whether any filter is set. */
export function filtersActive(f: ColonyFilters): boolean {
  return f.query.trim() !== "" || f.org !== null || f.statuses.size > 0 || f.updated !== "any" || f.spent !== "any";
}

/** The colonies that pass every filter. A status set matches a colony in any of its buckets. Pure. */
export function applyColonyFilters(sessions: readonly Session[], f: ColonyFilters, nowMs: number): Session[] {
  const q = f.query.trim().toLowerCase();
  return sessions.filter((s) => {
    if (f.org !== null && !sameOrg(orgOf(s), f.org)) return false;
    if (f.statuses.size > 0 && ![...f.statuses].some((b) => matchesOverviewFilter(s, b))) return false;
    if (f.updated !== "any") {
      const at = Date.parse(s.last_activity_at ?? s.updated_at);
      if (!Number.isFinite(at) || nowMs - at > WITHIN_MS[f.updated]) return false;
    }
    if (f.spent !== "any" && (sessionCost(s) ?? 0) <= Number(f.spent)) return false;
    if (q) {
      const hay = [taskLine(s, ""), s.issue_title, s.repo, s.issue != null ? `#${s.issue}` : ""].join(" ").toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });
}

/** A header cell that opens a small menu below it. */
function HeaderMenu({ label, active, warn = false, align = "left", children }: { label: string; active: boolean; warn?: boolean; align?: "left" | "right"; children: (close: () => void) => ReactNode }): ReactElement {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);
  return (
    <div ref={box} className={`relative min-w-0 ${align === "right" ? "text-right" : ""}`}>
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className={`inline-flex cursor-pointer items-center gap-1 border-0 bg-transparent p-0 text-[12.5px] ${warn ? "text-warn" : active ? "text-text" : "text-muted"} hover:text-text`}
      >
        {label}
        {active && <span aria-label="filtered" className="size-1.5 rounded-full bg-accent" />}
        <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" aria-hidden="true">
          <path d="m6 9 6 6 6-6" />
        </svg>
      </button>
      {open && (
        <div
          role="menu"
          className={`absolute top-full z-30 mt-1.5 min-w-[180px] rounded-lg border border-border-strong bg-panel p-1 text-left shadow-[0_12px_32px_rgb(0_0_0/0.35)] ${align === "right" ? "right-0" : "left-0"}`}
        >
          {children(() => setOpen(false))}
        </div>
      )}
    </div>
  );
}

function Option({ selected, onClick, children }: { selected: boolean; onClick: () => void; children: ReactNode }): ReactElement {
  return (
    <button
      type="button"
      role="menuitemcheckbox"
      aria-checked={selected}
      onClick={onClick}
      className={`flex w-full cursor-pointer items-center gap-2 rounded-md border-0 px-2 py-1.5 text-left text-[12.5px] ${selected ? "bg-panel-2 text-text" : "bg-transparent text-muted hover:bg-panel-2 hover:text-text"}`}
    >
      <span aria-hidden="true" className="w-3 shrink-0 text-accent">{selected ? "✓" : ""}</span>
      {children}
    </button>
  );
}

export function ColonyFilterHeader({
  filters,
  onChange,
  sessions,
  workspaces,
  counts,
  stalledQueue,
}: {
  filters: ColonyFilters;
  onChange: (next: ColonyFilters) => void;
  /** The colonies before filtering, for the org list. */
  sessions: readonly Session[];
  workspaces: readonly { org: string; avatar: string | null }[];
  counts: Record<OverviewFilter, number>;
  stalledQueue: boolean;
}): ReactElement {
  const set = (patch: Partial<ColonyFilters>) => onChange({ ...filters, ...patch });
  const orgs = workspaces.length > 0 ? workspaces : [...new Set(sessions.map(orgOf))].map((org) => ({ org, avatar: null }));
  return (
    <div className={`${COLONY_GRID} py-2 text-[12.5px] text-muted`}>
      <span />
      <label className="flex min-w-0 items-center gap-1.5">
        <span className="sr-only">filter colonies by title or repository</span>
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true" className="shrink-0 text-faint">
          <circle cx="11" cy="11" r="6.5" />
          <path d="m16 16 4 4" />
        </svg>
        <input
          value={filters.query}
          onChange={(e) => set({ query: e.target.value })}
          placeholder="Colony"
          className="min-w-0 flex-1 border-0 bg-transparent p-0 text-[12.5px] text-text outline-none placeholder:text-muted"
        />
        {filters.query && <span aria-label="filtered" className="size-1.5 shrink-0 rounded-full bg-accent" />}
      </label>
      <HeaderMenu label={filters.org ?? "Org"} active={filters.org !== null}>
        {(close) => (
          <>
            <Option selected={filters.org === null} onClick={() => { set({ org: null }); close(); }}>
              All orgs
            </Option>
            {orgs.map((o) => (
              <Option key={o.org} selected={sameOrg(filters.org, o.org)} onClick={() => { set({ org: o.org }); close(); }}>
                <OrgTile org={o.org} avatar={o.avatar} size={16} />
                <span className="truncate">{o.org}</span>
              </Option>
            ))}
          </>
        )}
      </HeaderMenu>
      <HeaderMenu
        label={stalledQueue ? "Status · queue stalled" : "Status"}
        active={filters.statuses.size > 0}
        warn={stalledQueue}
      >
        {() => (
          <>
            {OVERVIEW_FILTERS.map((b) => {
              const on = filters.statuses.has(b);
              return (
                <Option
                  key={b}
                  selected={on}
                  onClick={() => {
                    const next = new Set(filters.statuses);
                    if (on) next.delete(b);
                    else next.add(b);
                    set({ statuses: next });
                  }}
                >
                  <span className={`flex-1 ${b === "need you" || (b === "queued" && stalledQueue) ? "text-warn" : ""}`}>
                    {b === "queued" && stalledQueue ? "queued · stalled" : b}
                  </span>
                  <span className="tabular-nums text-faint">{counts[b]}</span>
                </Option>
              );
            })}
          </>
        )}
      </HeaderMenu>
      <HeaderMenu label="Updated" active={filters.updated !== "any"} align="right">
        {(close) => (
          <>
            {(["any", "1h", "24h", "7d"] as const).map((u) => (
              <Option key={u} selected={filters.updated === u} onClick={() => { set({ updated: u }); close(); }}>
                {u === "any" ? "Any time" : `Within ${u}`}
              </Option>
            ))}
          </>
        )}
      </HeaderMenu>
      <HeaderMenu label="Spent" active={filters.spent !== "any"} align="right">
        {(close) => (
          <>
            {(["any", "1", "5"] as const).map((v) => (
              <Option key={v} selected={filters.spent === v} onClick={() => { set({ spent: v }); close(); }}>
                {v === "any" ? "Any amount" : `Over $${v}`}
              </Option>
            ))}
          </>
        )}
      </HeaderMenu>
    </div>
  );
}
