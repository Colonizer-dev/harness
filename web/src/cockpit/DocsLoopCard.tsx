// The built-in Docs & README loop on the Loops page (docs_loop.rs): which repositories and orgs it
// runs on, how often, Run now / Dry run, and the last run's report — findings, and what was
// dispatched or skipped and why.
import { useCallback, useEffect, useState, type ReactElement } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { Badge, Button, Spinner, cx, inputClass, type Tone } from "../components/ui";
import {
  DOCS_ACTION_LABEL,
  DOCS_INTERVALS,
  DOCS_KIND_LABEL,
  allowEntryError,
  describeInterval,
  findingPlace,
  summarizeReport,
  type DocsAction,
  type DocsLoopSettings,
  type DocsLoopView,
  type DocsReport,
} from "./docsLoop";
import { relative } from "./loops";

const ACTION_TONE: Record<DocsAction, Tone> = {
  clean: "ok",
  dispatched: "accent",
  skipped: "neutral",
  report_only: "info",
  error: "err",
};

/** The card, loading and saving through the API. */
export function DocsLoopCard({ onOpenColony }: { onOpenColony: (id: string) => void }): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [view, setView] = useState<DocsLoopView | null>(null);
  const [dryRun, setDryRun] = useState<DocsReport | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    api.docsLoop().then(setView, (e) => toast(errorMessage(e), "error"));
  }, [api, toast]);
  useEffect(() => load(), [load]);

  const act = async <T,>(work: () => Promise<T>, then: (value: T) => void) => {
    setBusy(true);
    try {
      then(await work());
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };

  if (!view) {
    return (
      <p className="mt-6 flex items-center gap-2 text-body-sm text-muted">
        <Spinner /> Loading the Docs &amp; README loop…
      </p>
    );
  }
  return (
    <DocsLoopPanel
      view={view}
      dryRun={dryRun}
      busy={busy}
      onOpenColony={onOpenColony}
      onTarget={(target, enabled) => void act(() => api.setDocsLoopTarget(target, enabled), setView)}
      onSave={(settings) => void act(() => api.saveDocsLoop(settings), setView)}
      onRun={(dry) =>
        void act(
          () => api.runDocsLoop(dry),
          (report) => {
            if (dry) setDryRun(report);
            else {
              setDryRun(null);
              toast(`Docs & README: ${summarizeReport(report)}`);
              load();
            }
          },
        )
      }
    />
  );
}

/** The card itself, from what the API answered. Presentational, so it renders in tests. */
export function DocsLoopPanel({
  view,
  dryRun,
  busy,
  now = Date.now(),
  onTarget,
  onSave,
  onRun,
  onOpenColony,
}: {
  view: DocsLoopView;
  /** The last dry run's report: shown, never stored. */
  dryRun: DocsReport | null;
  busy: boolean;
  now?: number;
  onTarget: (target: string, enabled: boolean) => void;
  onSave: (settings: DocsLoopSettings) => void;
  onRun: (dryRun: boolean) => void;
  onOpenColony: (id: string) => void;
}): ReactElement {
  const [entry, setEntry] = useState("");
  const [touched, setTouched] = useState(false);
  const entryError = touched ? allowEntryError(entry) : null;
  const { settings } = view;
  const add = () => {
    setTouched(true);
    if (allowEntryError(entry)) return;
    onTarget(entry.trim(), true);
    setEntry("");
    setTouched(false);
  };
  const report = dryRun ?? view.last_report;
  return (
    <section className="mt-6 rounded-xl border border-border p-4" aria-label="Docs & README loop">
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1 basis-72">
          <div className="flex items-center gap-2">
            <h2 className="m-0 text-lead font-semibold text-text">{view.name}</h2>
            <Badge tone="neutral">built in</Badge>
            <Badge tone={view.enabled ? "ok" : "neutral"}>{view.enabled ? "On" : "Off"}</Badge>
          </div>
          <p className="mt-1 text-small-lg text-muted">
            Finds docs that fell behind the code — merged changes their docs never caught up with, broken links and anchors, commands that are gone, changelog gaps — without a model, then sends one docs-only colony per repository.
            {view.enabled
              ? ` Runs ${describeInterval(settings.interval_hours)}${view.next_run_at ? `, next ${relative(view.next_run_at, now)}` : ""}.`
              : " Off until you add a repository or org."}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          <Button size="sm" variant="secondary" disabled={!view.enabled || busy} onClick={() => onRun(false)}>
            Run now
          </Button>
          <Button size="sm" variant="ghost" disabled={!view.enabled || busy} onClick={() => onRun(true)} title="Report the findings; dispatch and record nothing">
            Dry run
          </Button>
        </div>
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-1.5" aria-label="Runs on">
        {settings.allow.map((a) => (
          <span key={a} className="inline-flex items-center gap-1 rounded-full border border-border px-2 py-0.5 font-mono text-meta-lg text-text">
            {a}
            <button type="button" className="cursor-pointer border-0 bg-transparent p-0 text-faint hover:text-text" aria-label={`stop running on ${a}`} disabled={busy} onClick={() => onTarget(a, false)}>
              ✕
            </button>
          </span>
        ))}
        <form
          className="flex items-center gap-1.5"
          onSubmit={(e) => {
            e.preventDefault();
            add();
          }}
        >
          <input
            className={cx(inputClass, "h-7 w-48 py-0 text-small-lg")}
            placeholder="owner or owner/name"
            aria-label="repository or org to run on"
            value={entry}
            onChange={(e) => setEntry(e.target.value)}
          />
          <Button size="sm" variant="primary" type="submit" disabled={busy}>
            Enable
          </Button>
        </form>
        {entryError && <span className="text-small text-err">{entryError}</span>}
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-3 text-small-lg text-muted">
        <label className="flex items-center gap-1.5">
          Runs
          <select
            className={cx(inputClass, "h-7 w-auto py-0 text-small-lg")}
            value={settings.interval_hours}
            disabled={busy}
            onChange={(e) => onSave({ ...settings, interval_hours: Number(e.target.value) })}
          >
            {DOCS_INTERVALS.some((i) => i.hours === settings.interval_hours) ? null : <option value={settings.interval_hours}>{describeInterval(settings.interval_hours)}</option>}
            {DOCS_INTERVALS.map((i) => (
              <option key={i.hours} value={i.hours}>
                {i.label}
              </option>
            ))}
          </select>
        </label>
        <label className="flex items-center gap-1.5">
          Cooldown after a dispatch
          <select
            className={cx(inputClass, "h-7 w-auto py-0 text-small-lg")}
            value={settings.cooldown_hours}
            disabled={busy}
            onChange={(e) => onSave({ ...settings, cooldown_hours: Number(e.target.value) })}
          >
            {[6, 12, 24, 48, 168].includes(settings.cooldown_hours) ? null : <option value={settings.cooldown_hours}>{settings.cooldown_hours} hours</option>}
            {[6, 12, 24, 48, 168].map((h) => (
              <option key={h} value={h}>
                {h === 168 ? "1 week" : `${h} hours`}
              </option>
            ))}
          </select>
        </label>
      </div>

      {report ? <DocsReportView report={report} now={now} onOpenColony={onOpenColony} /> : view.enabled ? <p className="mt-3 text-small-lg text-faint">No run yet.</p> : null}
    </section>
  );
}

/** One run's report: a summary, then each repository's action, reason and findings. */
export function DocsReportView({ report, now = Date.now(), onOpenColony }: { report: DocsReport; now?: number; onOpenColony: (id: string) => void }): ReactElement {
  return (
    <div className="mt-4 border-t border-border pt-3">
      <div className="text-small-lg text-muted">
        <span className="font-medium text-text">{report.dry_run ? "Dry run" : "Last run"}</span> {relative(report.at, now)} · {summarizeReport(report)}
      </div>
      <ul className="m-0 mt-2 list-none space-y-2 p-0">
        {report.repos.map((r) => (
          <li key={r.repo} className="text-small-lg">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-mono text-small text-text">{r.repo}</span>
              <Badge tone={ACTION_TONE[r.action]}>{DOCS_ACTION_LABEL[r.action]}</Badge>
              <span className="text-muted">{r.reason}</span>
              {r.colony && (
                <button type="button" className="cursor-pointer border-0 bg-transparent p-0 text-accent hover:underline" onClick={() => onOpenColony(r.colony!)}>
                  Open colony
                </button>
              )}
            </div>
            {r.findings.length > 0 && (
              <ul className="m-0 mt-1 list-disc space-y-0.5 pl-5 text-muted">
                {r.findings.map((f, i) => (
                  <li key={i}>
                    <span className="text-text">{DOCS_KIND_LABEL[f.kind]}</span>
                    {findingPlace(f) ? <span className="font-mono text-meta-lg text-faint"> {findingPlace(f)}</span> : null}
                    {f.advisory ? <span className="text-faint"> (advisory)</span> : null} — {f.message}
                  </li>
                ))}
                {r.more > 0 && <li>…and {r.more} more</li>}
              </ul>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}
