// The controls a paged list draws (see paging.ts): a search box, a filter menu, a segmented toggle
// in the style of the dashboard's Repositories | Packages switch, and the pager under the rows.
import type { ReactElement, ReactNode } from "react";
import { cx } from "../components/ui";
import { pageNumbers, type PageView } from "./paging";

export function SearchBox({ value, onChange, placeholder, label, className }: { value: string; onChange: (v: string) => void; placeholder: string; label: string; className?: string }): ReactElement {
  return (
    <label className={cx("relative flex min-w-0 items-center", className ?? "w-full sm:w-60")}>
      <span className="sr-only">{label}</span>
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true" className="pointer-events-none absolute left-2 text-faint">
        <circle cx="11" cy="11" r="6.5" />
        <path d="m16 16 4 4" />
      </svg>
      <input
        type="text"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        aria-label={label}
        className="w-full min-w-0 rounded-md border border-border bg-transparent py-1 pl-6 pr-2 text-[12.5px] text-text outline-none placeholder:text-faint focus:border-border-strong"
      />
    </label>
  );
}

export interface FilterOption {
  value: string;
  label: string;
  count?: number;
}

/** A compact select whose first option ("all") clears the filter. Hidden when there is nothing to choose between. */
export function FilterSelect({ value, onChange, label, allLabel, options }: { value: string; onChange: (v: string) => void; label: string; allLabel: string; options: FilterOption[] }): ReactElement | null {
  if (options.length < 2 && value === "all") return null;
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      aria-label={label}
      className={cx("max-w-[14rem] rounded-md border bg-panel px-2 py-1 text-[12.5px]", value === "all" ? "border-border text-muted" : "border-accent text-text")}
    >
      <option value="all">{allLabel}</option>
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
          {o.count != null ? ` (${o.count})` : ""}
        </option>
      ))}
    </select>
  );
}

/** The options for a filter over `items`, one per distinct key, most common first. */
export function optionsBy<T>(items: readonly T[], key: (item: T) => string | readonly string[] | null | undefined, label: (k: string) => string = (k) => k): FilterOption[] {
  const counts = new Map<string, number>();
  for (const it of items) {
    const k = key(it);
    const keys = k == null ? [] : typeof k === "string" ? [k] : [...new Set(k)];
    for (const one of keys) counts.set(one, (counts.get(one) ?? 0) + 1);
  }
  return [...counts]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([value, count]) => ({ value, label: label(value), count }));
}

/** A two-or-more-way toggle, drawn like the dashboard's Repositories | Packages switch. */
export function Segmented<T extends string>({ value, onChange, options, label }: { value: T; onChange: (v: T) => void; options: { value: T; label: ReactNode; title?: string }[]; label: string }): ReactElement {
  return (
    <div role="group" aria-label={label} className="flex rounded-lg border border-border p-0.5">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          aria-pressed={value === o.value}
          title={o.title}
          onClick={() => onChange(o.value)}
          className={cx("inline-flex cursor-pointer items-center gap-1.5 rounded-md border-0 px-3 py-1 text-[12.5px]", value === o.value ? "bg-panel-3 text-text" : "bg-transparent text-muted hover:text-text")}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** "11–20 of 54" with previous / page numbers / next. Draws only the count when everything fits on one page. */
export function Pagination({ view, onPage, noun = "rows", className }: { view: PageView<unknown>; onPage: (page: number) => void; noun?: string; className?: string }): ReactElement | null {
  if (view.total === 0) return null;
  const btn = "inline-flex min-w-7 cursor-pointer items-center justify-center rounded-md border-0 px-2 py-1 text-[12.5px] tabular-nums disabled:cursor-default disabled:opacity-40";
  return (
    <nav aria-label="pages" className={cx("flex flex-wrap items-center gap-x-3 gap-y-2 text-[12.5px] text-muted", className ?? "mt-2")}>
      <span className="tabular-nums text-faint">
        {view.pageCount > 1 ? `${view.from}–${view.to} of ${view.total}` : `${view.total}`} {noun}
      </span>
      {view.pageCount > 1 && (
        <div className="ml-auto flex items-center gap-0.5">
          <button type="button" aria-label="previous page" disabled={view.page === 0} onClick={() => onPage(view.page - 1)} className={cx(btn, "bg-transparent text-muted enabled:hover:text-text")}>
            ‹ Prev
          </button>
          {pageNumbers(view.page, view.pageCount).map((p, i) =>
            p === null ? (
              <span key={`gap${i}`} aria-hidden="true" className="px-1 text-faint">
                …
              </span>
            ) : (
              <button
                key={p}
                type="button"
                aria-label={`page ${p + 1}`}
                aria-current={p === view.page ? "page" : undefined}
                onClick={() => onPage(p)}
                className={cx(btn, p === view.page ? "bg-panel-3 text-text" : "bg-transparent text-muted hover:text-text")}
              >
                {p + 1}
              </button>
            ),
          )}
          <button
            type="button"
            aria-label="next page"
            disabled={view.page >= view.pageCount - 1}
            onClick={() => onPage(view.page + 1)}
            className={cx(btn, "bg-transparent text-muted enabled:hover:text-text")}
          >
            Next ›
          </button>
        </div>
      )}
    </nav>
  );
}
