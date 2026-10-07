// The built-in Docs & README loop on the Loops page (docs_loop.rs): which repositories and orgs it
// runs on, how often, Run now / Dry run, and the last run's report — findings, and what was
// dispatched or skipped and why.
import { useCallback, useEffect, useState, type ReactElement } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { Button, Spinner, cx, inputClass } from "../components/ui";
import { DetailSection, GroupedDetails, LoopCard, scheduleLine } from "./LoopCard";
import { IconBook } from "./loopIcons";
import { BUILTIN_HISTORY_ID, type DetailGroupDef, type DetailItem } from "./loopHistory";
import {
  DOCS_INTERVALS,
  DOCS_KIND_LABEL,
  allowEntryError,
  describeInterval,
  findingPlace,
  summarizeReport,
  type DocsLoopSettings,
  type DocsLoopView,
  type DocsReport,
} from "./docsLoop";
import { relative } from "./loops";

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
  open,
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
  /** Start with the detail drawer open (tests, links). */
  open?: boolean;
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
  const ready = settings.allow.length > 0;
  return (
    <LoopCard
      historyId={BUILTIN_HISTORY_ID.docs}
      icon={<IconBook />}
      name={view.name}
      purpose="Finds docs that fell behind the code, with no model, and sends one docs-only colony per repository."
      enabled={view.enabled}
      running={false}
      schedule={scheduleLine(describeInterval(settings.interval_hours), view.enabled, view.next_run_at, ready, now)}
      scope={{ text: ready ? settings.allow.join(", ") : "Not set up: add a repository or an org", ready }}
      defaultOpen={open}
      refreshKey={view.last_report?.id}
      onOpenColony={onOpenColony}
      actions={
        <>
          <Button size="sm" variant="secondary" disabled={!view.enabled || busy} onClick={() => onRun(true)} title="Report the findings; dispatch and record nothing">
            Dry run
          </Button>
          <Button size="sm" variant="secondary" disabled={!view.enabled || busy} onClick={() => onRun(false)}>
            Run now
          </Button>
        </>
      }
    >
      <DetailSection title={dryRun ? "Dry run" : "Last run"}>
        {report ? <DocsReportView report={report} now={now} onOpenColony={onOpenColony} /> : <p className="m-0 text-body-sm text-faint">{view.enabled ? "No run yet." : "Off until you add a repository or org."}</p>}
      </DetailSection>
      <DetailSection title="Settings">
        <div className="space-y-4">
          <div>
            <div className="mb-1.5 text-small-lg text-muted">Runs on</div>
            <div className="flex flex-wrap items-center gap-1.5" aria-label="Runs on">
              {settings.allow.map((a) => (
                <span key={a} className="inline-flex items-center gap-1 rounded-full border border-border bg-panel px-2.5 py-1 font-mono text-meta-lg text-text">
                  {a}
                  <button type="button" className="cursor-pointer border-0 bg-transparent p-0 text-faint hover:text-text" aria-label={`stop running on ${a}`} disabled={busy} onClick={() => onTarget(a, false)}>
                    ✕
                  </button>
                </span>
              ))}
            </div>
            <form
              className="mt-2 flex flex-wrap items-center gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                add();
              }}
            >
              <input className={cx(inputClass, "w-56")} placeholder="owner or owner/name" aria-label="repository or org to run on" value={entry} onChange={(e) => setEntry(e.target.value)} />
              <Button size="sm" variant="primary" type="submit" disabled={busy}>
                Enable
              </Button>
              {entryError && <span className="text-small text-err">{entryError}</span>}
            </form>
          </div>
          <div className="flex flex-wrap items-center gap-x-5 gap-y-2 text-small-lg text-muted">
            <label className="flex items-center gap-2">
              Runs
              <select className={cx(inputClass, "w-auto")} value={settings.interval_hours} disabled={busy} onChange={(e) => onSave({ ...settings, interval_hours: Number(e.target.value) })}>
                {DOCS_INTERVALS.some((i) => i.hours === settings.interval_hours) ? null : <option value={settings.interval_hours}>{describeInterval(settings.interval_hours)}</option>}
                {DOCS_INTERVALS.map((i) => (
                  <option key={i.hours} value={i.hours}>
                    {i.label}
                  </option>
                ))}
              </select>
            </label>
            <label className="flex items-center gap-2">
              Cooldown after a dispatch
              <select className={cx(inputClass, "w-auto")} value={settings.cooldown_hours} disabled={busy} onChange={(e) => onSave({ ...settings, cooldown_hours: Number(e.target.value) })}>
                {[6, 12, 24, 48, 168].includes(settings.cooldown_hours) ? null : <option value={settings.cooldown_hours}>{settings.cooldown_hours} hours</option>}
                {[6, 12, 24, 48, 168].map((h) => (
                  <option key={h} value={h}>
                    {h === 168 ? "1 week" : `${h} hours`}
                  </option>
                ))}
              </select>
            </label>
          </div>
        </div>
      </DetailSection>
    </LoopCard>
  );
}

const DOCS_GROUPS: DetailGroupDef[] = [
  { key: "dispatched", label: "Colony dispatched", tone: "accent" },
  { key: "error", label: "Failed", tone: "err" },
  { key: "report_only", label: "Report only", tone: "info" },
  { key: "skipped", label: "Skipped", tone: "neutral" },
  { key: "clean", label: "Clean", tone: "ok" },
];

/** One run's report: a summary, repositories grouped by what happened (identical reasons on one line), then the findings. */
export function DocsReportView({ report, now = Date.now(), onOpenColony }: { report: DocsReport; now?: number; onOpenColony: (id: string) => void }): ReactElement {
  const items: DetailItem[] = report.repos.map((r) => ({ group: r.action, repo: r.repo, reason: r.reason, colony: r.colony ?? undefined }));
  const withFindings = report.repos.filter((r) => r.findings.length > 0);
  return (
    <div className="space-y-3">
      <div className="text-small-lg text-muted">
        <span className="font-medium text-text">{report.dry_run ? "Dry run" : "Last run"}</span> {relative(report.at, now)} · {summarizeReport(report)}
      </div>
      <GroupedDetails items={items} defs={DOCS_GROUPS} unit={{ one: "repository", many: "repositories" }} onOpenColony={onOpenColony} />
      {withFindings.length > 0 && (
        <details className="overflow-hidden rounded-xl border border-border bg-panel">
          <summary className="cursor-pointer list-none px-3.5 py-2.5 text-body font-medium text-text [&::-webkit-details-marker]:hidden">
            Findings <span className="rounded-full bg-panel-2 px-2 text-small tabular-nums text-muted">{withFindings.reduce((n, r) => n + r.findings.length + r.more, 0)}</span>
          </summary>
          <ul className="m-0 list-none divide-y divide-border border-t border-border p-0">
            {withFindings.map((r) => (
              <li key={r.repo} className="px-3.5 py-2.5 text-small-lg">
                <span className="font-mono text-small text-text">{r.repo}</span>
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
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}
