import { useCallback, useEffect, useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import type { CratefieldCatalog, CratefieldModule } from "../../types";
import { IconExternal } from "../icons";
import { Badge, Button, Spinner, inputClass, type Tone } from "../ui";
import { Code, Pane } from "./ui";

const TONE: Record<CratefieldModule["status"], Tone> = { published: "ok", unreleased: "warn", planned: "neutral" };
const LABEL: Record<CratefieldModule["status"], string> = { published: "published", unreleased: "on main, unreleased", planned: "planned" };

export function filterModules(modules: readonly CratefieldModule[], { query, status, port }: { query: string; status: string[]; port: string[] }): CratefieldModule[] {
  const q = query.toLowerCase();
  return modules.filter((m) => {
    if (q && !m.name.toLowerCase().includes(q) && !m.summary.toLowerCase().includes(q) && !m.crate_name.toLowerCase().includes(q) && !m.slug.toLowerCase().includes(q)) return false;
    if (status.length > 0 && !status.includes(m.status)) return false;
    if (port.length > 0) {
      const allPorts = [...m.needs, ...m.optional];
      if (!port.some((p) => allPorts.includes(p))) return false;
    }
    return true;
  });
}

export function portsOf(modules: readonly CratefieldModule[]): string[] {
  const ports = new Set<string>();
  for (const m of modules) {
    m.needs.forEach((p) => ports.add(p));
    m.optional.forEach((p) => ports.add(p));
  }
  return Array.from(ports).sort();
}

export function catalogueNotice(catalog: CratefieldCatalog): string | null {
  if (catalog.source === "live" && catalog.registry === "live") return null;
  const dateStr = (() => {
    if (!catalog.fetched_at) return "unknown date";
    try {
      const d = new Date(catalog.fetched_at);
      return isNaN(d.getTime()) ? "unknown date" : d.toLocaleDateString();
    } catch {
      return "unknown date";
    }
  })();
  const regMsg = catalog.registry !== "live" ? `. Published status from ${catalog.registry}.` : "";
  if (catalog.source === "snapshot") return `Catalogue unreachable, showing the bundled snapshot from ${dateStr}${catalog.error ? `: ${catalog.error}` : ""}${regMsg}`;
  if (catalog.source === "cache") return `Catalogue unreachable, showing the copy fetched ${dateStr}${regMsg}`;
  if (catalog.registry !== "live") return `Published status comes from a ${catalog.registry} copy of crates.io.`;
  return null;
}

function ModuleCard({ module }: { module: CratefieldModule }) {
  const toast = useToast();
  const version = module.registry_version ?? module.version ?? "unknown";
  const initial = module.name.charAt(0).toUpperCase();

  const copyUseLine = useCallback(() => {
    const needs = module.needs.length > 0 ? `; it needs ${module.needs.join(", ")}.` : ".";
    const line = `Use the Cratefield module "${module.name}" (crate ${module.crate_name} ${version})${needs}`;
    navigator.clipboard?.writeText(line).then(
      () => toast("Copied to clipboard"),
      (e) => toast(`Failed to copy: ${errorMessage(e)}`, "error"),
    );
  }, [module, version, toast]);

  return (
    <div className="rounded-lg border border-border bg-panel p-4 space-y-3">
      <div className="flex items-baseline gap-2">
        <div className="flex h-8 w-8 flex-shrink-0 items-center justify-center rounded bg-accent/10 text-accent font-semibold text-small">
          {initial}
        </div>
        <div className="min-w-0 flex-1">
          <h4 className="text-body-sm font-semibold text-text truncate">{module.name}</h4>
          <p className="text-small-lg text-muted line-clamp-2">{module.summary}</p>
        </div>
      </div>

      <div className="flex items-center gap-1 flex-wrap">
        <Code>{version}</Code>
        <Code>{module.crate_name}</Code>
        <Badge tone={TONE[module.status]}>{LABEL[module.status]}</Badge>
      </div>

      <div className="space-y-1 text-small-lg text-muted">
        {module.needs.length > 0 && <p><span className="font-medium">Requires:</span> {module.needs.join(", ")}</p>}
        {module.optional.length > 0 && <p><span className="font-medium">Optional:</span> {module.optional.join(", ")}</p>}
        {module.depends_on.length > 0 && <p><span className="font-medium">Depends on:</span> {module.depends_on.join(", ")}</p>}
        <p className="text-muted">Secrets: not listed in the catalogue</p>
      </div>

      <div className="flex flex-wrap gap-2 pt-1">
        {module.links.docs_rs && (
          <a href={module.links.docs_rs} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 text-small text-accent hover:underline">
            docs.rs <IconExternal size={10} />
          </a>
        )}
        {module.links.crate && (
          <a href={module.links.crate} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 text-small text-accent hover:underline">
            crates.io <IconExternal size={10} />
          </a>
        )}
        {module.links.catalogue && (
          <a href={module.links.catalogue} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 text-small text-accent hover:underline">
            catalogue <IconExternal size={10} />
          </a>
        )}
        <Button variant="ghost" size="sm" onClick={copyUseLine}>
          Copy use line
        </Button>
      </div>
    </div>
  );
}

export function CratefieldModulesPane({ back }: { back?: () => void }) {
  const api = useApi();
  const [catalog, setCatalog] = useState<CratefieldCatalog | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<string[]>([]);
  const [portFilter, setPortFilter] = useState<string[]>([]);

  useEffect(() => {
    let cancelled = false;
    api
      .cratefieldCatalog()
      .then((c) => !cancelled && setCatalog(c))
      .catch((e) => !cancelled && setError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api]);

  const notice = catalog ? catalogueNotice(catalog) : null;
  const allPorts = catalog ? portsOf(catalog.modules) : [];
  const filtered = catalog ? filterModules(catalog.modules, { query, status: statusFilter, port: portFilter }) : [];

  return (
    <Pane title="Modules" subtitle="Cratefield modules and how to use them" back={back}>
      {!catalog ? (
        <p className="flex items-center gap-2 text-body-sm text-muted">
          <Spinner /> Loading…
        </p>
      ) : error ? (
        <p className="text-body-sm text-err">Failed to load catalogue: {error}</p>
      ) : (
        <div className="space-y-4">
          {notice && <p className="text-small-lg text-warn bg-warn-soft/30 rounded p-2 border border-warn-soft">{notice}</p>}

          <div>
            <label htmlFor="modules-search" className="block text-body-sm font-medium text-text mb-1">
              Search
            </label>
            <input
              id="modules-search"
              type="text"
              placeholder="Search modules, crate names..."
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              className={inputClass}
            />
          </div>

          <div className="grid grid-cols-2 gap-3 sm:grid-cols-1">
            <div>
              <label htmlFor="modules-status" className="block text-body-sm font-medium text-text mb-1">Status</label>
              <select
                id="modules-status"
                multiple
                value={statusFilter}
                onChange={(e) => setStatusFilter(Array.from(e.target.selectedOptions, (o) => o.value))}
                className={`${inputClass} p-2 h-auto`}
              >
                <option value="">All statuses</option>
                <option value="published">Published</option>
                <option value="unreleased">On main</option>
                <option value="planned">Planned</option>
              </select>
            </div>

            <div>
              <label htmlFor="modules-port" className="block text-body-sm font-medium text-text mb-1">Port</label>
              <select
                id="modules-port"
                multiple
                value={portFilter}
                onChange={(e) => setPortFilter(Array.from(e.target.selectedOptions, (o) => o.value))}
                className={`${inputClass} p-2 h-auto`}
              >
                <option value="">All ports</option>
                {allPorts.map((p) => (
                  <option key={p} value={p}>
                    {p}
                  </option>
                ))}
              </select>
            </div>
          </div>

          {filtered.length === 0 ? (
            <p className="text-body-sm text-muted">No modules match your filters.</p>
          ) : (
            <div className="grid gap-3 md:grid-cols-2 lg:grid-cols-3">
              {filtered.map((m) => (
                <ModuleCard key={m.slug} module={m} />
              ))}
            </div>
          )}
        </div>
      )}
    </Pane>
  );
}
