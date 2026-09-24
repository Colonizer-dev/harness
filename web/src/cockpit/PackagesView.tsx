// The workspace dashboard's Packages tab: what the workspace's repositories publish, what they
// depend on (from their lockfiles), and the supply-chain risks in those dependencies. Everything is
// read by the mothership from its own clones and the public registries (see crates/colonizer/src/
// deps.rs); the first scan of a workspace runs in the background, so a view that answers
// "scanning" asks again every few seconds until it lands.
import { useEffect, useMemo, useState, type ReactElement, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { cx, timeAgo } from "../components/ui";
import type {
  Dependency,
  Ecosystem,
  PackagesDependencies,
  PackagesPublished,
  RiskSeverity,
  ScanPending,
  SupplyChain,
  SupplyRisk,
} from "../types";

type Tab = "published" | "dependencies" | "supply";
const TABS: { id: Tab; label: string }[] = [
  { id: "published", label: "Published" },
  { id: "dependencies", label: "Dependencies" },
  { id: "supply", label: "Supply chain" },
];
/** Rows shown before "Show more". */
const PAGE = 150;

const ECO: Record<Ecosystem | "github", { label: string; short: string; color: string }> = {
  npm: { label: "npm", short: "npm", color: "#cb3837" },
  cargo: { label: "crates.io", short: "rs", color: "#dea584" },
  pypi: { label: "PyPI", short: "py", color: "#3572A5" },
  go: { label: "Go", short: "go", color: "#00ADD8" },
  dart: { label: "pub.dev", short: "dt", color: "#00B4AB" },
  swift: { label: "SwiftPM", short: "sw", color: "#F05138" },
  github: { label: "GitHub Packages", short: "gh", color: "var(--text)" },
};

export function EcoIcon({ eco, size = 18 }: { eco: Ecosystem | "github"; size?: number }): ReactElement {
  const e = ECO[eco] ?? ECO.npm;
  return (
    <span
      title={e.label}
      aria-label={e.label}
      className="inline-grid shrink-0 place-items-center rounded-[5px] font-mono font-semibold leading-none text-white"
      style={{ width: size, height: size, fontSize: size * 0.42, background: e.color, color: eco === "github" ? "var(--bg)" : "#fff" }}
    >
      {e.short}
    </span>
  );
}

const SEVERITY: Record<RiskSeverity, { label: string; tone: string }> = {
  critical: { label: "Critical", tone: "bg-err text-on-accent" },
  high: { label: "High", tone: "bg-err/15 text-err" },
  moderate: { label: "Moderate", tone: "bg-warn/15 text-warn" },
  low: { label: "Low", tone: "bg-panel-3 text-muted" },
};

function isPending(v: unknown): v is ScanPending {
  return typeof v === "object" && v !== null && (v as ScanPending).status === "scanning";
}

/** Fetch, and while the mothership answers "scanning", ask again every five seconds. */
function useScan<T>(fetcher: () => Promise<T | ScanPending>, key: string): { data: T | null; pending: string | null; error: string | null } {
  const [state, setState] = useState<{ key: string; data: T | null; pending: string | null; error: string | null }>({ key, data: null, pending: null, error: null });
  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const load = () => {
      fetcher().then(
        (v) => {
          if (!alive) return;
          if (isPending(v)) {
            setState({ key, data: null, pending: v.message, error: null });
            timer = setTimeout(load, 5000);
          } else {
            setState({ key, data: v, pending: null, error: null });
          }
        },
        (e) => alive && setState({ key, data: null, pending: null, error: errorMessage(e) }),
      );
    };
    load();
    return () => {
      alive = false;
      if (timer) clearTimeout(timer);
    };
    // The fetcher changes identity every render; the key names what it fetches.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return state.key === key ? state : { data: null, pending: null, error: null };
}

export function PackagesView({ org, onOpenColony }: { org: string; onOpenColony?: (id: string) => void }): ReactElement {
  const api = useApi();
  const [tab, setTab] = useState<Tab>("published");
  const published = useScan<PackagesPublished>(() => api.orgPublished(org), `published:${org}`);
  const deps = useScan<PackagesDependencies>(() => api.orgDependencies(org), `deps:${org}`);
  const supply = useScan<SupplyChain>(() => api.orgSupplyChain(org), `supply:${org}`);
  const riskCount = supply.data ? supply.data.risks.length : null;
  const counts: Record<Tab, number | null> = {
    published: published.data ? published.data.packages.length : null,
    dependencies: deps.data ? deps.data.totals.direct + deps.data.totals.transitive : null,
    supply: riskCount,
  };

  return (
    <div className="pt-1">
      <div role="tablist" aria-label="packages" className="mb-3 flex gap-1 border-b border-border">
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            aria-selected={tab === t.id}
            onClick={() => setTab(t.id)}
            className={cx(
              "-mb-px inline-flex cursor-pointer items-center gap-1.5 border-0 border-b-2 bg-transparent px-3 py-2 text-[13px]",
              tab === t.id ? "border-accent text-text" : "border-transparent text-muted hover:text-text",
            )}
          >
            {t.label}
            {counts[t.id] != null && (
              <span className={cx("rounded-full px-1.5 text-[11px] tabular-nums", t.id === "supply" && (supply.data?.counts.critical || supply.data?.counts.high) ? "bg-err/15 text-err" : "bg-panel-3 text-muted")}>
                {counts[t.id]}
              </span>
            )}
          </button>
        ))}
      </div>
      {tab === "published" && <Loaded state={published}>{(d) => <PublishedList data={d} />}</Loaded>}
      {tab === "dependencies" && <Loaded state={deps}>{(d) => <DependencyList data={d} />}</Loaded>}
      {tab === "supply" && <Loaded state={supply}>{(d) => <SupplyList data={d} onOpenColony={onOpenColony} />}</Loaded>}
    </div>
  );
}

function Loaded<T>({ state, children }: { state: { data: T | null; pending: string | null; error: string | null }; children: (d: T) => ReactNode }): ReactElement {
  if (state.error) return <p className="py-3 text-[13px] text-err">{state.error}</p>;
  if (state.data) return <>{children(state.data)}</>;
  return (
    <p className="flex items-center gap-2 py-3 text-[13px] text-muted">
      <span aria-hidden="true" className="size-2 animate-pulse rounded-full bg-accent" />
      {state.pending ?? "Loading…"}
    </p>
  );
}

function RepoNotes({ repos }: { repos: { repo: string; error?: string; skipped?: string[] }[] }): ReactElement | null {
  const bad = repos.filter((r) => r.error || (r.skipped && r.skipped.length > 0));
  if (bad.length === 0) return null;
  return (
    <details className="mt-3 text-[12px] text-faint">
      <summary className="cursor-pointer">{bad.length} {bad.length === 1 ? "repository" : "repositories"} not fully read</summary>
      <ul className="m-0 mt-1 list-none space-y-0.5 p-0">
        {bad.map((r) => (
          <li key={r.repo}>
            <span className="font-mono text-muted">{r.repo}</span>: {r.error ?? `skipped ${r.skipped?.join(", ")} (too large)`}
          </li>
        ))}
      </ul>
    </details>
  );
}

// --- Published ----------------------------------------------------------------------------------

function PublishedList({ data }: { data: PackagesPublished }): ReactElement {
  const gh = data.github_packages.packages;
  return (
    <div>
      {data.packages.length === 0 ? (
        <p className="py-3 text-[13px] text-faint">No package manifests found in this workspace's repositories.</p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[720px] border-collapse text-[13px]">
            <thead>
              <tr className="border-b border-border text-left text-[12px] text-muted">
                <th className="py-2 pr-3 font-normal">Package</th>
                <th className="py-2 pr-3 font-normal">Where</th>
                <th className="py-2 pr-3 font-normal">In repo</th>
                <th className="py-2 pr-3 font-normal">Published</th>
                <th className="py-2 pr-3 text-right font-normal">Downloads</th>
                <th className="py-2 font-normal">Last publish</th>
              </tr>
            </thead>
            <tbody>
              {data.packages.map((p) => (
                <tr key={`${p.ecosystem}:${p.name}:${p.repo}:${p.path}`} className="border-b border-border/60">
                  <td className="py-2 pr-3">
                    <span className="flex items-center gap-2">
                      <EcoIcon eco={p.ecosystem} />
                      {p.published?.url ? (
                        <a href={p.published.url} target="_blank" rel="noreferrer" className="font-mono text-text hover:underline">
                          {p.name}
                        </a>
                      ) : (
                        <span className="font-mono text-text">{p.name}</span>
                      )}
                      <StatusBadge status={p.status} />
                    </span>
                  </td>
                  <td className="py-2 pr-3 font-mono text-[12px] text-muted">
                    {p.repo.split("/")[1]}
                    {p.path ? `/${p.path}` : ""}
                  </td>
                  <td className="py-2 pr-3 tabular-nums">{p.version ?? "—"}</td>
                  <td className="py-2 pr-3 tabular-nums">
                    {p.published?.latest ?? "—"}
                    {p.unreleased_changes && (
                      <span className="ml-1.5 rounded bg-warn/15 px-1 text-[11px] text-warn" title="the repository's version is ahead of the registry's latest">
                        unreleased
                      </span>
                    )}
                  </td>
                  <td className="py-2 pr-3 text-right tabular-nums text-muted">
                    {p.published?.downloads != null ? `${p.published.downloads.toLocaleString()} · ${p.published.downloads_period ?? ""}` : "—"}
                  </td>
                  <td className="py-2 text-muted">{p.published?.published_at ? timeAgo(p.published.published_at) : "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <h3 className="mb-1.5 mt-5 flex items-center gap-2 text-[13px] font-medium text-text">
        <EcoIcon eco="github" size={16} /> GitHub Packages
      </h3>
      {gh.length === 0 ? (
        <p className="text-[12.5px] text-faint">{data.github_packages.note ?? "None published under this organization."}</p>
      ) : (
        <ul className="m-0 grid list-none gap-1.5 p-0 sm:grid-cols-2">
          {gh.map((p) => (
            <li key={`${p.type}:${p.name}`} className="flex items-center gap-2 rounded-lg border border-border px-2.5 py-1.5 text-[12.5px]">
              <span className="rounded bg-panel-3 px-1 font-mono text-[10.5px] text-muted">{p.type}</span>
              {p.url ? (
                <a href={p.url} target="_blank" rel="noreferrer" className="min-w-0 truncate font-mono text-text hover:underline">
                  {p.name}
                </a>
              ) : (
                <span className="min-w-0 truncate font-mono text-text">{p.name}</span>
              )}
              <span className="ml-auto shrink-0 text-faint">
                {p.visibility}
                {p.versions != null ? ` · ${p.versions} versions` : ""}
                {p.updated_at ? ` · ${timeAgo(p.updated_at)}` : ""}
              </span>
            </li>
          ))}
        </ul>
      )}
      <RepoNotes repos={data.repos} />
    </div>
  );
}

function StatusBadge({ status }: { status: "published" | "unpublished" | "private" }): ReactElement {
  const tone = status === "published" ? "bg-ok/15 text-ok" : status === "private" ? "bg-panel-3 text-muted" : "bg-panel-3 text-faint";
  return <span className={cx("rounded-full px-1.5 py-px text-[10.5px]", tone)}>{status}</span>;
}

// --- Dependencies -------------------------------------------------------------------------------

export function filterDependencies(
  list: Dependency[],
  f: { eco: Ecosystem | "all"; directOnly: boolean; outdated: boolean; vulnerable: boolean; q: string },
): Dependency[] {
  const q = f.q.trim().toLowerCase();
  return list.filter(
    (d) =>
      (f.eco === "all" || d.ecosystem === f.eco) &&
      (!f.directOnly || d.direct === true) &&
      (!f.outdated || d.outdated) &&
      (!f.vulnerable || d.vulnerable) &&
      (!q || d.name.toLowerCase().includes(q)),
  );
}

function DependencyList({ data }: { data: PackagesDependencies }): ReactElement {
  const [eco, setEco] = useState<Ecosystem | "all">("all");
  const [directOnly, setDirectOnly] = useState(false);
  const [outdated, setOutdated] = useState(false);
  const [vulnerable, setVulnerable] = useState(false);
  const [q, setQ] = useState("");
  const [shown, setShown] = useState(PAGE);
  const [open, setOpen] = useState<string | null>(null);
  const rows = useMemo(
    () =>
      filterDependencies(data.packages, { eco, directOnly, outdated, vulnerable, q }).sort(
        (a, b) => Number(b.vulnerable) - Number(a.vulnerable) || Number(b.direct === true) - Number(a.direct === true) || a.name.localeCompare(b.name),
      ),
    [data.packages, eco, directOnly, outdated, vulnerable, q],
  );
  return (
    <div>
      <div className="mb-3 flex flex-wrap gap-2">
        {data.ecosystems.map((e) => (
          <button
            key={e.ecosystem}
            type="button"
            onClick={() => setEco(eco === e.ecosystem ? "all" : e.ecosystem)}
            aria-pressed={eco === e.ecosystem}
            className={cx(
              "inline-flex cursor-pointer items-center gap-2 rounded-lg border px-2.5 py-1.5 text-[12.5px]",
              eco === e.ecosystem ? "border-accent bg-accent-soft text-text" : "border-border bg-transparent text-muted hover:text-text",
            )}
          >
            <EcoIcon eco={e.ecosystem} size={16} />
            <span className="tabular-nums">
              {e.direct} direct · {e.transitive} transitive
            </span>
          </button>
        ))}
        <span className="ml-auto self-center text-[12.5px] text-muted">
          <span className="text-warn">{data.totals.outdated} outdated</span> · <span className="text-err">{data.totals.vulnerable} with advisories</span>
        </span>
      </div>
      <div className="mb-2 flex flex-wrap items-center gap-3 text-[12.5px] text-muted">
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search packages…"
          aria-label="search dependencies"
          className="w-56 rounded-md border border-border bg-transparent px-2 py-1 text-[12.5px] text-text outline-none placeholder:text-faint focus:border-border-strong"
        />
        <Check label="Direct only" checked={directOnly} onChange={setDirectOnly} />
        <Check label="Outdated" checked={outdated} onChange={setOutdated} />
        <Check label="Vulnerable" checked={vulnerable} onChange={setVulnerable} />
        <span className="ml-auto tabular-nums text-faint">{rows.length} shown</span>
      </div>
      <ul className="m-0 list-none divide-y divide-border/60 border-y border-border p-0">
        {rows.slice(0, shown).map((d) => {
          const key = `${d.ecosystem}:${d.name}`;
          const isOpen = open === key;
          return (
            <li key={key} className="py-1.5">
              <button
                type="button"
                aria-expanded={isOpen}
                onClick={() => setOpen(isOpen ? null : key)}
                className="flex w-full cursor-pointer items-center gap-2 border-0 bg-transparent p-0 text-left text-[13px]"
              >
                <EcoIcon eco={d.ecosystem} size={16} />
                <span className="font-mono text-text">{d.name}</span>
                <span className="rounded-full bg-panel-3 px-1.5 text-[10.5px] text-muted">{d.direct === true ? (d.dev ? "dev" : "direct") : d.direct === false ? "transitive" : "unknown"}</span>
                <span className="flex flex-wrap gap-1">
                  {d.versions.map((v) => (
                    <span
                      key={v.version}
                      className={cx(
                        "rounded px-1 font-mono text-[11px] tabular-nums",
                        v.vulns.length > 0 ? "bg-err/15 text-err" : d.drift ? "bg-warn/15 text-warn" : "bg-panel-2 text-muted",
                      )}
                      title={v.vulns.length > 0 ? v.vulns.map((x) => x.id).join(", ") : d.drift ? "more than one version in use" : undefined}
                    >
                      {v.version}
                    </span>
                  ))}
                </span>
                {d.outdated && d.latest && <span className="text-[11.5px] text-warn">→ {d.latest}</span>}
                <span className="ml-auto text-[11.5px] text-faint">{d.versions.reduce((n, v) => n + v.users.length, 0)} uses</span>
              </button>
              {isOpen && (
                <div className="mt-1.5 space-y-1 pl-6 text-[12px] text-muted">
                  {d.versions.map((v) => (
                    <div key={v.version}>
                      <span className="font-mono text-text">{v.version}</span>
                      {" in "}
                      {v.users.map((u) => `${u.repo.split("/")[1]}/${u.path}`).join(", ")}
                      {v.vulns.map((x) => (
                        <a key={x.id} href={x.url} target="_blank" rel="noreferrer" className="ml-2 text-err hover:underline">
                          {x.id} ({x.severity}){x.fixed ? ` · fixed in ${x.fixed}` : ""}
                        </a>
                      ))}
                    </div>
                  ))}
                </div>
              )}
            </li>
          );
        })}
      </ul>
      {rows.length > shown && (
        <button type="button" onClick={() => setShown((n) => n + PAGE)} className="mt-2 cursor-pointer border-0 bg-transparent p-0 text-[12.5px] text-accent hover:underline">
          Show {Math.min(PAGE, rows.length - shown)} more
        </button>
      )}
      <RepoNotes repos={data.repos} />
    </div>
  );
}

function Check({ label, checked, onChange }: { label: string; checked: boolean; onChange: (v: boolean) => void }): ReactElement {
  return (
    <label className="inline-flex cursor-pointer items-center gap-1.5">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      {label}
    </label>
  );
}

// --- Supply chain -------------------------------------------------------------------------------

export function filterRisks(list: SupplyRisk[], f: { severity: RiskSeverity | "all"; fixable: boolean; kind: string }): SupplyRisk[] {
  return list.filter((r) => (f.severity === "all" || r.severity === f.severity) && (!f.fixable || r.fix.available) && (f.kind === "all" || r.kind === f.kind));
}

const KIND_LABEL: Record<string, string> = {
  vulnerability: "Vulnerability",
  yanked: "Yanked",
  deprecated: "Deprecated",
  "install-script": "Install script",
  "fresh-release": "Fresh release",
  "young-package": "Young package",
  "low-downloads": "Low downloads",
  typosquat: "Possible typosquat",
  license: "Licence",
  "unpinned-source": "Unpinned source",
  "wildcard-range": "Wildcard range",
  "unpinned-version": "Unpinned version",
  "missing-integrity": "No integrity hash",
};

/** The instructions a colony gets for fixing one risk. */
export function fixInstructions(r: SupplyRisk): string {
  const where = r.users.map((u) => `${u.path} (${u.repo})`).join(", ");
  const target = r.fix.available && r.fix.version ? `Upgrade it to ${r.fix.version} or later` : "Upgrade it to a safe version or replace it";
  const via = r.via.length > 0 ? ` It is pulled in via ${r.via.join(", ")}; upgrade that dependency if the package itself is not direct.` : "";
  return `Supply-chain fix: ${r.ecosystem} package "${r.name}"${r.version ? ` ${r.version}` : ""} — ${KIND_LABEL[r.kind] ?? r.kind}: ${r.reason}. Used in ${where}.${via} ${target}, update the lockfile, run the repository's tests and builds, and explain the change in the PR. Do not add any Claude/AI attribution.`;
}

function SupplyList({ data, onOpenColony }: { data: SupplyChain; onOpenColony?: (id: string) => void }): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [severity, setSeverity] = useState<RiskSeverity | "all">("all");
  const [fixable, setFixable] = useState(false);
  const [kind, setKind] = useState("all");
  const [shown, setShown] = useState(PAGE);
  const [sending, setSending] = useState<string | null>(null);
  const kinds = useMemo(() => [...new Set(data.risks.map((r) => r.kind))].sort(), [data.risks]);
  const rows = useMemo(() => filterRisks(data.risks, { severity, fixable, kind }), [data.risks, severity, fixable, kind]);

  const handOff = async (r: SupplyRisk, key: string) => {
    const repo = r.users[0]?.repo;
    if (!repo) return;
    setSending(key);
    try {
      const s = await api.createSession({ repo, title: `Supply chain: ${r.name}`, instructions: fixInstructions(r), autopilot: true });
      toast({
        title: `A colony is fixing ${r.name}`,
        body: `${repo} · ${KIND_LABEL[r.kind] ?? r.kind}`,
        kind: "success",
        action: onOpenColony ? { label: "Watch it work", onClick: () => onOpenColony(s.id) } : undefined,
      });
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSending(null);
    }
  };

  return (
    <div>
      <div className="mb-3 flex flex-wrap items-center gap-2">
        {(["critical", "high", "moderate", "low"] as RiskSeverity[]).map((s) => (
          <button
            key={s}
            type="button"
            aria-pressed={severity === s}
            onClick={() => setSeverity(severity === s ? "all" : s)}
            className={cx(
              "inline-flex cursor-pointer items-center gap-1.5 rounded-lg border px-2.5 py-1 text-[12.5px]",
              severity === s ? "border-accent" : "border-border",
              "bg-transparent",
            )}
          >
            <span className={cx("rounded-full px-1.5 text-[11px] tabular-nums", SEVERITY[s].tone)}>{data.counts[s] ?? 0}</span>
            {SEVERITY[s].label}
          </button>
        ))}
        <Check label={`Fix available (${data.fixable})`} checked={fixable} onChange={setFixable} />
        <select
          value={kind}
          onChange={(e) => setKind(e.target.value)}
          aria-label="risk kind"
          className="rounded-md border border-border bg-panel px-2 py-1 text-[12.5px] text-text"
        >
          <option value="all">All kinds</option>
          {kinds.map((k) => (
            <option key={k} value={k}>
              {KIND_LABEL[k] ?? k}
            </option>
          ))}
        </select>
        <span className="ml-auto text-[12px] tabular-nums text-faint">{rows.length} shown</span>
      </div>
      {rows.length === 0 ? (
        <p className="py-3 text-[13px] text-faint">{data.risks.length === 0 ? "No supply-chain risks found." : "Nothing matches these filters."}</p>
      ) : (
        <ul className="m-0 list-none divide-y divide-border/60 border-y border-border p-0">
          {rows.slice(0, shown).map((r, i) => {
            const key = `${r.kind}:${r.ecosystem}:${r.name}:${r.version ?? ""}:${i}`;
            return (
              <li key={key} className="flex flex-wrap items-start gap-x-3 gap-y-1 py-2 text-[13px]">
                <span className={cx("mt-0.5 shrink-0 rounded-full px-1.5 text-[11px]", SEVERITY[r.severity]?.tone ?? SEVERITY.low.tone)}>{SEVERITY[r.severity]?.label ?? r.severity}</span>
                <EcoIcon eco={r.ecosystem} size={16} />
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-baseline gap-x-2">
                    {r.url ? (
                      <a href={r.url} target="_blank" rel="noreferrer" className="font-mono text-text hover:underline">
                        {r.name}
                        {r.version ? `@${r.version}` : ""}
                      </a>
                    ) : (
                      <span className="font-mono text-text">
                        {r.name}
                        {r.version ? `@${r.version}` : ""}
                      </span>
                    )}
                    <span className="text-[12px] text-muted">{KIND_LABEL[r.kind] ?? r.kind}</span>
                    {r.fix.available && <span className="rounded bg-ok/15 px-1 text-[11px] text-ok">fix{r.fix.version ? ` ${r.fix.version}` : ""}</span>}
                    {!r.direct && r.via.length > 0 && <span className="text-[11.5px] text-faint">via {r.via.join(", ")}</span>}
                  </div>
                  <div className="text-[12.5px] text-muted">{r.reason}</div>
                  <div className="text-[11.5px] text-faint">{r.users.map((u) => `${u.repo.split("/")[1]}/${u.path}`).join(" · ")}</div>
                </div>
                {r.users.length > 0 && (
                  <button
                    type="button"
                    disabled={sending === key}
                    onClick={() => void handOff(r, key)}
                    className="shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-2 py-1 text-[12px] text-muted hover:border-border-strong hover:text-text disabled:opacity-50"
                  >
                    {sending === key ? "Sending…" : "Hand to a colony"}
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {rows.length > shown && (
        <button type="button" onClick={() => setShown((n) => n + PAGE)} className="mt-2 cursor-pointer border-0 bg-transparent p-0 text-[12.5px] text-accent hover:underline">
          Show {Math.min(PAGE, rows.length - shown)} more
        </button>
      )}
      <p className="mt-2 text-[11.5px] text-faint">{data.note}</p>
      <RepoNotes repos={data.repos} />
    </div>
  );
}
