// The built-in "TypeScript: remove any" loop (ts_any_loop.rs), shown on the Loops page: its switch
// and allowlist (both off/empty until the operator opts in), its cadence, batch size and rate
// limits, a small trend of the explicit-any total, and the last run's report — the count and how it
// was made, the busiest modules, what it dispatched, what it skipped and why, and any published
// batch whose recount needs a person. A dry run shows the same report without starting or saving
// anything.
import { useCallback, useEffect, useState, type ReactElement } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { Badge, Button, Spinner, Switch, cx, inputClass } from "../components/ui";
import type { LoopCadence, TsAnyLoop as LoopView, TsAnyReport, TsAnyRun, TsAnySettings } from "../types";
import { describeLoopCadence, relative } from "./loops";

export const TS_ANY_ORIGIN = "ts-any:";

/** The allowlist as typed: comma, space or newline separated, trimmed, without duplicates. */
export function parseTsAnyAllow(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(/[\s,]+/)) {
    const entry = raw.trim().replace(/\/\*$/, "");
    if (entry && !out.some((x) => x.toLowerCase() === entry.toLowerCase())) out.push(entry);
  }
  return out;
}

export type TsAnyCadenceChoice = "hourly" | "6h" | "daily" | "weekly" | "custom";

/** Which of the offered cadences a saved one is; anything else reads as custom (kept as saved). */
export function tsAnyCadenceChoice(c: LoopCadence): TsAnyCadenceChoice {
  if (c.every === "interval") return c.minutes === 60 ? "hourly" : c.minutes === 360 ? "6h" : "custom";
  if (c.every === "daily") return "daily";
  if (c.every === "weekly") return "weekly";
  return "custom";
}

/** The cadence for a choice; `custom` keeps the saved one. Times are UTC, off the hour. */
export function tsAnyCadenceFor(choice: TsAnyCadenceChoice, saved: LoopCadence): LoopCadence {
  switch (choice) {
    case "hourly":
      return { every: "interval", minutes: 60 };
    case "6h":
      return { every: "interval", minutes: 360 };
    case "daily":
      return saved.every === "daily" ? saved : { every: "daily", hour: 7, minute: 43 };
    case "weekly":
      return saved.every === "weekly" ? saved : { every: "weekly", weekday: 0, hour: 7, minute: 43 };
    default:
      return saved;
  }
}

/** The totals of the last runs, oldest first (the history is newest first). */
export function trendOf(history: TsAnyRun[], limit = 20): number[] {
  return history
    .slice(0, limit)
    .map((h) => h.total)
    .reverse();
}

/** "−4 since the last run", "unchanged", or "" with nothing to compare. */
export function deltaLine(total: number, previous: number[]): string {
  if (previous.length === 0) return "";
  const d = total - previous[0];
  if (d === 0) return "unchanged since the last run";
  return `${d > 0 ? "+" : "−"}${Math.abs(d)} since the last run`;
}

/** A tiny line of the totals: enough to see the direction. */
export function Sparkline({ values, width = 120, height = 28 }: { values: number[]; width?: number; height?: number }): ReactElement | null {
  if (values.length < 2) return null;
  const max = Math.max(...values);
  const min = Math.min(...values);
  const span = max - min || 1;
  const step = width / (values.length - 1);
  const points = values.map((v, i) => `${(i * step).toFixed(1)},${(height - 2 - ((v - min) / span) * (height - 4)).toFixed(1)}`).join(" ");
  const falling = values[values.length - 1] <= values[0];
  return (
    <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} role="img" aria-label={`trend: ${values.join(", ")}`} className="shrink-0">
      <polyline points={points} fill="none" strokeWidth={1.5} stroke={falling ? "var(--ok, #16a34a)" : "var(--warn, #d97706)"} />
    </svg>
  );
}

/** A run's report: the counts, the busiest modules, what was dispatched and skipped, the recounts. */
export function TsAnyReportView({ report, now = Date.now(), onOpenColony }: { report: TsAnyReport; now?: number; onOpenColony?: (id: string) => void }): ReactElement {
  const counted = report.repos.filter((r) => r.typescript);
  return (
    <div className="mt-3 space-y-3 text-small-lg">
      <div className="flex flex-wrap items-center gap-2 text-muted">
        <span className="font-medium text-text">{report.dry_run ? "Dry run" : "Last run"}</span>
        <span>{relative(report.finished_at, now)}</span>
        <span>· {report.trigger}</span>
        {report.blocked && <Badge tone="warn">report only: external writes are blocked</Badge>}
        <Badge tone={report.total > 0 ? "warn" : "ok"}>{report.total} explicit any</Badge>
        {counted.length === 0 && <span>· no TypeScript repository counted</span>}
      </div>
      {report.note && <p className="m-0 text-muted">{report.note}</p>}
      {report.attention.length > 0 && (
        <div className="rounded-lg border border-[var(--err,#dc2626)] px-3 py-2">
          <div className="font-medium text-text">Needs attention</div>
          <ul className="m-0 mt-1 list-none space-y-1 p-0">
            {report.attention.map((a) => (
              <li key={a.session}>
                <span className="font-mono">{a.repo}</span> · {a.module} — {a.problems.join("; ")}
                {a.pr_url && (
                  <>
                    {" "}
                    <a href={a.pr_url} target="_blank" rel="noreferrer" className="text-muted underline decoration-dotted">
                      pull request
                    </a>
                  </>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}
      {counted.map((r) => (
        <div key={r.repo}>
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-mono text-text">{r.repo}</span>
            <span className="text-muted">
              {r.total} explicit any
              {r.implicit != null ? ` · ${r.implicit} implicit` : ""} · {r.ts_files} files
            </span>
            {deltaLine(r.total, r.previous) && <span className="text-faint">· {deltaLine(r.total, r.previous)}</span>}
          </div>
          <div className="text-faint">{r.method === "typescript" ? `counted with ${r.method_note}` : r.method_note}</div>
          {r.modules.length > 0 && (
            <table className="mt-1 w-full border-collapse text-left">
              <thead className="text-faint">
                <tr>
                  <th className="py-1 pr-2 font-normal">Module</th>
                  <th className="py-1 pr-2 font-normal">any</th>
                  <th className="py-1 pr-2 font-normal">Files</th>
                </tr>
              </thead>
              <tbody>
                {r.modules.slice(0, 5).map((m) => (
                  <tr key={m.module} className="border-t border-border">
                    <td className="py-1 pr-2 font-mono text-text">{m.module}</td>
                    <td className="py-1 pr-2 text-muted">{m.explicit}</td>
                    <td className="py-1 pr-2 text-muted">{m.files}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {(r.error || r.notes.length > 0) && (
            <ul className="m-0 mt-1 list-none space-y-0.5 p-0 text-muted">
              {[...(r.error ? [r.error] : []), ...r.notes].map((line) => (
                <li key={line}>⚠ {line}</li>
              ))}
            </ul>
          )}
        </div>
      ))}
      {report.dispatched.length > 0 && (
        <div>
          <div className="font-medium text-text">{report.dry_run ? "Would dispatch" : "Dispatched"}</div>
          <ul className="m-0 mt-1 list-none space-y-0.5 p-0 text-muted">
            {report.dispatched.map((d) => (
              <li key={`${d.repo}:${d.module}`}>
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
              <li key={`${s.repo}:${s.module}:${i}`}>
                <span className="font-mono">{s.repo}</span>
                {s.module ? ` (${s.module})` : ""}: {s.reason}
              </li>
            ))}
          </ul>
        </div>
      )}
      {report.checks.length > 0 && (
        <div>
          <div className="font-medium text-text">Recounted after publishing</div>
          <ul className="m-0 mt-1 list-none space-y-0.5 p-0 text-muted">
            {report.checks.map((c) => (
              <li key={c.session}>
                {c.flagged ? "⚠ " : "✓ "}
                <span className="font-mono">{c.repo}</span> ({c.module}): {c.summary}
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

/** The loop's card on the Loops page. */
export function TsAnyLoopCard({ onOpenColony }: { onOpenColony: (id: string) => void }): ReactElement | null {
  const api = useApi();
  const toast = useToast();
  const [view, setView] = useState<LoopView | null>(null);
  const [draft, setDraft] = useState<TsAnySettings | null>(null);
  const [allowText, setAllowText] = useState("");
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState<"save" | "dry" | "run" | null>(null);
  const [dry, setDry] = useState<TsAnyReport | null>(null);

  const load = useCallback(() => {
    api.tsAnyLoop().then(
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

  const save = async (settings: TsAnySettings) => {
    setBusy("save");
    try {
      const v = await api.saveTsAnyLoop(settings);
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
      const report = await api.runTsAnyLoop({ dry_run: dryRun });
      if (dryRun) setDry(report);
      else {
        setDry(null);
        load();
      }
      toast(`${dryRun ? "Dry run" : "Run"}: ${report.total} explicit any; ${dryRun ? "would dispatch" : "dispatched"} ${report.dispatched.length}`);
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(null);
    }
  };

  const s = view.settings;
  const withAllow = { ...draft, allow: parseTsAnyAllow(allowText) };
  const shown = dry ?? view.last_report;
  const trend = trendOf(view.history);

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
                : "off · counts explicit any on the host and hands one small batch per repository to a colony"}
            {view.blocked ? " · report only (external writes are blocked)" : ""}
          </div>
        </div>
        <Sparkline values={trend} />
        <div className="flex shrink-0 items-center gap-1.5">
          <Switch checked={s.enabled} disabled={busy !== null} onChange={(on) => void save({ ...withAllow, enabled: on })} label={`${view.name} enabled`} />
          <Button size="sm" variant="secondary" disabled={busy !== null} onClick={() => void run(true)}>
            {busy === "dry" ? "Counting…" : "Dry run"}
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
            <input className={cx(inputClass, "mt-1")} value={allowText} placeholder="acme, globex/web" onChange={(e) => setAllowText(e.target.value)} aria-label="allowlist" />
          </label>
          <label>
            <span className="text-muted">How often</span>
            <select className={cx(inputClass, "mt-1")} value={tsAnyCadenceChoice(draft.cadence)} onChange={(e) => setDraft({ ...draft, cadence: tsAnyCadenceFor(e.target.value as TsAnyCadenceChoice, draft.cadence) })} aria-label="cadence">
              <option value="hourly">Hourly</option>
              <option value="6h">Every 6 hours</option>
              <option value="daily">Daily</option>
              <option value="weekly">Weekly</option>
              {tsAnyCadenceChoice(draft.cadence) === "custom" && <option value="custom">{describeLoopCadence(draft.cadence)}</option>}
            </select>
          </label>
          <label>
            <span className="text-muted">Occurrences per batch</span>
            <input type="number" min={1} max={100} className={cx(inputClass, "mt-1")} value={draft.batch_cap} onChange={(e) => setDraft({ ...draft, batch_cap: Number(e.target.value) })} aria-label="batch size" />
          </label>
          <label>
            <span className="text-muted">Colonies per run (one per repository at most)</span>
            <input type="number" min={1} max={10} className={cx(inputClass, "mt-1")} value={draft.max_per_run} onChange={(e) => setDraft({ ...draft, max_per_run: Number(e.target.value) })} aria-label="per run" />
          </label>
          <label>
            <span className="text-muted">Cooldown per repository (hours)</span>
            <input type="number" min={1} max={720} className={cx(inputClass, "mt-1")} value={draft.cooldown_hours} onChange={(e) => setDraft({ ...draft, cooldown_hours: Number(e.target.value) })} aria-label="cooldown" />
          </label>
          <div className="flex flex-col gap-1.5">
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={draft.offline_install} onChange={(e) => setDraft({ ...draft, offline_install: e.target.checked })} /> Install the repository's TypeScript offline, from the host's cache
            </label>
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={draft.implicit} onChange={(e) => setDraft({ ...draft, implicit: e.target.checked })} /> Also count implicit any (needs the repository's TypeScript)
            </label>
          </div>
          <div className="text-muted sm:col-span-2">
            {view.node ? "node is on this host: the repository's own TypeScript is used when it can be had offline; otherwise a token scan." : "node is not on this host: every count is a token scan."}
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
          ⚠ {view.attention.length} published batch{view.attention.length === 1 ? "" : "es"} need{view.attention.length === 1 ? "s" : ""} review: {view.attention.map((a) => `${a.repo} ${a.module}`).join(", ")}.
        </p>
      )}
      {shown && <TsAnyReportView report={shown} onOpenColony={onOpenColony} />}
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
