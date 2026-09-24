// The repository picker on the Nest map: repositories grouped under their organizations, each org
// with its avatar and GitHub description, each repository with its tagline, main language, a
// half-year of commits and its top contributors — plus a card for the chosen repository with
// GitHub's language bar, a year of weekly commits and who wrote it. Meta comes from
// GET /api/repos/{owner}/{repo}/meta, fetched lazily and kept for the page's lifetime.
import { useContext, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactElement } from "react";
import { ApiContext } from "../context";
import { Avatar } from "../components/Avatar";
import { cx, timeAgo } from "../components/ui";
import type { OrgInfo, RepoMeta } from "../types";

// --- Language colours (GitHub linguist) ------------------------------------------------------

const LANGUAGE_COLORS: Record<string, string> = {
  Rust: "#dea584",
  TypeScript: "#3178c6",
  JavaScript: "#f1e05a",
  Python: "#3572A5",
  Go: "#00ADD8",
  Swift: "#F05138",
  Kotlin: "#A97BFF",
  Java: "#b07219",
  Shell: "#89e051",
  HTML: "#e34c26",
  CSS: "#663399",
  Ruby: "#701516",
  C: "#555555",
  "C++": "#f34b7d",
  "C#": "#178600",
  Dart: "#00B4AB",
  Vue: "#41b883",
  Svelte: "#ff3e00",
  MDX: "#fcb32c",
  Dockerfile: "#384d54",
  HCL: "#844FBA",
};

/** A language's GitHub colour; grey for any the table does not name. */
export function languageColor(name: string | null | undefined): string {
  return (name && LANGUAGE_COLORS[name]) || "#8b949e";
}

// --- Meta cache ------------------------------------------------------------------------------

type MetaEntry = RepoMeta | "loading" | "error";
const metaCache = new Map<string, MetaEntry>();
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((l) => l());

/** Fetches a repository's meta once per page; later calls read the cache. */
function loadMeta(api: { repoMeta(repo: string): Promise<RepoMeta> } | null, repo: string): void {
  if (!api || metaCache.has(repo)) return;
  metaCache.set(repo, "loading");
  api.repoMeta(repo).then(
    (meta) => {
      metaCache.set(repo, meta);
      notify();
    },
    () => {
      metaCache.set(repo, "error");
      notify();
    },
  );
}

/** Re-renders when any repository's meta lands. */
function useMetaVersion(): number {
  const [version, setVersion] = useState(0);
  useEffect(() => {
    const listener = () => setVersion((v) => v + 1);
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
    };
  }, []);
  return version;
}

function metaOf(repo: string): RepoMeta | null {
  const entry = metaCache.get(repo);
  return entry && typeof entry === "object" ? entry : null;
}

// --- Grouping and search ---------------------------------------------------------------------

export interface OrgGroup {
  org: string;
  info: OrgInfo | undefined;
  repos: string[];
}

/** Repositories grouped under their organization, orgs in the order their repos first appear. */
export function groupByOrg(repos: readonly string[], orgs: readonly OrgInfo[]): OrgGroup[] {
  const groups = new Map<string, OrgGroup>();
  for (const repo of repos) {
    const org = repo.split("/")[0];
    const key = org.toLowerCase();
    const group = groups.get(key) ?? { org, info: orgs.find((o) => o.org.toLowerCase() === key), repos: [] };
    group.repos.push(repo);
    groups.set(key, group);
  }
  return [...groups.values()];
}

/**
 * The groups a query leaves: an org whose name or description matches keeps all its repositories;
 * otherwise only its repositories whose name or description matches, with the org header kept
 * above them. Empty queries leave everything.
 */
export function filterGroups(groups: readonly OrgGroup[], query: string, describe: (repo: string) => string | null): OrgGroup[] {
  const q = query.trim().toLowerCase();
  if (!q) return [...groups];
  const hit = (text: string | null | undefined) => Boolean(text && text.toLowerCase().includes(q));
  return groups
    .map((g) => {
      if (hit(g.org) || hit(g.info?.description)) return g;
      return { ...g, repos: g.repos.filter((r) => hit(r.split("/")[1]) || hit(describe(r))) };
    })
    .filter((g) => g.repos.length > 0);
}

// --- Small charts ----------------------------------------------------------------------------

/** A line of weekly commits: the last `weeks` of them, scaled to their own peak. */
function Sparkline({ values, weeks = 26, width = 64, height = 18 }: { values: number[]; weeks?: number; width?: number; height?: number }) {
  const data = values.slice(-weeks);
  if (data.length < 2) return <span className="inline-block" style={{ width, height }} />;
  const peak = Math.max(1, ...data);
  const points = data.map((v, i) => `${((i / (data.length - 1)) * width).toFixed(1)},${(height - 1 - (v / peak) * (height - 2)).toFixed(1)}`).join(" ");
  return (
    <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} aria-hidden="true" className="shrink-0 text-accent">
      <polyline points={points} fill="none" stroke="currentColor" strokeWidth={1.4} strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  );
}

function AvatarStack({ people, max = 4, size = 18 }: { people: RepoMeta["contributors"]; max?: number; size?: number }) {
  const shown = people.slice(0, max);
  const more = people.length - shown.length;
  return (
    <span className="flex shrink-0 items-center">
      {shown.map((p, i) => (
        <span key={p.login} title={`${p.login} · ${p.contributions} commits`} className={cx("rounded-full ring-2 ring-panel", i > 0 && "-ml-1.5")}>
          <Avatar name={p.login} src={p.avatar_url || undefined} size={size} rounded="full" />
        </span>
      ))}
      {more > 0 && <span className="ml-1 text-[10.5px] tabular-nums text-faint">+{more}</span>}
    </span>
  );
}

function LanguageChip({ name }: { name: string | null | undefined }) {
  if (!name) return null;
  return (
    <span className="inline-flex shrink-0 items-center gap-1 text-[11px] text-muted">
      <span aria-hidden="true" className="size-2 rounded-full" style={{ background: languageColor(name) }} />
      {name}
    </span>
  );
}

// --- The picker ------------------------------------------------------------------------------

export function RepoPicker({ repos, value, onChange }: { repos: string[]; value: string | null; onChange: (repo: string) => void }): ReactElement {
  const api = useContext(ApiContext);
  useMetaVersion();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [orgs, setOrgs] = useState<OrgInfo[]>([]);
  const [active, setActive] = useState(0);
  const trigger = useRef<HTMLButtonElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!api) return;
    let alive = true;
    api.orgs().then((o) => alive && setOrgs(o), () => {});
    return () => {
      alive = false;
    };
  }, [api]);

  // The chosen repository's meta straight away; the rest when the list opens.
  useEffect(() => {
    if (value) loadMeta(api, value);
  }, [api, value]);
  useEffect(() => {
    if (open) repos.forEach((r) => loadMeta(api, r));
  }, [api, open, repos]);

  const groups = useMemo(() => groupByOrg(repos, orgs), [repos, orgs]);
  const shown = filterGroups(groups, query, (r) => metaOf(r)?.description ?? null);
  const options = shown.flatMap((g) => g.repos);

  useEffect(() => {
    if (!open) return;
    setActive(Math.max(0, value ? options.indexOf(value) : 0));
    search.current?.focus();
    // Only on open: typing re-filters without jumping the highlight back.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);
  useEffect(() => {
    list.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const close = (refocus = true) => {
    setOpen(false);
    setQuery("");
    if (refocus) trigger.current?.focus();
  };
  const pick = (repo: string) => {
    onChange(repo);
    close();
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(options.length - 1, i + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const repo = options[active];
      if (repo) pick(repo);
    }
  };

  const current = value ? metaOf(value) : null;
  let index = -1;

  return (
    <div className="relative">
      <button
        ref={trigger}
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label="repository"
        onClick={() => (open ? close() : setOpen(true))}
        className="inline-flex max-w-[380px] cursor-pointer items-center gap-2 rounded-full border border-border bg-transparent py-1 pl-1.5 pr-3 text-[12.5px] text-text hover:border-border-strong"
      >
        {value && <Avatar name={value.split("/")[0]} src={groups.find((g) => g.repos.includes(value))?.info?.avatar_url} size={18} rounded="full" />}
        <span className="min-w-0 truncate font-mono">
          {value ? (
            <>
              <span className="text-faint">{value.split("/")[0]}/</span>
              {value.split("/")[1]}
            </>
          ) : (
            "Choose a repository"
          )}
        </span>
        <LanguageChip name={current?.primary_language} />
        <span aria-hidden="true" className="text-faint">
          ▾
        </span>
      </button>

      {open && (
        <>
          <div aria-hidden="true" className="fixed inset-0 z-40" onClick={() => close(false)} />
          <div
            onKeyDown={onKey}
            className="absolute left-0 top-full z-50 mt-2 flex max-h-[min(560px,calc(100vh-160px))] w-[min(520px,calc(100vw-32px))] flex-col overflow-hidden rounded-xl border border-border-strong bg-panel shadow-[0_16px_48px_rgb(0_0_0/0.4)]"
          >
            <div className="border-b border-border p-2">
              <input
                ref={search}
                value={query}
                onChange={(e) => {
                  setQuery(e.target.value);
                  setActive(0);
                }}
                placeholder="Find an organization or repository…"
                aria-label="find a repository"
                aria-controls="repo-picker-list"
                className="w-full rounded-md border border-border bg-transparent px-2.5 py-1.5 text-[13px] text-text outline-none placeholder:text-faint focus:border-border-strong"
              />
            </div>
            <div ref={list} id="repo-picker-list" role="listbox" aria-label="repositories" className="scroll-thin min-h-0 flex-1 overflow-y-auto py-1">
              {shown.length === 0 && <p className="px-3 py-3 text-[12.5px] text-faint">Nothing matches “{query.trim()}”.</p>}
              {shown.map((g) => (
                <div key={g.org} role="group" aria-label={g.org}>
                  <div className="flex items-center gap-2.5 px-3 pb-1 pt-2.5">
                    <Avatar name={g.org} src={g.info?.avatar_url} size={22} rounded="md" />
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-[12.5px] font-semibold text-text">{g.org}</div>
                      {g.info?.description && <div className="truncate text-[11.5px] text-faint">{g.info.description}</div>}
                    </div>
                    <span className="shrink-0 text-[11px] tabular-nums text-faint">{g.repos.length}</span>
                  </div>
                  {g.repos.map((repo) => {
                    index += 1;
                    const i = index;
                    const meta = metaOf(repo);
                    const selected = repo === value;
                    return (
                      <div
                        key={repo}
                        role="option"
                        aria-selected={selected}
                        data-index={i}
                        onMouseEnter={() => setActive(i)}
                        onClick={() => pick(repo)}
                        className={cx(
                          "mx-1.5 flex cursor-pointer items-center gap-3 rounded-lg py-1.5 pl-9 pr-2.5",
                          i === active ? "bg-panel-2" : "bg-transparent",
                        )}
                      >
                        <div className="min-w-0 flex-1">
                          <div className="flex items-center gap-2">
                            <span className={cx("truncate font-mono text-[12.5px]", selected ? "text-accent" : "text-text")}>{repo.split("/")[1]}</span>
                            <LanguageChip name={meta?.primary_language} />
                          </div>
                          <div className="truncate text-[11.5px] text-faint">{meta ? (meta.description ?? "No description") : metaCache.get(repo) === "error" ? "Could not read from GitHub" : "…"}</div>
                        </div>
                        {meta && <Sparkline values={meta.commits_weekly} />}
                        {meta && <AvatarStack people={meta.contributors} />}
                      </div>
                    );
                  })}
                </div>
              ))}
            </div>
          </div>
        </>
      )}
    </div>
  );
}

// --- The chosen repository's card ------------------------------------------------------------

export function RepoCard({ repo }: { repo: string }): ReactElement | null {
  const api = useContext(ApiContext);
  useMetaVersion();
  useEffect(() => {
    loadMeta(api, repo);
  }, [api, repo]);
  const meta = metaOf(repo);
  if (!meta) return null;
  const top = meta.languages.slice(0, 3);
  const weeks = meta.commits_weekly;
  const peak = Math.max(1, ...weeks);
  const total = weeks.reduce((a, b) => a + b, 0);
  return (
    <section aria-label={`${repo} on GitHub`} className="flex flex-wrap items-start gap-x-6 gap-y-3 rounded-xl border border-border bg-panel-2 px-4 py-3">
      <div className="min-w-0 flex-1 basis-64">
        <div className="flex items-center gap-2">
          <span className="truncate font-mono text-[13px] font-semibold text-text">{meta.full_name}</span>
          {meta.html_url && (
            <a href={meta.html_url} target="_blank" rel="noreferrer" className="shrink-0 text-[11.5px] text-muted underline decoration-dotted underline-offset-2 hover:text-text">
              GitHub ↗
            </a>
          )}
        </div>
        <p className="mt-0.5 line-clamp-2 text-[12.5px] text-muted">{meta.description ?? "No description on GitHub."}</p>
        <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11.5px] text-faint">
          <span>★ {meta.stars}</span>
          {meta.pushed_at && <span>pushed {timeAgo(meta.pushed_at)}</span>}
          {meta.homepage && (
            <a href={meta.homepage} target="_blank" rel="noreferrer" className="truncate hover:text-text">
              {meta.homepage.replace(/^https?:\/\//, "")}
            </a>
          )}
        </div>
      </div>

      {meta.languages.length > 0 && (
        <div className="w-[220px] shrink-0">
          <div className="flex h-2 overflow-hidden rounded-full" role="img" aria-label={top.map((l) => `${l.name} ${l.percent}%`).join(", ")}>
            {meta.languages.map((l) => (
              <span key={l.name} title={`${l.name} ${l.percent}%`} style={{ width: `${l.percent}%`, background: languageColor(l.name) }} />
            ))}
          </div>
          <div className="mt-1.5 flex flex-wrap gap-x-3 gap-y-0.5">
            {top.map((l) => (
              <span key={l.name} className="inline-flex items-center gap-1 text-[11px] text-muted">
                <span aria-hidden="true" className="size-2 rounded-full" style={{ background: languageColor(l.name) }} />
                <span className="text-text">{l.name}</span> {l.percent}%
              </span>
            ))}
          </div>
        </div>
      )}

      <div className="shrink-0">
        {weeks.length > 0 ? (
          <div role="img" aria-label={`${total} commits in the last year`}>
            <div className="flex h-8 items-end gap-px">
              {weeks.map((v, i) => (
                <span key={i} title={`${v} commits`} className="w-[3px] rounded-sm bg-accent" style={{ height: `${Math.max(v > 0 ? 8 : 3, (v / peak) * 100)}%`, opacity: v > 0 ? 0.9 : 0.25 }} />
              ))}
            </div>
            <div className="mt-1 text-[11px] text-faint">{total} commits · 52 weeks</div>
          </div>
        ) : (
          <div className="text-[11px] text-faint">{meta.stats_pending ? "GitHub is counting commits…" : "No commit history"}</div>
        )}
      </div>

      {meta.contributors.length > 0 && (
        <div className="shrink-0">
          <AvatarStack people={meta.contributors} max={8} size={22} />
          <div className="mt-1 text-[11px] text-faint">{meta.contributors.length} top contributors</div>
        </div>
      )}
    </section>
  );
}
