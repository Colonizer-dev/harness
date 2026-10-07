// The built-in Disk cleanup loop on the Loops page: its card, a dry-run preview (shown before the
// first enable), its last run and its settings. The rules live in diskCleanup.ts.
import { useState, type ReactElement } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { Button, Spinner, Switch, cx } from "../components/ui";
import { DetailSection, LoopCard, scheduleLine } from "./LoopCard";
import { IconDisk } from "./loopIcons";
import { BUILTIN_HISTORY_ID } from "./loopHistory";
import type { DiskCleanupReport, DiskCleanupSettings, Loop } from "../types";
import { formatBytes } from "./host";
import { relative } from "./loops";
import {
  DISK_CLEANUP_CATEGORIES,
  DISK_CLEANUP_MIN_MINUTES,
  categoryLabel,
  describeDiskCleanup,
  diskCleanupBody,
  heldReason,
  toggleAction,
  parseExtraPaths,
  reportSummary,
  settingsOf,
} from "./diskCleanup";

/** One report — a dry run or a stored run — category by category, with every path and size. */
export function DiskCleanupReportView({ report }: { report: DiskCleanupReport }): ReactElement {
  return (
    <div className="space-y-3 text-body-sm">
      <p className="font-medium text-text">{report.dry_run ? `A run now ${reportSummary(report)}` : reportSummary(report)}. {report.dry_run ? "Nothing has been removed." : ""}</p>
      {report.categories.map((c) => (
        <section key={c.category} className="rounded-lg border border-border px-3 py-2">
          <div className="flex items-center gap-2">
            <span className="flex-1 font-medium text-text">{categoryLabel(c.category)}</span>
            <span className="text-muted">{c.enabled ? `${c.count} · ${formatBytes(c.bytes)}` : "off"}</span>
          </div>
          {c.items.length > 0 && (
            <ul className="m-0 mt-1.5 list-none space-y-0.5 p-0">
              {c.items.map((item) => (
                <li key={item.path} className="flex gap-3 font-mono text-meta-lg text-muted">
                  <span className="w-14 shrink-0 text-right tabular-nums">{item.bytes == null ? "—" : formatBytes(item.bytes)}</span>
                  <span className="min-w-0 truncate" title={item.path}>
                    {item.path}
                  </span>
                </li>
              ))}
            </ul>
          )}
          {(c.held ?? []).length > 0 && (
            <ul className="m-0 mt-1.5 list-none space-y-0.5 p-0">
              {(c.held ?? []).map((h) => (
                <li key={`${h.path}:${h.reason}`} className="truncate text-meta-lg text-faint" title={h.path}>
                  kept: {h.path} ({heldReason(h.reason)})
                </li>
              ))}
            </ul>
          )}
          {(c.failed ?? []).map((f) => (
            <p key={f} className="m-0 mt-1 text-meta-lg text-err">
              {f}
            </p>
          ))}
          {c.note && <p className="m-0 mt-1 text-meta-lg text-faint">{c.note}</p>}
        </section>
      ))}
      {report.attention && <p className="text-warn">{report.attention}</p>}
    </div>
  );
}

/** The settings form, controlled, so the dialog owns saving. */
export function DiskCleanupSettingsForm({
  settings,
  minutes,
  onChange,
  onMinutes,
}: {
  settings: DiskCleanupSettings;
  minutes: number;
  onChange: (s: DiskCleanupSettings) => void;
  onMinutes: (m: number) => void;
}): ReactElement {
  const field = "rounded-lg border border-border bg-transparent px-2.5 py-1.5 text-body-sm text-text outline-none focus:border-border-strong";
  return (
    <div className="space-y-4 text-body-sm">
      <label className="flex flex-wrap items-center gap-2">
        Run every
        <input type="number" aria-label="minutes between runs" min={DISK_CLEANUP_MIN_MINUTES} max={10080} value={minutes} onChange={(e) => onMinutes(Number(e.target.value))} className={cx(field, "w-24")} />
        minutes (at least {DISK_CLEANUP_MIN_MINUTES}; hourly by default)
      </label>
      <label className="flex flex-wrap items-center gap-2">
        and early when free space is under
        <input type="number" aria-label="free-space trigger percent" min={0} max={90} value={settings.trigger_free_pct} onChange={(e) => onChange({ ...settings, trigger_free_pct: Number(e.target.value) })} className={cx(field, "w-20")} />
        % (0 turns it off)
      </label>
      <fieldset className="space-y-2">
        <legend className="mb-1 text-muted">What it may clean</legend>
        {DISK_CLEANUP_CATEGORIES.map((c) => (
          <div key={c.key} className="flex items-start gap-2">
            <Switch checked={settings[c.key]} onChange={(on) => onChange({ ...settings, [c.key]: on })} label={c.label} />
            <div className="min-w-0">
              <div className="text-text">{c.label}</div>
              <div className="text-small text-faint">{c.hint}</div>
            </div>
          </div>
        ))}
      </fieldset>
      <label className="flex flex-wrap items-center gap-2">
        Stopped or failed colonies' build output waits
        <input type="number" aria-label="days a stopped colony waits" min={1} max={365} value={settings.stopped_after_days} onChange={(e) => onChange({ ...settings, stopped_after_days: Number(e.target.value) })} className={cx(field, "w-20")} />
        days
      </label>
      {settings.archives && (
        <label className="flex flex-wrap items-center gap-2">
          Keep archives
          <input type="number" aria-label="days archives are kept" min={1} value={settings.archive_keep_days} onChange={(e) => onChange({ ...settings, archive_keep_days: Number(e.target.value) })} className={cx(field, "w-20")} />
          days
        </label>
      )}
      {settings.host_paths && (
        <label className="block">
          <span className="mb-1 block text-muted">Directories you build in on this host (one absolute path per line)</span>
          <textarea
            rows={3}
            value={settings.extra_paths.join("\n")}
            onChange={(e) => onChange({ ...settings, extra_paths: parseExtraPaths(e.target.value) })}
            placeholder="/home/you/code"
            className={cx(field, "w-full resize-y font-mono")}
          />
          <span className="mt-1 block text-small text-faint">
            Only Cargo target/ dirs untouched for {settings.host_min_age_days} days go. Never ~/.cargo, caches, .git or anything outside these paths.
          </span>
        </label>
      )}
    </div>
  );
}

/** The loop's card: its switch, a preview before the first enable, its last run and its settings. */
export function DiskCleanupCard({ loop, onChanged }: { loop: Loop; onChanged: () => void }): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [preview, setPreview] = useState<DiskCleanupReport | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [settings, setSettings] = useState<DiskCleanupSettings>(() => settingsOf(loop));
  const [minutes, setMinutes] = useState(loop.cadence.every === "interval" ? loop.cadence.minutes : 60);
  const [busy, setBusy] = useState(false);
  const [openSignal, setOpenSignal] = useState(0);
  const last = loop.disk_cleanup?.history[0];

  const runPreview = async () => {
    setPreviewing(true);
    try {
      setPreview(await api.runDiskCleanup(loop.id, true));
      onChanged();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setPreviewing(false);
    }
  };

  const save = async (change: { enabled?: boolean }) => {
    setBusy(true);
    try {
      await api.updateLoop(loop.id, diskCleanupBody(loop, { ...change, minutes, settings }));
      toast(change.enabled ? "Disk cleanup is on" : change.enabled === false ? "Disk cleanup is off" : "Disk cleanup saved");
      onChanged();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };

  // Turning it on before the owner has seen a dry run shows what it would remove first.
  const toggle = (on: boolean) => {
    if (toggleAction(loop, on) === "preview") {
      setOpenSignal((n) => n + 1);
      void runPreview();
      return;
    }
    void save({ enabled: on });
  };

  const runNow = async () => {
    setBusy(true);
    try {
      const report = await api.runDiskCleanup(loop.id, false);
      toast(`Disk cleanup: ${reportSummary(report)}`);
      onChanged();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <LoopCard
      historyId={BUILTIN_HISTORY_ID.diskCleanup}
      icon={<IconDisk />}
      name={loop.name}
      purpose="Clears build output, finished worktrees and stopped microVMs before the disk fills up."
      enabled={loop.enabled}
      onToggle={toggle}
      busy={busy}
      schedule={scheduleLine(describeDiskCleanup(loop), loop.enabled, loop.next_run_at)}
      scope={{ text: "This computer", ready: true }}
      attention={loop.disk_cleanup?.attention}
      refreshKey={last?.at}
      openSignal={openSignal}
      actions={
        <>
          <Button size="sm" variant="secondary" disabled={busy || previewing} onClick={() => void runPreview()}>
            {previewing ? "Working it out…" : "Preview"}
          </Button>
          <Button size="sm" variant="secondary" disabled={busy || !loop.enabled} onClick={() => void runNow()}>
            Run now
          </Button>
        </>
      }
    >
      {(preview || previewing) && (
        <DetailSection title="Preview" meta="what a run would remove now">
          {preview ? (
            <>
              <DiskCleanupReportView report={preview} />
              {!loop.enabled && (
                <Button className="mt-3" variant="primary" disabled={busy} onClick={() => void save({ enabled: true })}>
                  Turn on disk cleanup
                </Button>
              )}
            </>
          ) : (
            <p className="flex items-center gap-2 text-body-sm text-muted">
              <Spinner /> Working out what a run would remove…
            </p>
          )}
        </DetailSection>
      )}
      <DetailSection title="Last run">
        {last ? (
          <>
            <p className="mb-2 text-small-lg text-muted">
              {relative(last.at)} · {last.trigger === "low_disk" ? "low disk" : last.trigger}
            </p>
            <DiskCleanupReportView report={last} />
          </>
        ) : (
          <p className="m-0 text-body-sm text-faint">Not run yet. Preview shows what the first run would remove.</p>
        )}
      </DetailSection>
      <DetailSection title="Settings">
        <DiskCleanupSettingsForm settings={settings} minutes={minutes} onChange={setSettings} onMinutes={setMinutes} />
        <Button className="mt-4" size="sm" variant="primary" disabled={busy} onClick={() => void save({})}>
          Save changes
        </Button>
      </DetailSection>
    </LoopCard>
  );
}
