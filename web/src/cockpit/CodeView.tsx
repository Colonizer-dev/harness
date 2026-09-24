// The Code page: one workspace's repositories as code — languages, lines, coverage, commits,
// branches and releases — each opening a VS Code-like editor (CodeEditor, loaded on demand).
// Everything comes from the mothership: its bare clones for lines and files, GitHub via `gh` for
// the rest. Nothing here writes to GitHub; the editor's Create PR does, after a confirm.
import { Suspense, lazy, useEffect, useMemo, useState, type ReactElement } from "react";
import { useApi } from "../context";
import type { OrgInfo, Repo, RepoCoverage, RepoGitSummary, RepoLoc, RepoMeta, Session } from "../types";
import { Avatar } from "../components/Avatar";
import { Spinner, cx, sameOrg, store, stored, timeAgo } from "../components/ui";
import { Segmented } from "./ListControls";
import { languageColor } from "./RepoPicker";
import { compact, shares, sumLoc } from "./code";

const CodeEditor = lazy(() => import("./CodeEditor"));

/** Grid of cards or a compact list; remembered per browser. */
export type CodeLayout = "grid" | "list";
export const CODE_LAYOUT_KEY = "colonizer.codeLayout";
export function readCodeLayout(): CodeLayout {
  return stored(CODE_LAYOUT_KEY) === "list" ? "list" : "grid";
}

interface RepoFacts {
  meta?: RepoMeta | null;
  loc?: RepoLoc | null;
  coverage?: RepoCoverage | null;
  git?: RepoGitSummary | null;
  error?: string;
}

/** Loads each repository's facts once per page, four at a time, as the page shows them. */
function useRepoFacts(repos: readonly string[]): Record<string, RepoFacts> {
  const api = useApi();
  const [facts, setFacts] = useState<Record<string, RepoFacts>>({});
  const key = repos.join(",");
  useEffect(() => {
    let cancelled = false;
    const queue = [...repos];
    const worker = async () => {
      while (!cancelled && queue.length > 0) {
        const repo = queue.shift()!;
        const [meta, loc, coverage, git] = await Promise.allSettled([api.repoMeta(repo), api.repoLoc(repo), api.repoCoverage(repo), api.repoGitSummary(repo)]);
        if (cancelled) return;
        const value = <T,>(r: PromiseSettledResult<T>) => (r.status === "fulfilled" ? r.value : null);
        setFacts((f) => ({
          ...f,
          [repo]: {
            meta: value(meta),
            loc: value(loc),
            coverage: value(coverage),
            git: value(git),
            error: loc.status === "rejected" ? String((loc.reason as Error)?.message ?? loc.reason) : undefined,
          },
        }));
      }
    };
    void Promise.all([worker(), worker(), worker(), worker()]);
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, api]);
  return facts;
}

function LanguageBar({ rows, height = 8 }: { rows: { name: string; percent: number }[]; height?: number }): ReactElement | null {
  if (rows.length === 0) return null;
  return (
    <div className="flex w-full overflow-hidden rounded-full bg-panel-3" style={{ height }} role="img" aria-label={rows.map((r) => `${r.name} ${r.percent}%`).join(", ")}>
      {rows.map((r) => (
        <span key={r.name} title={`${r.name} ${r.percent}%`} style={{ width: `${r.percent}%`, background: languageColor(r.name) }} />
      ))}
    </div>
  );
}

function LanguageLegend({ rows, limit = 4 }: { rows: { name: string; percent: number }[]; limit?: number }): ReactElement {
  return (
    <div className="flex flex-wrap gap-x-3 gap-y-1 text-[11.5px] text-muted">
      {rows.slice(0, limit).map((r) => (
        <span key={r.name} className="inline-flex items-center gap-1.5">
          <span aria-hidden="true" className="size-2 rounded-full" style={{ background: languageColor(r.name) }} />
          <span className="text-text">{r.name}</span> {r.percent}%
        </span>
      ))}
    </div>
  );
}

function CommitBars({ weeks, height = 28 }: { weeks: number[]; height?: number }): ReactElement | null {
  if (weeks.length === 0) return null;
  const max = Math.max(1, ...weeks);
  return (
    <div className="flex items-end gap-px" style={{ height }} role="img" aria-label={`${weeks.reduce((a, b) => a + b, 0)} commits in 52 weeks`}>
      {weeks.map((w, i) => (
        <span key={i} title={`${w} commits`} className={cx("w-[3px] rounded-sm", w > 0 ? "bg-accent" : "bg-panel-3")} style={{ height: Math.max(2, (w / max) * height) }} />
      ))}
    </div>
  );
}

function CoveragePill({ coverage }: { coverage: RepoCoverage | null | undefined }): ReactElement {
  if (!coverage) return <span className="text-faint">…</span>;
  if (!coverage.measured) {
    return (
      <span className="text-faint" title={`${coverage.reason}. Upload an lcov, istanbul (coverage-summary.json), cobertura or llvm-cov report as a CI artifact named with "coverage" to show it here.`}>
        not measured
      </span>
    );
  }
  const tone = coverage.percent >= 80 ? "text-ok" : coverage.percent >= 50 ? "text-warn" : "text-err";
  return (
    <span className={cx("font-medium tabular-nums", tone)} title={`${coverage.format} report ${coverage.file} in artifact ${coverage.artifact}`}>
      {coverage.percent}%
    </span>
  );
}

function Stat({ label, children }: { label: string; children: React.ReactNode }): ReactElement {
  return (
    <div className="min-w-0">
      <div className="text-[11px] uppercase tracking-wide text-faint">{label}</div>
      <div className="truncate text-[14px] text-text">{children}</div>
    </div>
  );
}

/** What a card and a list row both show about one repository. */
function repoSummary(facts: RepoFacts | undefined) {
  const langRows = facts?.loc ? shares(facts.loc.by_language) : (facts?.meta?.languages.map((l) => ({ name: l.name, percent: l.percent })) ?? []);
  const commits = facts?.meta?.commits_weekly ?? [];
  const yearCommits = commits.reduce((a, b) => a + b, 0);
  const release = facts?.git?.release?.tagName ?? facts?.git?.latest_tag ?? null;
  return { langRows, commits, yearCommits, release };
}

const liveCount = (sessions: Session[], repo: string): number => sessions.filter((s) => s.repo === repo && ["starting", "running", "waiting_for_answer"].includes(s.status)).length;

/** The list layout: one compact row per repository, the same facts as the cards. */
function RepoCodeTable({ repos, facts, sessions, onOpen }: { repos: string[]; facts: Record<string, RepoFacts>; sessions: Session[]; onOpen: (repo: string) => void }): ReactElement {
  const th = "py-2 pr-4 font-normal";
  return (
    <div className="mt-6 overflow-x-auto rounded-xl border border-border bg-panel">
      <table className="w-full min-w-[860px] border-collapse text-[13px]">
        <thead>
          <tr className="border-b border-border text-left text-[12px] text-muted">
            <th className={cx(th, "pl-4")}>Repository</th>
            <th className={cx(th, "w-40")}>Languages</th>
            <th className={cx(th, "text-right")}>Lines</th>
            <th className={cx(th, "text-right")}>Coverage</th>
            <th className={cx(th, "text-right")}>Commits · 52w</th>
            <th className={cx(th, "text-right")}>Branches</th>
            <th className={cx(th, "text-right")}>Open PRs</th>
            <th className={th}>Release</th>
            <th className={th}>Pushed</th>
            <th className="py-2 pr-4" aria-label="actions" />
          </tr>
        </thead>
        <tbody>
          {repos.map((repo) => {
            const f = facts[repo];
            const { langRows, yearCommits, release } = repoSummary(f);
            const live = liveCount(sessions, repo);
            return (
              <tr key={repo} className="border-b border-border/60 last:border-b-0 hover:bg-panel-2">
                <td className="max-w-[18rem] py-2.5 pl-4 pr-4">
                  <div className="flex items-center gap-2">
                    <span className="truncate font-mono text-[13px] font-semibold text-text">{repo.split("/")[1]}</span>
                    {live > 0 && <span className="shrink-0 text-[11.5px] text-accent">{live} live</span>}
                  </div>
                  <div className="truncate text-[12px] text-muted">{f?.meta?.description ?? (f ? "No description" : "Loading…")}</div>
                </td>
                <td className="py-2.5 pr-4">
                  <LanguageBar rows={langRows} height={6} />
                  {langRows[0] && (
                    <div className="mt-1 truncate text-[11.5px] text-muted">
                      {langRows[0].name} {langRows[0].percent}%
                    </div>
                  )}
                </td>
                <td className="py-2.5 pr-4 text-right tabular-nums text-text">{f?.loc ? compact(f.loc.total) : f?.error ? <span className="text-faint" title={f.error}>—</span> : "…"}</td>
                <td className="py-2.5 pr-4 text-right">
                  <CoveragePill coverage={f?.coverage} />
                </td>
                <td className="py-2.5 pr-4 text-right tabular-nums text-text">{f?.meta ? (f.meta.stats_pending ? "counting…" : yearCommits) : "…"}</td>
                <td className="py-2.5 pr-4 text-right tabular-nums text-text">{f?.git?.branches ?? "…"}</td>
                <td className="py-2.5 pr-4 text-right tabular-nums text-text">{f?.git ? (f.git.open_prs ?? "—") : "…"}</td>
                <td className="max-w-[9rem] truncate py-2.5 pr-4 font-mono text-[12px] text-muted">{f?.git ? (release ?? "none") : "…"}</td>
                <td className="whitespace-nowrap py-2.5 pr-4 text-[12px] text-faint">{f?.meta?.pushed_at ? timeAgo(f.meta.pushed_at) : "—"}</td>
                <td className="py-2.5 pr-4 text-right">
                  <button type="button" onClick={() => onOpen(repo)} className="cursor-pointer whitespace-nowrap rounded-lg border border-border-strong bg-panel-2 px-2.5 py-1 text-[12px] font-medium text-text hover:bg-panel-3">
                    Open editor
                  </button>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function RepoCodeCard({ repo, facts, live, onOpen }: { repo: string; facts: RepoFacts | undefined; live: number; onOpen: () => void }): ReactElement {
  const name = repo.split("/")[1];
  const { langRows, commits, yearCommits, release } = repoSummary(facts);
  return (
    <article className="flex flex-col gap-3 rounded-xl border border-border bg-panel p-4">
      <header className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <h3 className="m-0 truncate font-mono text-[14px] font-semibold text-text">{name}</h3>
          <p className="m-0 mt-0.5 line-clamp-2 text-[12.5px] text-muted">{facts?.meta?.description ?? (facts ? "No description" : "Loading…")}</p>
        </div>
        <button type="button" onClick={onOpen} className="shrink-0 cursor-pointer rounded-lg border border-border-strong bg-panel-2 px-3 py-1.5 text-[12.5px] font-medium text-text hover:bg-panel-3">
          Open editor
        </button>
      </header>
      <LanguageBar rows={langRows} />
      <LanguageLegend rows={langRows} />
      <div className="grid grid-cols-3 gap-3">
        <Stat label="Lines">{facts?.loc ? compact(facts.loc.total) : facts?.error ? <span className="text-faint" title={facts.error}>—</span> : "…"}</Stat>
        <Stat label="Coverage">
          <CoveragePill coverage={facts?.coverage} />
        </Stat>
        <Stat label="Commits · 52w">{facts?.meta ? (facts.meta.stats_pending ? "counting…" : yearCommits) : "…"}</Stat>
        <Stat label="Branches">{facts?.git?.branches ?? "…"}</Stat>
        <Stat label="Open PRs">{facts?.git ? (facts.git.open_prs ?? "—") : "…"}</Stat>
        <Stat label="Release">{facts?.git ? (release ?? "none") : "…"}</Stat>
      </div>
      <CommitBars weeks={commits} />
      <footer className="flex items-center gap-2 text-[11.5px] text-faint">
        <span className="flex -space-x-1.5">
          {(facts?.meta?.contributors ?? []).slice(0, 5).map((c) => (
            <Avatar key={c.login} name={c.login} src={c.avatar_url || undefined} size={20} rounded="full" title={`${c.login} · ${c.contributions} commits`} className="border border-panel" />
          ))}
        </span>
        {facts?.meta?.pushed_at && <span>pushed {timeAgo(facts.meta.pushed_at)}</span>}
        {live > 0 && <span className="ml-auto text-accent">{live} live {live === 1 ? "colony" : "colonies"}</span>}
      </footer>
    </article>
  );
}

export function CodeView({
  orgs,
  repos,
  sessions,
  selectedOrg,
  onSelectOrg,
  onCreated,
  onOpenColony,
  openRequest,
  initialLayout,
}: {
  orgs: OrgInfo[];
  repos: Repo[];
  sessions: Session[];
  selectedOrg: string | null;
  onSelectOrg: (org: string | null) => void;
  onCreated: (session: Session) => void;
  onOpenColony: (id: string) => void;
  /** Open this file in the editor (from the Chat view); a new `n` asks again. */
  openRequest?: { repo: string; path: string; n: number } | null;
  /** The layout to start in; read from localStorage when absent (tests pin it). */
  initialLayout?: CodeLayout;
}): ReactElement {
  const [editing, setEditing] = useState<string | null>(openRequest?.repo ?? null);
  const [initialPath, setInitialPath] = useState<string | null>(openRequest?.path ?? null);
  const [seenRequest, setSeenRequest] = useState(openRequest?.n ?? 0);
  const [layout, setLayoutState] = useState<CodeLayout>(() => initialLayout ?? readCodeLayout());
  const setLayout = (next: CodeLayout) => {
    setLayoutState(next);
    store(CODE_LAYOUT_KEY, next);
  };
  if (openRequest && openRequest.n !== seenRequest) {
    setSeenRequest(openRequest.n);
    setEditing(openRequest.repo);
    setInitialPath(openRequest.path);
  }
  const org = orgs.find((o) => sameOrg(o.org, selectedOrg)) ?? null;
  const orgRepos = useMemo(
    () => repos.filter((r) => selectedOrg && sameOrg(r.full_name.split("/")[0], selectedOrg) && !r.archived).map((r) => r.full_name),
    [repos, selectedOrg],
  );
  const facts = useRepoFacts(orgRepos);
  const totals = useMemo(() => sumLoc(orgRepos.map((r) => facts[r]?.loc)), [orgRepos, facts]);
  const yearCommits = orgRepos.reduce((t, r) => t + (facts[r]?.meta?.commits_weekly.reduce((a, b) => a + b, 0) ?? 0), 0);

  if (editing) {
    return (
      <Suspense
        fallback={
          <div className="flex flex-1 items-center justify-center gap-2 text-[13px] text-muted">
            <Spinner /> Loading the editor…
          </div>
        }
      >
        <CodeEditor
          key={editing}
          repo={editing}
          initialPath={initialPath}
          onClose={() => {
            setEditing(null);
            setInitialPath(null);
          }}
          onCreated={onCreated}
          onOpenColony={onOpenColony}
        />
      </Suspense>
    );
  }

  if (!selectedOrg || !org) {
    return (
      <main className="scroll-thin flex-1 overflow-y-auto px-8 py-8">
        <h1 className="m-0 text-[30px] font-semibold tracking-[-0.035em] text-text">Code</h1>
        <p className="mt-2 text-[14px] text-muted">The Code page is per workspace. Pick one:</p>
        <div className="mt-5 grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-2">
          {orgs
            .filter((o) => !o.awaiting_decision)
            .map((o) => (
              <button key={o.org} type="button" onClick={() => onSelectOrg(o.org)} className="flex cursor-pointer items-center gap-2.5 rounded-lg border border-border bg-panel px-3 py-2 text-left hover:bg-panel-2">
                <Avatar name={o.org} src={o.avatar_url} size={24} rounded="md" />
                <span className="min-w-0">
                  <span className="block truncate text-[13.5px] text-text">{o.org}</span>
                  {o.description && <span className="block truncate text-[11.5px] text-faint">{o.description}</span>}
                </span>
              </button>
            ))}
        </div>
      </main>
    );
  }

  const orgShares = shares(totals.by_language);
  return (
    <main className="scroll-thin flex-1 overflow-y-auto px-8 py-8">
      <header className="flex flex-wrap items-start gap-4">
        <Avatar name={org.org} src={org.avatar_url} size={44} rounded="xl" />
        <div className="min-w-0 flex-1">
          <h1 className="m-0 text-[30px] font-semibold leading-tight tracking-[-0.035em] text-text">Code · {org.org}</h1>
          {org.description && <p className="m-0 mt-1 text-[14px] text-muted">{org.description}</p>}
        </div>
        {orgRepos.length > 0 && (
          <Segmented
            label="layout"
            value={layout}
            onChange={setLayout}
            options={[
              { value: "grid", label: <><LayoutIcon kind="grid" /> Grid</>, title: "Repository cards" },
              { value: "list", label: <><LayoutIcon kind="list" /> List</>, title: "One compact row per repository" },
            ]}
          />
        )}
      </header>
      <section aria-label="workspace totals" className="mt-6 grid gap-4 rounded-xl border border-border bg-panel p-4 sm:grid-cols-[repeat(3,minmax(0,1fr))_2fr]">
        <Stat label="Repositories">{orgRepos.length}</Stat>
        <Stat label="Lines of code">{totals.total ? compact(totals.total) : "…"}</Stat>
        <Stat label="Commits · 52w">{yearCommits || "…"}</Stat>
        <div className="flex min-w-0 flex-col gap-1.5">
          <LanguageBar rows={orgShares} height={10} />
          <LanguageLegend rows={orgShares} limit={6} />
        </div>
      </section>
      {orgRepos.length === 0 ? (
        <p className="mt-6 text-[13px] text-faint">No repositories in {org.org} that this GitHub login can see.</p>
      ) : layout === "list" ? (
        <RepoCodeTable repos={orgRepos} facts={facts} sessions={sessions} onOpen={setEditing} />
      ) : (
        <div className="mt-6 grid grid-cols-1 gap-4 lg:grid-cols-2">
          {orgRepos.map((repo) => (
            <RepoCodeCard key={repo} repo={repo} facts={facts[repo]} live={liveCount(sessions, repo)} onOpen={() => setEditing(repo)} />
          ))}
        </div>
      )}
    </main>
  );
}

function LayoutIcon({ kind }: { kind: CodeLayout }): ReactElement {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
      {kind === "grid" ? (
        <>
          <rect x="4" y="4" width="7" height="7" rx="1.5" />
          <rect x="13" y="4" width="7" height="7" rx="1.5" />
          <rect x="4" y="13" width="7" height="7" rx="1.5" />
          <rect x="13" y="13" width="7" height="7" rx="1.5" />
        </>
      ) : (
        <path d="M4 6h16M4 12h16M4 18h16" />
      )}
    </svg>
  );
}
