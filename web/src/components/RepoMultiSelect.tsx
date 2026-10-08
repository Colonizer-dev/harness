// One repository multi-select for every setting that takes a list of repositories (issue #1212):
// a dropdown with search, "All repositories", "All in <org>" per org and each repository as a
// checkbox; closed, the choice reads as chips. Free text ("Add owner/name…") covers repositories
// the mothership does not know yet. The value is the list the server stores: `*`, `owner` and
// `owner/name` entries. Hidden orgs (issue #1213) are in no list and in no "All".
import { useCallback, useContext, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent, type ReactElement } from "react";
import { ApiContext } from "../context";
import { Popover } from "../cockpit/chat/Popover";
import { filterGroups, groupByOrg } from "../cockpit/RepoPicker";
import type { OrgInfo } from "../types";
import { Avatar } from "./Avatar";
import { IconCheck, IconChevronDown, IconSearch, IconX } from "./icons";
import { ALL, buildRows, chipLabel, chipsFor, coversOrg, coversRepo, removeEntry, visibleOrgs, visibleRepos, pickRow, type Row } from "./repoSelect";
import { cx } from "./ui";

export function RepoMultiSelect({
  value,
  onChange,
  label = "repositories",
  repos,
  orgs,
  org,
  allowAll = true,
  maxChips = 3,
  disabled = false,
  placeholder = "Choose repositories",
  id,
}: {
  value: string[];
  onChange: (next: string[]) => void;
  /** The accessible name: "repositories to merge in". */
  label?: string;
  /** The repositories to offer (`owner/name`); omitted, the mothership's own list. */
  repos?: string[];
  /** The orgs, for avatars and hidden ones; omitted, the mothership's own list. */
  orgs?: OrgInfo[];
  /** Offer only this org's repositories: a setting that belongs to one workspace. */
  org?: string;
  /** False for a setting whose server side has no wildcard; "All repositories" is then not offered. */
  allowAll?: boolean;
  maxChips?: number;
  disabled?: boolean;
  placeholder?: string;
  id?: string;
}): ReactElement {
  const api = useContext(ApiContext);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [fetched, setFetched] = useState<{ repos: { full_name: string; description: string | null }[]; orgs: OrgInfo[] } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const trigger = useRef<HTMLDivElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const base = useId();

  // The mothership's lists, fetched once, when the dropdown first opens and nothing was passed in.
  useEffect(() => {
    if (!open || fetched || !api || (repos && orgs)) return;
    let alive = true;
    Promise.all([repos ? Promise.resolve([]) : api.repos().catch(() => []), orgs ? Promise.resolve(orgs) : api.orgs().catch(() => [])]).then(([r, o]) => {
      if (alive) setFetched({ repos: r, orgs: o });
    });
    return () => {
      alive = false;
    };
  }, [open, fetched, api, repos, orgs]);

  const allOrgs = orgs ?? fetched?.orgs ?? [];
  const known = useMemo(() => visibleOrgs(allOrgs), [allOrgs]);
  const names = useMemo(() => {
    const all = visibleRepos(repos ?? (fetched?.repos ?? []).map((r) => r.full_name), allOrgs);
    return org ? all.filter((r) => r.split("/")[0].toLowerCase() === org.toLowerCase()) : all;
  }, [repos, fetched, allOrgs, org]);
  const descriptions = useMemo(() => new Map((fetched?.repos ?? []).map((r) => [r.full_name, r.description])), [fetched]);
  const groups = useMemo(() => groupByOrg(names, known), [names, known]);
  const shown = filterGroups(groups, query, (r) => descriptions.get(r) ?? null);

  const rows = buildRows(shown, query, allowAll, (repo) => descriptions.get(repo) ?? null);

  useEffect(() => {
    if (open) search.current?.focus();
  }, [open]);
  useEffect(() => {
    setActive((a) => Math.min(a, Math.max(0, rows.length - 1)));
  }, [rows.length]);
  useEffect(() => {
    list.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView?.({ block: "nearest" });
  }, [active]);

  const close = useCallback(() => {
    setOpen(false);
    setQuery("");
    setError(null);
  }, []);

  const pick = (row: Row) => {
    const next = pickRow(value, row);
    setError(next.error);
    if (!next.error) {
      onChange(next.value);
      if (row.kind === "add") setQuery("");
    }
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(rows.length - 1, i + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (e.key === "Enter" || (e.key === " " && !query)) {
      e.preventDefault();
      const row = rows[active];
      if (row) pick(row);
    }
  };

  const { shown: chips, more } = chipsFor(value, maxChips);
  const checkedOf = (row: Row): { checked: boolean; implied: boolean } => {
    if (row.kind === "all") return { checked: value.includes(ALL), implied: false };
    if (row.kind === "org") return { checked: coversOrg(value, row.org), implied: value.includes(ALL) };
    if (row.kind === "repo") {
      const own = value.some((v) => v.toLowerCase() === row.repo.toLowerCase());
      return { checked: coversRepo(value, row.repo), implied: !own && coversRepo(value, row.repo) };
    }
    return { checked: false, implied: false };
  };

  return (
    <div className="min-w-0">
      <div
        ref={trigger}
        id={id}
        className={cx(
          "flex min-h-9 w-full min-w-0 flex-wrap items-center gap-1.5 rounded-lg border border-border bg-panel px-2 py-1 text-small-lg text-text transition-colors",
          open && "border-accent",
          disabled && "opacity-55",
        )}
      >
        {chips.map((entry) => (
          <span key={entry} className="inline-flex max-w-full items-center gap-1 rounded-full border border-border px-2 py-0.5 font-mono text-meta-lg text-text">
            <span className="truncate">{chipLabel(entry)}</span>
            <button
              type="button"
              disabled={disabled}
              aria-label={`remove ${chipLabel(entry)}`}
              onClick={() => onChange(removeEntry(value, entry))}
              className="grid cursor-pointer place-items-center border-0 bg-transparent p-0 text-faint hover:text-text"
            >
              <IconX size={11} />
            </button>
          </span>
        ))}
        {more > 0 && (
          <button type="button" disabled={disabled} aria-label={`${more} more`} onClick={() => setOpen(true)} className="cursor-pointer rounded-full border-0 bg-panel-2 px-2 py-0.5 text-meta-lg text-muted">
            +{more}
          </button>
        )}
        <button
          type="button"
          disabled={disabled}
          aria-haspopup="listbox"
          aria-expanded={open}
          aria-label={label}
          onClick={() => (open ? close() : setOpen(true))}
          className="flex min-w-24 flex-1 cursor-pointer items-center justify-between gap-2 border-0 bg-transparent p-0 text-left text-small-lg text-faint"
        >
          <span className="truncate">{value.length === 0 ? placeholder : "Edit"}</span>
          <IconChevronDown size={14} />
        </button>
      </div>

      <Popover open={open} onClose={close} anchor={trigger} width={420} label={`Choose ${label}`}>
        <div onKeyDown={onKey} className="flex min-h-0 flex-1 flex-col">
          <div className="flex items-center gap-2 border-b border-border px-3 py-2">
            <IconSearch size={14} className="text-faint" />
            <input
              ref={search}
              value={query}
              onChange={(e) => {
                setQuery(e.target.value);
                setError(null);
                setActive(0);
              }}
              placeholder="Find an org or repository…"
              aria-label="find a repository"
              role="combobox"
              aria-expanded="true"
              aria-controls={`${base}-list`}
              aria-activedescendant={rows[active] ? `${base}-${active}` : undefined}
              className="bare-field min-w-0 flex-1 border-0 bg-transparent text-body-sm text-text outline-none placeholder:text-faint"
            />
          </div>
          <div ref={list} id={`${base}-list`} role="listbox" aria-multiselectable="true" aria-label={label} className="scroll-thin min-h-0 flex-1 overflow-y-auto p-1">
            {rows.length === 0 && <div className="px-3 py-3 text-small-lg text-faint">{api && !fetched && !repos ? "Loading…" : "Nothing matches."}</div>}
            {rows.map((row, i) => {
              const { checked, implied } = checkedOf(row);
              const text =
                row.kind === "all" ? "All repositories" : row.kind === "org" ? `All in ${row.org}` : row.kind === "repo" ? row.repo.split("/")[1] : `Add “${row.text}”…`;
              return (
                <div
                  key={`${row.kind}-${row.kind === "org" ? row.org : row.kind === "repo" ? row.repo : row.kind === "add" ? row.text : ""}`}
                  id={`${base}-${i}`}
                  data-index={i}
                  role="option"
                  aria-selected={checked}
                  aria-label={row.kind === "repo" ? row.repo : text}
                  onMouseMove={() => setActive(i)}
                  onClick={() => pick(row)}
                  className={cx(
                    "flex cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-1.5",
                    i === active && "bg-panel-2",
                    row.kind === "repo" && "ml-5",
                    row.kind === "org" && "mt-1",
                  )}
                >
                  {row.kind !== "add" && (
                    <span aria-hidden="true" className={cx("grid size-4 shrink-0 place-items-center rounded border", checked ? "border-accent bg-accent text-bg" : "border-border-strong", implied && "opacity-50")}>
                      {checked && <IconCheck size={11} />}
                    </span>
                  )}
                  {row.kind === "org" && <Avatar name={row.org} src={row.info?.avatar_url} size={18} rounded="md" />}
                  <div className="min-w-0 flex-1">
                    <div className={cx("truncate text-body-sm", row.kind === "repo" ? "font-mono" : "font-medium")}>{text}</div>
                    {row.kind === "repo" && row.description && <div className="truncate text-meta-lg text-faint">{row.description}</div>}
                    {row.kind === "all" && <div className="truncate text-meta-lg text-faint">Every repository of every org you show in Colonizer</div>}
                  </div>
                  {row.kind === "org" && <span className="shrink-0 text-meta tabular-nums text-faint">{row.count}</span>}
                </div>
              );
            })}
          </div>
          {error && <div className="border-t border-border px-3 py-2 text-small text-err">{error}</div>}
        </div>
      </Popover>
    </div>
  );
}
