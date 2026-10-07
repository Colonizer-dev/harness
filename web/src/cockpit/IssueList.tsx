// The issues list of the Colonize pane (issue #1218): grouped by repository, each row a plain
// one-line summary, the colony's state pill (with a live dot while it runs), the pull request and the
// age, and one-click actions that appear on hover or focus. The filters and the bulk bar sit above.
// Everything here is presentational: Colonize.tsx owns the data and what the actions do.
import { useState, type ReactElement, type ReactNode } from "react";

import { Avatar } from "../components/Avatar";
import { IconChevronDown, IconExternal, IconSearch } from "../components/icons";
import { cx } from "../components/ui";
import { SearchBox } from "./ListControls";
import { relative } from "./InboxView";
import { epicMarker } from "../features/repos/claims";
import {
  STATUS_LABEL,
  STATUS_ORDER,
  issuePriority,
  plainSummary,
  type IssueFilters,
  type IssuePriority,
  type IssueState,
  type IssueStatus,
  type RepoGroup,
  type RepoIssue,
} from "./issuesList";

/** The coloured pill for where an issue stands; a live dot pulses while the colony runs. */
export function StatePill({ state, onOpen }: { state: IssueState; onOpen?: (id: string) => void }): ReactElement | null {
  if (state.status === "new") return null;
  const tone =
    state.status === "colonizing" ? "bg-accent-soft text-accent" : state.status === "pr_open" ? "bg-ok-soft text-ok" : state.status === "blocked" ? "bg-warn-soft text-warn" : "bg-info-soft text-muted";
  const body = (
    <>
      {state.live ? <span aria-hidden="true" className="size-1.5 rounded-full bg-current pulse-soft" /> : <span aria-hidden="true" className="size-1.5 rounded-full bg-current opacity-60" />}
      {state.status === "colonizing" && state.queued ? "Queued" : STATUS_LABEL[state.status]}
    </>
  );
  const cls = cx("inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full border-0 px-2 py-0.5 text-meta-lg font-medium", tone);
  return state.colony && onOpen ? (
    <button type="button" onClick={() => onOpen(state.colony!.id)} title="Open the colony" className={cx(cls, "cursor-pointer hover:brightness-95")}>
      {body}
    </button>
  ) : (
    <span className={cls}>{body}</span>
  );
}

const PRIORITY_LABEL: Record<IssuePriority | "any", string> = { any: "Any priority", high: "High", normal: "Normal", low: "Low" };

/** Search, status chips, priority and label chips: what narrows the list. */
export function IssueFilterBar({
  query,
  onQuery,
  filters,
  onFilters,
  counts,
  labels,
}: {
  query: string;
  onQuery: (q: string) => void;
  filters: IssueFilters;
  onFilters: (patch: Partial<IssueFilters>) => void;
  counts: Record<IssueStatus, number>;
  labels: readonly [string, number][];
}): ReactElement {
  const on = new Set(filters.labels);
  const chip = (active: boolean) =>
    cx(
      "inline-flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-full border px-2.5 py-1 text-meta-lg transition-colors",
      active ? "border-accent bg-accent-soft text-accent" : "border-border bg-transparent text-muted hover:border-border-strong hover:text-text",
    );
  return (
    <div className="shrink-0 space-y-2 px-4 pt-3">
      <div className="flex items-center gap-2">
        <SearchBox value={query} onChange={onQuery} placeholder="Search title, #number or repo…" label="search issues" className="min-w-0 flex-1" />
        <select
          aria-label="filter by priority"
          value={filters.priority}
          onChange={(e) => onFilters({ priority: e.target.value as IssueFilters["priority"] })}
          className="h-8 shrink-0 cursor-pointer rounded-lg border border-border bg-panel-2 px-2 text-small-lg text-muted outline-none focus:border-border-strong"
        >
          {(Object.keys(PRIORITY_LABEL) as (IssuePriority | "any")[]).map((p) => (
            <option key={p} value={p}>
              {PRIORITY_LABEL[p]}
            </option>
          ))}
        </select>
      </div>
      <div role="group" aria-label="filter by status" className="scroll-thin -mx-1 flex gap-1.5 overflow-x-auto px-1 pb-0.5">
        <button type="button" aria-pressed={filters.statuses.length === 0} onClick={() => onFilters({ statuses: [] })} className={chip(filters.statuses.length === 0)}>
          All
        </button>
        {STATUS_ORDER.map((s) => {
          const active = filters.statuses.includes(s);
          return (
            <button
              key={s}
              type="button"
              aria-pressed={active}
              onClick={() => onFilters({ statuses: active ? filters.statuses.filter((x) => x !== s) : [...filters.statuses, s] })}
              className={chip(active)}
            >
              {STATUS_LABEL[s]} <span className="tabular-nums text-faint">{counts[s]}</span>
            </button>
          );
        })}
      </div>
      {labels.length > 0 && (
        <div role="group" aria-label="filter by label" className="flex max-h-16 shrink-0 flex-wrap gap-1 overflow-y-auto">
          {labels.slice(0, 24).map(([name, n]) => (
            <button
              key={name}
              type="button"
              aria-pressed={on.has(name)}
              onClick={() => onFilters({ labels: on.has(name) ? filters.labels.filter((l) => l !== name) : [...filters.labels, name] })}
              className={cx("cursor-pointer rounded-full border px-2 py-0.5 text-meta-lg", on.has(name) ? "border-accent bg-accent-soft text-accent" : "border-border bg-transparent text-muted hover:text-text")}
            >
              {name} <span className="tabular-nums text-faint">{n}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** The bar above the rows: the selection, and what to do with it. */
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
  const small = "h-7 cursor-pointer rounded-md border border-border bg-transparent px-2 text-small text-text transition-colors hover:bg-panel-2 disabled:cursor-not-allowed disabled:opacity-50";
  return (
    <div className="flex shrink-0 flex-wrap items-center gap-2 px-4 py-2 text-small text-muted" role="toolbar" aria-label="bulk actions">
      <button type="button" disabled={pickable === 0} onClick={onToggleAll} className={small}>
        {allOn ? "Select none" : `Select all ${pickable}`}
      </button>
      <span className="tabular-nums">
        {shown} shown · {selected} selected
      </span>
      {selected > 0 && (
        <>
          <button type="button" disabled={busy || frontable === 0} onClick={onFront} title="Move the selected issues' queued colonies to the front of the queue" className={small}>
            Move to front{frontable > 0 ? ` · ${frontable}` : ""}
          </button>
          <button type="button" onClick={onHide} className={small}>
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

/** One issue. The actions fade in on hover and focus; on a touch screen they are always there. */
export function IssueRow({
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
  id,
}: {
  issue: RepoIssue;
  state: IssueState;
  checked: boolean;
  disabled: boolean;
  fresh: boolean;
  hiddenRow: boolean;
  /** What this pane just did with it, drawn in place of the pill. */
  result: ReactNode;
  busy: boolean;
  actions: RowActions;
  onToggle: () => void;
  id: string;
}): ReactElement {
  const epic = epicMarker(issue);
  const priority = issuePriority(issue);
  const canColonize = !disabled && state.status !== "colonizing" && state.status !== "blocked" && state.status !== "pr_open" && !epic;
  const action = "inline-flex h-7 cursor-pointer items-center gap-1 whitespace-nowrap rounded-md border-0 bg-transparent px-2 text-small text-muted transition-colors hover:bg-panel-3 hover:text-text focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-40";
  return (
    <li
      data-new={fresh || undefined}
      data-status={state.status}
      className={cx(
        "group/row relative flex items-start gap-3 rounded-xl px-2.5 py-2.5 transition-colors hover:bg-panel-2 focus-within:bg-panel-2",
        (disabled || epic) && !canColonize && state.status === "new" && "opacity-60",
        hiddenRow && "opacity-50",
        fresh && "bg-accent-soft/60",
      )}
    >
      <input
        id={id}
        type="checkbox"
        disabled={disabled}
        checked={checked}
        title={epic ? "An epic is a planning container: hand off its sub-issues instead" : undefined}
        onChange={onToggle}
        className="mt-1 size-4 shrink-0 cursor-pointer accent-[var(--accent)] disabled:cursor-not-allowed"
      />
      <div className="min-w-0 flex-1">
        <div className="flex items-start gap-2">
          <label htmlFor={id} className="min-w-0 flex-1 cursor-pointer">
            <span className="flex items-center gap-1.5 text-body-sm font-medium leading-snug text-text">
              {priority === "high" && <span title="High priority" aria-label="high priority" className="size-1.5 shrink-0 rounded-full bg-err" />}
              <span className="min-w-0 truncate max-sm:whitespace-normal">{issue.title}</span>
            </span>
            <span className="mt-0.5 block truncate text-small-lg text-muted">{plainSummary(issue)}</span>
          </label>
          <span className="flex shrink-0 items-center gap-2 pt-0.5">
            {result ?? <StatePill state={state} onOpen={actions.onOpenColony} />}
            <span className="w-7 text-right text-meta-lg tabular-nums text-faint">{fresh ? "new" : relative(issue.updatedAt)}</span>
          </span>
        </div>
        <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-meta-lg text-faint">
          <span className="font-mono">#{issue.number}</span>
          {epic && <span className="text-warn">{epic}</span>}
          {state.prUrl && (
            <a href={state.prUrl} target="_blank" rel="noreferrer" className="inline-flex items-center gap-0.5 text-ok no-underline hover:underline">
              PR <IconExternal size={10} />
            </a>
          )}
          {issue.labels.slice(0, 3).map((l) => (
            <span key={l.name} className="rounded-full border border-border px-1.5 leading-4 text-muted">
              {l.name}
            </span>
          ))}
          {issue.author && <span>@{issue.author.login}</span>}
          <span className="ml-auto flex items-center gap-0.5 opacity-100 transition-opacity sm:opacity-0 sm:group-hover/row:opacity-100 sm:group-focus-within/row:opacity-100 [@media(hover:none)]:opacity-100">
            {canColonize && (
              <button type="button" disabled={busy} onClick={() => actions.onColonize(issue)} className={cx(action, "font-medium text-accent hover:text-accent")} aria-label={`colonize #${issue.number}`}>
                Colonize
              </button>
            )}
            {state.queued && state.colony && (
              <button type="button" disabled={busy} onClick={() => actions.onFront(state.colony!.id)} className={action} aria-label={`move #${issue.number} to front`}>
                Move to front
              </button>
            )}
            {state.status === "blocked" && state.colony && (
              <button type="button" onClick={() => actions.onOpenColony(state.colony!.id)} className={cx(action, "text-warn hover:text-warn")}>
                Answer
              </button>
            )}
            {hiddenRow ? (
              <button type="button" onClick={() => actions.onUnhide(issue)} className={action}>
                Unhide
              </button>
            ) : (
              <button type="button" onClick={() => actions.onHide(issue)} className={action} aria-label={`hide #${issue.number}`}>
                Skip
              </button>
            )}
            <a href={issue.url} target="_blank" rel="noreferrer" className={cx(action, "no-underline")} aria-label={`open #${issue.number} on GitHub`}>
              GitHub <IconExternal size={11} />
            </a>
          </span>
        </div>
      </div>
    </li>
  );
}

/** A repository's header and its rows; the header folds them away. */
export function RepoSection({
  group,
  avatar,
  children,
  total,
}: {
  group: RepoGroup<RepoIssue>;
  avatar: string | null;
  children: ReactNode;
  /** All of the repository's issues under the current filters, not just this page's. */
  total: number;
}): ReactElement {
  const [open, setOpen] = useState(true);
  const [owner, name] = group.repo.split("/");
  return (
    <li className="list-none" aria-label={group.repo}>
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="sticky top-0 z-[1] flex w-full cursor-pointer items-center gap-2 border-0 bg-panel/90 px-2.5 py-1.5 text-left backdrop-blur-sm"
      >
        <Avatar name={owner} src={avatar ?? undefined} size={18} rounded="md" />
        <span className="min-w-0 truncate text-small-lg font-semibold text-text">
          <span className="font-normal text-muted">{owner}/</span>
          {name}
        </span>
        <span className="rounded-full bg-panel-3 px-1.5 text-meta-lg tabular-nums text-muted">{total}</span>
        <IconChevronDown size={13} className={cx("ml-auto text-faint transition-transform", !open && "-rotate-90")} />
      </button>
      {open && <ul className="m-0 list-none space-y-0.5 p-0 pb-1">{children}</ul>}
    </li>
  );
}

/** The message in place of the list: nothing to show, or a failure to say. */
export function EmptyList({ title, body, action }: { title: string; body: string; action?: ReactNode }): ReactElement {
  return (
    <li className="list-none px-6 py-10 text-center">
      <span aria-hidden="true" className="mx-auto mb-3 grid size-10 place-items-center rounded-full bg-panel-2 text-faint">
        <IconSearch size={18} />
      </span>
      <p className="m-0 text-body-sm font-medium text-text">{title}</p>
      <p className="m-0 mt-1 text-small-lg text-muted">{body}</p>
      {action && <div className="mt-3">{action}</div>}
    </li>
  );
}
