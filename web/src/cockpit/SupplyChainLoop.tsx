// The built-in "Dependencies & supply chain" loop (supply_chain_loop.rs), shown at the top of the
// Loops page: its switch and allowlist (both off/empty until the operator opts in), its cadence and
// rate limits, the host's scanners, and the last run's report — findings by severity, what it
// dispatched, what it skipped and why, and what needs a person's attention. A dry run lists the
// same report without starting anything or saving anything.
import { useCallback, useEffect, useState, type ReactElement } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { Badge, Button, Spinner, Switch, cx, inputClass } from "../components/ui";
import type { LoopCadence, SupplyChainLoop as LoopView, SupplyChainReport, SupplyChainSettings, SupplySeverity } from "../types";
import { describeLoopCadence, relative } from "./loops";

export const SUPPLY_ORIGIN = "supply-chain:";
export const SEVERITIES: SupplySeverity[] = ["critical", "high", "moderate", "low"];
const SEVERITY_TONE: Record<SupplySeverity, "err" | "warn" | "neutral"> = { critical: "err", high: "err", moderate: "warn", low: "neutral" };

/** The allowlist as typed: comma, space or newline separated, trimmed, without duplicates. */
export function parseAllow(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(/[\s,]+/)) {
    const entry = raw.trim().replace(/\/\*$/, "");
    if (entry && !out.some((x) => x.toLowerCase() === entry.toLowerCase())) out.push(entry);
  }
  return out;
}

export type CadenceChoice = "hourly" | "6h" | "daily" | "weekly" | "custom";

/** Which of the offered cadences a saved one is; anything else reads as custom (kept as saved). */
export function cadenceChoice(c: LoopCadence): CadenceChoice {
  if (c.every === "interval") return c.minutes === 60 ? "hourly" : c.minutes === 360 ? "6h" : "custom";
  if (c.every === "daily") return "daily";
  if (c.every === "weekly") return "weekly";
  return "custom";
}

/** The cadence for a choice; `custom` keeps the saved one. Times are UTC, off the hour. */
export function cadenceFor(choice: CadenceChoice, saved: LoopCadence): LoopCadence {
  switch (choice) {
    case "hourly":
      return { every: "interval", minutes: 60 };
    case "6h":
      return { every: "interval", minutes: 360 };
    case "daily":
      return saved.every === "daily" ? saved : { every: "daily", hour: 6, minute: 17 };
    case "weekly":
      return saved.every === "weekly" ? saved : { every: "weekly", weekday: 0, hour: 6, minute: 17 };
    default:
      return saved;
  }
}

/** "2 critical · 1 high", or "no findings". */
export function countsLine(counts: SupplyChainReport["counts"]): string {
  const parts = SEVERITIES.filter((s) => (counts[s] ?? 0) > 0).map((s) => `${counts[s]} ${s}`);
  return parts.length ? parts.join(" · ") : "no findings";
}

/** The last run's report: what was found, what was dispatched, what was skipped and why. */
export function SupplyReport({ report, now = Date.now(), onOpenColony }: { report: SupplyChainReport; now?: number; onOpenColony?: (id: string) => void }): ReactElement {
  const findings = report.repos.flatMap((r) => r.findings.map((f) => ({ ...f, repo: r.repo })));
  return (
    <div className="mt-3 space-y-3 text-small-lg">
      <div className="flex flex-wrap items-center gap-2 text-muted">
        <span className="font-medium text-text">{report.dry_run ? "Dry run" : "Last run"}</span>
        <span>{relative(report.finished_at, now)}</span>
        <span>· {report.trigger}</span>
        {report.blocked && <Badge tone="warn">report only: external writes are blocked</Badge>}
        {SEVERITIES.map((s) =>
          (report.counts[s] ?? 0) > 0 ? (
            <Badge key={s} tone={SEVERITY_TONE[s]}>
              {report.counts[s]} {s}
            </Badge>
          ) : null,
        )}
        {findings.length === 0 && <span>· no findings</span>}
      </div>
      {report.note && <p className="m-0 text-muted">{report.note}</p>}
      {report.attention.length > 0 && (
        <div className="rounded-lg border border-[var(--err,#dc2626)] px-3 py-2">
          <div className="font-medium text-text">Needs attention</div>
          <ul className="m-0 mt-1 list-none space-y-1 p-0">
            {report.attention.map((a) => (
              <li key={`${a.repo}:${a.package}:${a.id}`}>
                <span className="font-mono">{a.repo}</span> · {a.package}
                {a.version ? ` ${a.version}` : ""} — {a.reason}
              </li>
            ))}
          </ul>
        </div>
      )}
      {findings.length > 0 && (
        <table className="w-full border-collapse text-left">
          <thead className="text-faint">
            <tr>
              <th className="py-1 pr-2 font-normal">Severity</th>
              <th className="py-1 pr-2 font-normal">Package</th>
              <th className="py-1 pr-2 font-normal">Finding</th>
              <th className="py-1 pr-2 font-normal">Fix</th>
            </tr>
          </thead>
          <tbody>
            {findings.map((f, i) => (
              <tr key={`${f.repo}:${f.ecosystem}:${f.package}:${f.id ?? f.kind}:${i}`} className="border-t border-border align-top">
                <td className="py-1 pr-2">
                  <Badge tone={SEVERITY_TONE[f.severity]}>{f.severity}</Badge>
                </td>
                <td className="py-1 pr-2">
                  <span className="text-text">{f.package}</span>
                  {f.version ? <span className="text-faint"> {f.version}</span> : null}
                  <div className="font-mono text-meta text-faint">
                    {f.repo} · {f.lockfile || f.ecosystem}
                  </div>
                </td>
                <td className="py-1 pr-2 text-muted">
                  {f.kind !== "vulnerability" && <span className="text-text">{f.kind}: </span>}
                  {f.url ? (
                    <a href={f.url} target="_blank" rel="noreferrer" className="text-muted underline decoration-dotted">
                      {f.id ?? f.title}
                    </a>
                  ) : (
                    f.id
                  )}
                  {f.id ? " — " : ""}
                  {f.title}
                </td>
                <td className="py-1 pr-2 text-muted">
                  {f.fixed ? `${f.fix_via ? `${f.fix_via} ` : ""}${f.fixed}${f.major_bump ? " (major)" : ""}` : f.fix_available ? "available" : "none"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {report.dispatched.length > 0 && (
        <div>
          <div className="font-medium text-text">{report.dry_run ? "Would dispatch" : "Dispatched"}</div>
          <ul className="m-0 mt-1 list-none space-y-0.5 p-0 text-muted">
            {report.dispatched.map((d) => (
              <li key={`${d.repo}:${d.ecosystem}`}>
                {d.session && onOpenColony ? (
                  <button type="button" onClick={() => onOpenColony(d.session!)} className="cursor-pointer border-0 bg-transparent p-0 text-left text-muted underline decoration-dotted hover:text-text">
                    {d.title}
                  </button>
                ) : (
                  d.title
                )}{" "}
                · <span className="font-mono">{d.repo}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {report.skipped.length > 0 && (
        <div>
          <div className="font-medium text-text">Skipped</div>
          <ul className="m-0 mt-1 list-none space-y-0.5 p-0 text-muted">
            {report.skipped.map((s, i) => (
              <li key={`${s.repo}:${s.ecosystem}:${i}`}>
                <span className="font-mono">{s.repo}</span>
                {s.ecosystem ? ` (${s.ecosystem})` : ""}: {s.reason}
              </li>
            ))}
          </ul>
        </div>
      )}
      {report.repos.some((r) => r.missing.length > 0 || r.error) && (
        <ul className="m-0 list-none space-y-0.5 p-0 text-muted">
          {report.repos.flatMap((r) => [...(r.error ? [`${r.repo}: ${r.error}`] : []), ...r.missing.map((m) => `${r.repo}: ${m}`)]).map((line) => (
            <li key={line}>⚠ {line}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** The loop's card on the Loops page. */
export function SupplyChainLoopCard({ onOpenColony }: { onOpenColony: (id: string) => void }): ReactElement | null {
  const api = useApi();
  const toast = useToast();
  const [view, setView] = useState<LoopView | null>(null);
  const [draft, setDraft] = useState<SupplyChainSettings | null>(null);
  const [allowText, setAllowText] = useState("");
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState<"save" | "dry" | "run" | null>(null);
  const [dry, setDry] = useState<SupplyChainReport | null>(null);

  const load = useCallback(() => {
    api.supplyChainLoop().then(
      (v) => {
        setView(v);
        setDraft((d) => d ?? v.settings);
        setAllowText((t) => t || v.settings.allow.join(", "));
      },
      () => setView(null),
    );
  }, [api]);
  useEffect(() => {
    load();
    const t = setInterval(load, 60_000);
    return () => clearInterval(t);
  }, [load]);

  if (!view || !draft) return null;

  const save = async (settings: SupplyChainSettings) => {
    setBusy("save");
    try {
      const v = await api.saveSupplyChainLoop(settings);
      setView(v);
      setDraft(v.settings);
      setAllowText(v.settings.allow.join(", "));
      toast(v.settings.enabled ? `${v.name}: on for ${v.settings.allow.length || "no"} entr${v.settings.allow.length === 1 ? "y" : "ies"}` : `${v.name}: off`);
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(null);
    }
  };

  const run = async (dryRun: boolean) => {
    setBusy(dryRun ? "dry" : "run");
    try {
      const report = await api.runSupplyChainLoop({ dry_run: dryRun });
      if (dryRun) setDry(report);
      else {
        setDry(null);
        load();
      }
      toast(`${dryRun ? "Dry run" : "Run"}: ${countsLine(report.counts)}; ${dryRun ? "would dispatch" : "dispatched"} ${report.dispatched.length}`);
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(null);
    }
  };

  const s = view.settings;
  const withAllow = { ...draft, allow: parseAllow(allowText) };
  const missing = Object.entries(view.scanners).filter(([, on]) => !on).map(([name]) => name);
  const shown = dry ?? view.last_report;

  return (
    <section className="mt-6 rounded-xl border border-border px-4 py-3" aria-label={view.name}>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <div className="min-w-0 flex-1 basis-64">
          <div className="flex items-center gap-2">
            <span className="text-body-lg font-medium text-text">{view.name}</span>
            <Badge>built-in</Badge>
            {view.running && <Spinner />}
          </div>
          <div className="mt-0.5 text-small-lg text-muted">
            {s.enabled && s.allow.length > 0
              ? `${describeLoopCadence(s.cadence)} · ${s.allow.join(", ")}${view.next_run_at ? ` · next ${relative(view.next_run_at)}` : ""}`
              : s.enabled
                ? "on, but nothing is opted in: add an org or a repository"
                : "off · checks lockfiles on the host and opens pull requests with minimal bumps"}
            {view.blocked ? " · report only (external writes are blocked)" : ""}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          <Switch checked={s.enabled} disabled={busy !== null} onChange={(on) => void save({ ...withAllow, enabled: on })} label={`${view.name} enabled`} />
          <Button size="sm" variant="secondary" disabled={busy !== null} onClick={() => void run(true)}>
            {busy === "dry" ? "Checking…" : "Dry run"}
          </Button>
          <Button size="sm" variant="secondary" disabled={busy !== null || !s.enabled || s.allow.length === 0} onClick={() => void run(false)}>
            {busy === "run" ? "Running…" : "Run now"}
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
            Settings
          </Button>
        </div>
      </div>

      {open && (
        <div className="mt-3 grid gap-3 border-t border-border pt-3 text-small-lg sm:grid-cols-2">
          <label className="sm:col-span-2">
            <span className="text-muted">Opted-in orgs and repositories (empty: nothing runs)</span>
            <input className={cx(inputClass, "mt-1")} value={allowText} placeholder="acme, globex/api" onChange={(e) => setAllowText(e.target.value)} aria-label="allowlist" />
          </label>
          <label>
            <span className="text-muted">How often</span>
            <select className={cx(inputClass, "mt-1")} value={cadenceChoice(draft.cadence)} onChange={(e) => setDraft({ ...draft, cadence: cadenceFor(e.target.value as CadenceChoice, draft.cadence) })} aria-label="cadence">
              <option value="hourly">Hourly</option>
              <option value="6h">Every 6 hours</option>
              <option value="daily">Daily</option>
              <option value="weekly">Weekly</option>
              {cadenceChoice(draft.cadence) === "custom" && <option value="custom">{describeLoopCadence(draft.cadence)}</option>}
            </select>
          </label>
          <label>
            <span className="text-muted">Dispatch findings from</span>
            <select className={cx(inputClass, "mt-1")} value={draft.min_severity} onChange={(e) => setDraft({ ...draft, min_severity: e.target.value as SupplySeverity })} aria-label="minimum severity">
              {SEVERITIES.map((sev) => (
                <option key={sev} value={sev}>
                  {sev} and worse
                </option>
              ))}
            </select>
          </label>
          <label>
            <span className="text-muted">Colonies per repository per run</span>
            <input type="number" min={1} max={5} className={cx(inputClass, "mt-1")} value={draft.max_per_repo} onChange={(e) => setDraft({ ...draft, max_per_repo: Number(e.target.value) })} aria-label="per repository" />
          </label>
          <label>
            <span className="text-muted">Colonies per run</span>
            <input type="number" min={1} max={10} className={cx(inputClass, "mt-1")} value={draft.max_per_run} onChange={(e) => setDraft({ ...draft, max_per_run: Number(e.target.value) })} aria-label="per run" />
          </label>
          <label>
            <span className="text-muted">Cooldown per repository (hours)</span>
            <input type="number" min={1} max={720} className={cx(inputClass, "mt-1")} value={draft.cooldown_hours} onChange={(e) => setDraft({ ...draft, cooldown_hours: Number(e.target.value) })} aria-label="cooldown" />
          </label>
          <div className="flex flex-col gap-1.5">
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={draft.outdated} onChange={(e) => setDraft({ ...draft, outdated: e.target.checked })} /> Report outdated direct dependencies (majors behind)
            </label>
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={draft.builtin} onChange={(e) => setDraft({ ...draft, builtin: e.target.checked })} /> Use the built-in OSV lookup where no scanner is installed
            </label>
          </div>
          <div className="text-muted sm:col-span-2">
            Scanners on this host: {Object.entries(view.scanners).map(([name, on]) => `${name} ${on ? "✓" : "✗"}`).join(" · ")}
            {missing.length > 0 ? " — Colonizer never installs one; the report says what each lockfile needs." : ""}
          </div>
          <div className="sm:col-span-2">
            <Button size="sm" variant="primary" disabled={busy !== null} onClick={() => void save(withAllow)}>
              {busy === "save" ? "Saving…" : "Save"}
            </Button>
          </div>
        </div>
      )}

      {view.attention.length > 0 && !dry && (
        <p className="mt-2 text-small-lg text-text">
          ⚠ {view.attention.length} critical or high finding{view.attention.length === 1 ? "" : "s"} with no fixed version need{view.attention.length === 1 ? "s" : ""} a person.
        </p>
      )}
      {shown && <SupplyReport report={shown} onOpenColony={onOpenColony} />}
      {view.history.length > 1 && (
        <details className="mt-2 text-small-lg text-muted">
          <summary className="cursor-pointer">History ({view.history.length} runs)</summary>
          <ul className="m-0 mt-1 list-none space-y-0.5 p-0">
            {view.history.map((h) => (
              <li key={h.id}>
                {relative(h.at)} · {h.trigger} · {h.summary}
              </li>
            ))}
          </ul>
        </details>
      )}
    </section>
  );
}
