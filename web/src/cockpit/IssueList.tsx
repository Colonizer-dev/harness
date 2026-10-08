// The issues list of the Colonize panel (issues #1218, #1228): an issue is a row of the Spotlight
// panel (a checkbox tile, the title, a plain one-line summary, the colony's state pill with a live dot
// while it runs, the age), with one-click actions that show on hover, focus or selection. The status
// and label filters and the bulk actions are chips above and below the list. Everything here is
// presentational: Colonize.tsx owns the data and what the actions do.
import type { ReactElement, ReactNode } from "react";

import { IconExternal } from "../components/icons";
import { cx } from "../components/ui";
import { epicMarker } from "../features/repos/claims";
import { relative } from "./InboxView";
import {
  STATUS_LABEL,
  STATUS_ORDER,
  issuePriority,
  plainSummary,
  type IssueFilters,
  type IssuePriority,
  type IssueState,
  type IssueStatus,
  type RepoIssue,
} from "./issuesList";
import type { PanelRow } from "./spotlight/Panel";

/** The coloured pill for where an issue stands; a live dot pulses while the colony runs. */
export function StatePill({ state }: { state: IssueState }): ReactElement | null {
  if (state.status === "new") return null;
  const tone =
    state.status === "colonizing" ? "bg-accent-soft text-accent" : state.status === "pr_open" ? "bg-ok-soft text-ok" : state.status === "blocked" ? "bg-warn-soft text-warn" : "bg-info-soft text-muted";
  return (
    <span className={cx("inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full border-0 px-2 py-0.5 text-meta-lg font-medium", tone)}>
      {state.live ? <span aria-hidden="true" className="size-1.5 rounded-full bg-current pulse-soft" /> : <span aria-hidden="true" className="size-1.5 rounded-full bg-current opacity-60" />}
      {state.status === "colonizing" && state.queued ? "Queued" : STATUS_LABEL[state.status]}
    </span>
  );
}

const PRIORITY_LABEL: Record<IssuePriority | "any", string> = { any: "Any priority", high: "High", normal: "Normal", low: "Low" };

/** Status, priority and label chips: what narrows the list. */
export function FilterChips({
  filters,
  onFilters,
  counts,
  labels,
}: {
  filters: IssueFilters;
  onFilters: (patch: Partial<IssueFilters>) => void;
  counts: Record<IssueStatus, number>;
  labels: readonly [string, number][];
}): ReactElement {
  const on = new Set(filters.labels);
  return (
    <div className="space-y-2 px-4 pb-2.5 sm:px-5">
      <div role="group" aria-label="filter by status" className="scroll-thin -mx-1 flex items-center gap-1.5 overflow-x-auto px-1 pb-0.5 [&>*]:shrink-0">
        <button type="button" className="spot-chip" aria-pressed={filters.statuses.length === 0} onClick={() => onFilters({ statuses: [] })}>
          All
        </button>
        {STATUS_ORDER.map((s) => {
          const active = filters.statuses.includes(s);
          return (
            <button key={s} type="button" className="spot-chip" aria-pressed={active} onClick={() => onFilters({ statuses: active ? filters.statuses.filter((x) => x !== s) : [...filters.statuses, s] })}>
              {STATUS_LABEL[s]} <span className="tabular-nums text-faint">{counts[s]}</span>
            </button>
          );
        })}
        <select
          aria-label="filter by priority"
          value={filters.priority}
          onChange={(e) => onFilters({ priority: e.target.value as IssueFilters["priority"] })}
          className="spot-chip ml-auto shrink-0 appearance-none bg-transparent pr-3 outline-none"
        >
          {(Object.keys(PRIORITY_LABEL) as (IssuePriority | "any")[]).map((p) => (
            <option key={p} value={p}>
              {PRIORITY_LABEL[p]}
            </option>
          ))}
        </select>
      </div>
      {labels.length > 0 && (
        <div role="group" aria-label="filter by label" className="scroll-thin -mx-1 flex gap-1.5 max-sm:hidden overflow-x-auto px-1 pb-0.5">
          {labels.slice(0, 24).map(([name, n]) => (
            <button key={name} type="button" className="spot-chip !h-6 !text-meta-lg" aria-pressed={on.has(name)} onClick={() => onFilters({ labels: on.has(name) ? filters.labels.filter((l) => l !== name) : [...filters.labels, name] })}>
              {name} <span className="tabular-nums text-faint">{n}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** The bar under the list: the selection, and what to do with it. */
export function BulkBar({
  shown,
  selected,
  allOn,
  pickable,
  busy,
  hidden,
  showHidden,
  frontable,
  onToggleAll,
  onFront,
  onHide,
  onClear,
  onShowHidden,
  loading,
}: {
  shown: number;
  selected: number;
  allOn: boolean;
  pickable: number;
  busy: boolean;
  hidden: number;
  showHidden: boolean;
  /** Selected issues whose colony is queued, so "front" has something to move. */
  frontable: number;
  onToggleAll: () => void;
  onFront: () => void;
  onHide: () => void;
  onClear: () => void;
  onShowHidden: () => void;
  loading: ReactNode;
}): ReactElement {
  return (
    <div className="flex flex-wrap items-center gap-1.5 border-t border-border px-4 py-2 text-small text-muted sm:px-5" role="toolbar" aria-label="bulk actions">
      <button type="button" disabled={pickable === 0} onClick={onToggleAll} className="spot-chip !h-6">
        {allOn ? "Select none" : `Select all ${pickable}`}
      </button>
      <span className="tabular-nums">
        {shown} shown · {selected} selected
      </span>
      {selected > 0 && (
        <>
          <button type="button" disabled={busy || frontable === 0} onClick={onFront} title="Move the selected issues' queued colonies to the front of the queue" className="spot-chip !h-6">
            Move to front{frontable > 0 ? ` · ${frontable}` : ""}
          </button>
          <button type="button" onClick={onHide} className="spot-chip !h-6">
            Hide
          </button>
          <button type="button" onClick={onClear} className="cursor-pointer border-0 bg-transparent p-0 text-small text-muted underline underline-offset-2 hover:text-text">
            Clear
          </button>
        </>
      )}
      <span className="ml-auto inline-flex items-center gap-3">
        {loading}
        {hidden > 0 && (
          <button type="button" onClick={onShowHidden} aria-pressed={showHidden} className="cursor-pointer border-0 bg-transparent p-0 text-small text-muted underline underline-offset-2 hover:text-text">
            {showHidden ? "Hide hidden" : `Show ${hidden} hidden`}
          </button>
        )}
      </span>
    </div>
  );
}

export interface RowActions {
  onColonize: (issue: RepoIssue) => void;
  onFront: (colonyId: string) => void;
  onHide: (issue: RepoIssue) => void;
  onUnhide: (issue: RepoIssue) => void;
  onOpenColony: (id: string) => void;
}

const ACTION =
  "inline-flex h-7 cursor-pointer items-center gap-1 whitespace-nowrap rounded-lg border-0 bg-transparent px-2 text-small text-muted transition-colors hover:bg-panel-3 hover:text-text focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-40";

/** The checkbox that leads an issue row: the pick, drawn as Spotlight draws a row's tile. */
function PickTile({ id, checked, disabled, title, onToggle, label }: { id: string; checked: boolean; disabled: boolean; title?: string; onToggle: () => void; label: string }): ReactElement {
  return (
    <span
      className="spot-tile grid size-8 shrink-0 place-items-center rounded-[10px] bg-panel-3"
      onClick={(e) => e.stopPropagation()}
    >
      <input id={id} type="checkbox" aria-label={label} disabled={disabled} checked={checked} title={title} onChange={onToggle} className="size-4 cursor-pointer accent-[var(--accent)] disabled:cursor-not-allowed" />
    </span>
  );
}

/** One issue as a panel row. Enter colonizes it (or opens the colony that holds it); the actions are on the row. */
export function issueRow({
  issue,
  state,
  checked,
  disabled,
  fresh,
  hiddenRow,
  result,
  busy,
  actions,
  onToggle,
  onPick,
  id,
}: {
  issue: RepoIssue;
  state: IssueState;
  checked: boolean;
  disabled: boolean;
  fresh: boolean;
  hiddenRow: boolean;
  /** What the panel just did with it, drawn in place of the pill. */
  result: ReactNode;
  busy: boolean;
  actions: RowActions;
  onToggle: () => void;
  /** Enter on the row. */
  onPick: () => void;
  id: string;
}): PanelRow {
  const epic = epicMarker(issue);
  const priority = issuePriority(issue);
  const canColonize = !disabled && state.status !== "colonizing" && state.status !== "blocked" && state.status !== "pr_open" && !epic;
  const opensColony = !canColonize && state.colony !== null && (state.status === "blocked" || state.status === "colonizing" || state.status === "pr_open");
  return {
    id,
    ariaLabel: `#${issue.number} ${issue.title}`,
    title: (
      <span className="flex items-center gap-1.5">
        {priority === "high" && <span title="High priority" aria-label="high priority" className="size-1.5 shrink-0 rounded-full bg-err" />}
        <span className="min-w-0 truncate">{issue.title}</span>
      </span>
    ),
    subtitle: (
      <span className="flex min-w-0 items-center gap-2">
        <span className="shrink-0 font-mono">#{issue.number}</span>
        {epic && <span className="shrink-0 text-warn">{epic}</span>}
        <span className="min-w-0 truncate">{plainSummary(issue)}</span>
        {issue.labels.slice(0, 2).map((l) => (
          <span key={l.name} className="hidden shrink-0 rounded-full border border-current/25 px-1.5 text-meta-lg leading-4 sm:inline">
            {l.name}
          </span>
        ))}
      </span>
    ),
    leading: <PickTile id={`${id}-pick`} checked={checked} disabled={disabled || epic !== null} label={`select #${issue.number}`} title={epic ? "An epic is a planning container: hand off its sub-issues instead" : undefined} onToggle={onToggle} />,
    trailing: (
      <span className="flex items-center gap-2">
        {result ?? <StatePill state={state} />}
        {state.prUrl && (
          <a href={state.prUrl} target="_blank" rel="noreferrer" className="inline-flex items-center gap-0.5 text-ok no-underline hover:underline" onClick={(e) => e.stopPropagation()}>
            PR <IconExternal size={10} />
          </a>
        )}
        <span className="w-8 text-right text-meta-lg tabular-nums">{fresh ? "new" : relative(issue.updatedAt)}</span>
      </span>
    ),
    actions: (
      <>
        {canColonize && (
          <button type="button" disabled={busy} onClick={() => actions.onColonize(issue)} className={cx(ACTION, "font-medium text-accent")} aria-label={`colonize #${issue.number}`}>
            Colonize
          </button>
        )}
        {state.queued && state.colony && (
          <button type="button" disabled={busy} onClick={() => actions.onFront(state.colony!.id)} className={ACTION} aria-label={`move #${issue.number} to front`}>
            Front
          </button>
        )}
        {hiddenRow ? (
          <button type="button" onClick={() => actions.onUnhide(issue)} className={ACTION}>
            Unhide
          </button>
        ) : (
          <button type="button" onClick={() => actions.onHide(issue)} className={ACTION} aria-label={`hide #${issue.number}`}>
            Skip
          </button>
        )}
        <a href={issue.url} target="_blank" rel="noreferrer" className={cx(ACTION, "no-underline")} aria-label={`open #${issue.number} on GitHub`}>
          <IconExternal size={12} />
        </a>
      </>
    ),
    clickSelects: true,
    verb: opensColony ? (state.status === "blocked" ? "answer" : "open colony") : "colonize",
    disabled: !canColonize && !opensColony,
    onPick: opensColony ? onPick : () => actions.onColonize(issue),
  };
}
