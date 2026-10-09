// The built-in "Merge train" loop on the Loops page (issue #754): off by default, hourly, and only
// in the repositories switched on here. The card holds its switch, the per-repository opt-in with
// caps, the careful limits (cooldown, CI wait, known-flaky checks, self-heal, revert, redo), a dry
// run, and the last run's report — merged, updated, red, redo dispatched, skipped, each with its why.
import { useCallback, useEffect, useState, type ReactElement } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { Badge, Button, Switch, cx, type Tone } from "../components/ui";
import { RepoMultiSelect } from "../components/RepoMultiSelect";
import { Disclosure, DetailSection, GroupedDetails, LoopCard, scheduleLine } from "./LoopCard";
import { IconMerge } from "./loopIcons";
import { BUILTIN_HISTORY_ID, plural, type DetailGroupDef, type DetailItem } from "./loopHistory";
import type { MergeLoopAction, MergeLoopReport, MergeLoopSettings, MergeLoopView, Repo } from "../types";
import { intervalWords, relative } from "./loops";
import { actionLabel, parseNames, reportRows, repoOptIn, setRepoCap, setRepoNever, setRepoOptIn, toggleRepos } from "./mergeLoop";

const TONE: Record<MergeLoopAction, Tone> = {
  merged: "ok",
  updated: "info",
  rebased: "info",
  rerun: "info",
  redo_dispatched: "accent",
  resolving: "accent",
  fixing: "accent",
  red: "err",
  needs_redo: "warn",
  waiting: "neutral",
  skipped: "neutral",
};

const field = "rounded-md border border-border bg-panel px-2 py-1 text-small-lg text-text outline-none focus:border-accent";

/** The groups a merge run's pull requests file under, one per action, in the order that matters. */
function mergeGroups(dry: boolean): DetailGroupDef[] {
  const order: MergeLoopAction[] = ["merged", "updated", "rebased", "resolving", "fixing", "rerun", "redo_dispatched", "red", "needs_redo", "waiting", "skipped"];
  return order.map((a) => {
    const label = actionLabel(a, dry);
    return { key: a, label: label.charAt(0).toUpperCase() + label.slice(1), tone: TONE[a] === "neutral" ? "neutral" : TONE[a] };
  });
}

/** A run's report: the counts line, why it stopped if it did, then its pull requests grouped by what happened, identical reasons on one line. */
export function MergeLoopReportView({ report, now }: { report: MergeLoopReport; now?: number }): ReactElement {
  const rows = reportRows(report);
  const items: DetailItem[] = rows.map((r) => ({ group: r.action, repo: r.repo, ref: { text: r.pr, url: r.pr_url }, title: r.title, reason: r.reason }));
  return (
    <div>
      <p className="m-0 flex flex-wrap items-center gap-x-2 gap-y-1 text-small-lg text-muted">
        <span className="font-medium text-text">{report.dry_run ? "Last dry run" : "Last run"}</span>
        {report.finished_at && <span>{relative(report.finished_at, now)}</span>}
        <span>· {report.summary}</span>
      </p>
      {report.forced_dry_run && <p className="m-0 mt-2 text-small-lg text-warn">External writes are blocked (COLONIZER_NO_EXTERNAL_EFFECTS), so the run only looked.</p>}
      {report.repos
        .filter((r) => r.paused || r.heal.length > 0)
        .map((r) => (
          <p key={r.repo} className="m-0 mt-2 text-small-lg text-warn">
            {r.repo}: {r.paused ? `paused — ${r.paused}` : r.heal.join(" · ")}
          </p>
        ))}
      {rows.length > 0 && (
        <div className="mt-3">
          <GroupedDetails items={items} defs={mergeGroups(report.dry_run)} unit={{ one: "PR", many: "PRs" }} />
        </div>
      )}
    </div>
  );
}

function NumberField({ label, value, min, max, onChange, unit }: { label: string; value: number; min: number; max: number; unit?: string; onChange: (n: number) => void }) {
  return (
    <label className="flex items-center gap-2 text-small-lg text-muted">
      {label}
      <input type="number" min={min} max={max} value={value} onChange={(e) => onChange(Number(e.target.value))} className={cx(field, "w-20")} />
      {unit}
    </label>
  );
}

/** The loop's card, driven by props so it renders without a mothership. */
export function MergeLoopPanel({
  view,
  draft,
  repoNames,
  dirty,
  busy,
  now,
  onChange,
  onSave,
  onRun,
  open,
}: {
  view: MergeLoopView;
  draft: MergeLoopSettings;
  repoNames: readonly string[];
  dirty: boolean;
  busy: boolean;
  now?: number;
  onChange: (next: MergeLoopSettings) => void;
  onSave: () => void;
  onRun: (dryRun: boolean) => void;
  /** Start with the detail drawer open (tests, links). */
  open?: boolean;
}): ReactElement {
  const minutes = draft.cadence.every === "interval" ? draft.cadence.minutes : 60;
  const opted = toggleRepos(draft, repoNames).filter((r) => repoOptIn(draft, r) === "on" || repoOptIn(draft, r) === "org").length;
  const ready = opted > 0;
  const attention = view.writes_blocked ? "External writes are blocked: every run is a dry run." : null;
  return (
    <LoopCard
      historyId={BUILTIN_HISTORY_ID.mergeTrain}
      icon={<IconMerge />}
      name="Merge train"
      purpose="Merges colony pull requests one at a time, only on a green main with fresh CI."
      enabled={draft.enabled}
      onToggle={(on) => onChange({ ...draft, enabled: on })}
      running={view.running}
      schedule={scheduleLine(`every ${intervalWords(minutes)}`, draft.enabled, view.next_run_at, ready)}
      scope={{ text: ready ? plural(opted, "repository", "repositories") : "Not set up: add a repository", ready }}
      attention={attention}
      defaultOpen={open}
      refreshKey={view.last_report?.finished_at}
      actions={
        <>
          <Button size="sm" variant="secondary" disabled={busy} onClick={() => onRun(true)}>
            Dry run
          </Button>
          <Button size="sm" variant="secondary" disabled={busy || view.running || dirty} onClick={() => onRun(false)}>
            Run now
          </Button>
          {dirty && (
            <Button size="sm" variant="primary" disabled={busy} onClick={onSave}>
              Save changes
            </Button>
          )}
        </>
      }
    >
      <DetailSection title="Last run">
        {view.last_report ? <MergeLoopReportView report={view.last_report} now={now} /> : <p className="m-0 text-body-sm text-faint">Not run yet. A dry run lists what it would merge, update, rebase and skip, and why.</p>}
      </DetailSection>
      <DetailSection title="Settings">
        <div className="space-y-3">
          <Disclosure title={`Repositories · ${opted} on`} defaultOpen={!ready}>
            <div className="mb-3 grid gap-2 text-small-lg text-muted sm:grid-cols-2">
              <div className="sm:col-span-2">
                Opted-in repositories and orgs (empty: the loop merges nowhere)
                <div className="mt-1">
                  <RepoMultiSelect label="merge train repositories" value={draft.allow} onChange={(allow) => onChange({ ...draft, allow })} disabled={busy} placeholder="Choose repositories or orgs" />
                </div>
              </div>
              <div>
                Never merge in
                <div className="mt-1">
                  <RepoMultiSelect label="never merge in" allowAll={false} value={draft.never} onChange={(never) => onChange({ ...draft, never })} disabled={busy} placeholder="None" />
                </div>
              </div>
              <div>
                Local checks when CI cannot run
                <div className="mt-1">
                  <RepoMultiSelect label="local checks in" value={draft.local_checks} onChange={(local_checks) => onChange({ ...draft, local_checks })} disabled={busy} placeholder="None" />
                </div>
              </div>
            </div>
            <ul className="m-0 list-none divide-y divide-border p-0">
              {toggleRepos(draft, repoNames).map((repo) => {
                const state = repoOptIn(draft, repo);
                return (
                  <li key={repo} className="flex flex-wrap items-center gap-3 py-2 text-small-lg">
                    <Switch checked={state === "on" || state === "org"} disabled={state === "org" || state === "never"} onChange={(on) => onChange(setRepoOptIn(draft, repo, on))} label={`Merge train in ${repo}`} />
                    <span className="min-w-0 flex-1 truncate font-mono text-small text-text">{repo}</span>
                    {state === "org" && <span className="text-faint">on for its org</span>}
                    <label className="flex items-center gap-1 text-muted">
                      cap
                      <input
                        type="number"
                        min={1}
                        max={20}
                        placeholder={String(draft.max_merges)}
                        value={draft.repo_max_merges[repo] ?? ""}
                        onChange={(e) => onChange(setRepoCap(draft, repo, e.target.value === "" ? null : Number(e.target.value)))}
                        className={cx(field, "w-14")}
                      />
                    </label>
                    <label className="flex items-center gap-1 text-muted" title="Upstream review only: the train never merges here">
                      <input type="checkbox" checked={state === "never"} onChange={(e) => onChange(setRepoNever(draft, repo, e.target.checked))} />
                      never
                    </label>
                    {view.repos[repo]?.paused && <Badge tone="warn">paused</Badge>}
                  </li>
                );
              })}
            </ul>
          </Disclosure>
          <Disclosure title="Limits and safety nets">
            <div className="flex flex-wrap gap-x-5 gap-y-2">
              <NumberField label="Every" value={minutes} min={15} max={10080} unit="min" onChange={(n) => onChange({ ...draft, cadence: { every: "interval", minutes: n } })} />
              <NumberField label="Merges per run" value={draft.max_merges} min={1} max={20} onChange={(n) => onChange({ ...draft, max_merges: n })} />
              <NumberField label="Cooldown" value={draft.cooldown_secs} min={30} max={3600} unit="s" onChange={(n) => onChange({ ...draft, cooldown_secs: n })} />
              <NumberField label="Wait for CI" value={draft.ci_wait_minutes} min={1} max={120} unit="min" onChange={(n) => onChange({ ...draft, ci_wait_minutes: n })} />
              <NumberField label="Fix attempts" value={draft.fix_attempts} min={1} max={10} onChange={(n) => onChange({ ...draft, fix_attempts: n })} />
            </div>
            <label className="mt-3 flex items-center gap-2 text-small-lg text-muted">
              Known-flaky checks
              <input
                type="text"
                placeholder="e2e*, lint"
                defaultValue={draft.flaky_checks.join(", ")}
                onBlur={(e) => onChange({ ...draft, flaky_checks: parseNames(e.target.value) })}
                className={cx(field, "min-w-0 flex-1")}
              />
            </label>
            <div className="mt-3 flex flex-col gap-2 text-small-lg text-muted">
              {(
                [
                  ["self_heal", "Self-heal a red main (re-run once, then a fix colony)"],
                  ["revert_on_red", "…by reverting the train's own merge instead"],
                  ["redo_on_conflict", "Redo colony for a conflicting rebase"],
                  ["resolve_conflicts", "Resolve conflicts with the colony (merge main in, never rebase)"],
                  ["fix_red", "Fix red pull requests with the colony (resume it on the failing checks)"],
                ] as const
              ).map(([key, text]) => (
                <label key={key} className="flex items-center gap-2">
                  <Switch
                    checked={draft[key]}
                    disabled={key === "revert_on_red" && !draft.self_heal}
                    onChange={(on) => onChange({ ...draft, [key]: on, ...(key === "self_heal" && !on ? { revert_on_red: false } : {}) })}
                    label={text}
                  />
                  {text}
                </label>
              ))}
            </div>
          </Disclosure>
          {dirty && (
            <Button size="sm" variant="primary" disabled={busy} onClick={onSave}>
              Save changes
            </Button>
          )}
        </div>
      </DetailSection>
    </LoopCard>
  );
}

/** Fetches the loop, keeps an unsaved draft of its settings, and runs it. */
export function MergeLoopCard({ repos }: { repos: readonly Repo[] }): ReactElement | null {
  const api = useApi();
  const toast = useToast();
  const [view, setView] = useState<MergeLoopView | null>(null);
  const [draft, setDraft] = useState<MergeLoopSettings | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    api.mergeLoop().then(
      (v) => {
        setView(v);
        setDraft((d) => d ?? v.settings);
      },
      () => setView(null),
    );
  }, [api]);
  useEffect(() => {
    load();
    const t = setInterval(load, 30_000);
    return () => clearInterval(t);
  }, [load]);

  if (!view || !draft) return null;
  const dirty = JSON.stringify(draft) !== JSON.stringify(view.settings);

  const save = async () => {
    setBusy(true);
    try {
      const saved = await api.saveMergeLoop(draft);
      setView(saved);
      setDraft(saved.settings);
      toast(saved.settings.enabled ? "Merge train loop saved" : "Merge train loop is off");
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const run = async (dryRun: boolean) => {
    setBusy(true);
    try {
      const answer = await api.runMergeLoop(dryRun);
      if (answer.report) {
        setView({ ...view, last_report: answer.report, history: [answer.report, ...view.history] });
        toast(answer.report.summary);
      } else {
        toast("Merge train run started");
        load();
      }
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <MergeLoopPanel
      view={view}
      draft={draft}
      repoNames={repos.map((r) => r.full_name)}
      dirty={dirty}
      busy={busy}
      now={Date.now()}
      onChange={setDraft}
      onSave={() => void save()}
      onRun={(dry) => void run(dry)}
    />
  );
}
