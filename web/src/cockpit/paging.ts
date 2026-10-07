// One long list, shown ten rows at a time: a search box, a set of filters and a page. Shared by the
// Packages tables and the colony lists so they page, search and filter the same way. The state is a
// plain reducer (tested without a DOM); the hook wraps it and does the filtering and slicing.
import { useMemo, useReducer } from "react";

/** Rows per page, everywhere a list pages. */
export const PAGE_SIZE = 10;

export interface PagedState<F> {
  query: string;
  filters: F;
  /** Zero-based. */
  page: number;
}

export type PagedAction<F> =
  | { type: "query"; query: string }
  | { type: "filters"; patch: Partial<F> }
  | { type: "page"; page: number }
  | { type: "reset"; filters: F };

/** Any change to what matches puts the list back on its first page; only paging moves the page. */
export function pagedReducer<F>(state: PagedState<F>, action: PagedAction<F>): PagedState<F> {
  switch (action.type) {
    case "query":
      return action.query === state.query ? state : { ...state, query: action.query, page: 0 };
    case "filters":
      return { ...state, filters: { ...state.filters, ...action.patch }, page: 0 };
    case "page":
      return { ...state, page: Math.max(0, Math.floor(action.page)) };
    case "reset":
      return { query: "", filters: action.filters, page: 0 };
  }
}

export interface PageView<T> {
  /** The rows on the current page. */
  rows: T[];
  /** How many rows match, across every page. */
  total: number;
  /** Zero-based, clamped to the last page when the list shrinks under it. */
  page: number;
  pageCount: number;
  /** One-based bounds of the page, for "11–20 of 54"; both 0 when nothing matches. */
  from: number;
  to: number;
}

/** One page of `items`. A page past the end (the list shrank on a poll) shows the last page. */
export function pageOf<T>(items: readonly T[], page: number, size: number = PAGE_SIZE): PageView<T> {
  const total = items.length;
  const pageCount = Math.max(1, Math.ceil(total / size));
  const at = Math.min(Math.max(0, page), pageCount - 1);
  const start = at * size;
  const rows = items.slice(start, start + size);
  return { rows, total, page: at, pageCount, from: total === 0 ? 0 : start + 1, to: start + rows.length };
}

/** The page numbers to draw: always the first and last, the current one and its neighbours, with
 *  null standing for a gap ("…"). Zero-based. */
export function pageNumbers(page: number, pageCount: number): (number | null)[] {
  if (pageCount <= 7) return Array.from({ length: pageCount }, (_, i) => i);
  const keep = new Set([0, pageCount - 1, page - 1, page, page + 1].filter((p) => p >= 0 && p < pageCount));
  if (page <= 3) for (let p = 0; p <= 4; p++) keep.add(p);
  if (page >= pageCount - 4) for (let p = pageCount - 5; p < pageCount; p++) keep.add(p);
  const sorted = [...keep].sort((a, b) => a - b);
  const out: (number | null)[] = [];
  sorted.forEach((p, i) => {
    if (i > 0 && p - sorted[i - 1] > 1) out.push(null);
    out.push(p);
  });
  return out;
}

/** Whether `needle` (already trimmed and lower-cased) is in any of `fields`. An empty needle matches. */
export function matchesQuery(needle: string, ...fields: (string | null | undefined)[]): boolean {
  if (!needle) return true;
  return fields.some((f) => f != null && f.toLowerCase().includes(needle));
}

export interface PagedFilter<T, F> extends PageView<T> {
  query: string;
  filters: F;
  /** Every matching row, before paging. */
  matched: T[];
  setQuery: (query: string) => void;
  setFilters: (patch: Partial<F>) => void;
  setPage: (page: number) => void;
  reset: () => void;
}

/**
 * Search, filter and page `items`. `match` gets each item, the query trimmed and lower-cased, and
 * the filters. Changing the query or a filter goes back to page one.
 */
export function usePagedFilter<T, F>(
  items: readonly T[],
  opts: { filters: F; match: (item: T, query: string, filters: F) => boolean; pageSize?: number },
): PagedFilter<T, F> {
  const [state, dispatch] = useReducer(pagedReducer<F>, { query: "", filters: opts.filters, page: 0 });
  const { match } = opts;
  const needle = state.query.trim().toLowerCase();
  const matched = useMemo(() => items.filter((it) => match(it, needle, state.filters)), [items, needle, state.filters, match]);
  const view = pageOf(matched, state.page, opts.pageSize ?? PAGE_SIZE);
  const initial = opts.filters;
  return {
    ...view,
    query: state.query,
    filters: state.filters,
    matched,
    setQuery: (query) => dispatch({ type: "query", query }),
    setFilters: (patch) => dispatch({ type: "filters", patch }),
    setPage: (page) => dispatch({ type: "page", page }),
    reset: () => dispatch({ type: "reset", filters: initial }),
  };
}
