// The built-in Disk cleanup loop's row on the Loops page and its dialog: a dry-run preview (shown
// before the first enable), its settings, and its run history. The rules live in diskCleanup.ts.
import { useEffect, useRef, useState, type ReactElement } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { Button, Spinner, Switch, cx } from "../components/ui";
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
  parseExtraPaths,
  reportSummary,
  settingsOf,
} from "./diskCleanup";

export type DiskCleanupTab = "preview" | "settings" | "history";

/** The built-in loop's row in the Loops list. */
export function DiskCleanupRow({
  loop,
  now,
  onToggle,
  onOpen,
  onRunNow,
}: {
  loop: Loop;
  now: number;
  onToggle: (on: boolean) => void;
  onOpen: (tab: DiskCleanupTab) => void;
  onRunNow: () => void;
}): ReactElement {
  const last = loop.disk_cleanup?.history[0];
  const attention = loop.disk_cleanup?.attention;
  return (
    <li className={cx("flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3", !loop.enabled && "opacity-80")} data-loop="disk-cleanup">
      <span aria-hidden className="grid size-7 shrink-0 place-items-center rounded-full border border-border text-[13px] text-muted">
        ⌫
      </span>
      <div className="min-w-0 flex-1 basis-64">
        <div className="flex items-center gap-2">
          <span className="truncate text-[14px] font-medium text-text">{loop.name}</span>
          <span className="truncate font-mono text-[11.5px] text-faint">this host</span>
        </div>
        <div className="mt-0.5 truncate text-[12.5px] text-muted" title={loop.last_note ?? undefined}>
          {describeDiskCleanup(loop)}
          {loop.enabled && loop.next_run_at ? ` · next ${relative(loop.next_run_at, now)}` : " · off"}
        </div>
        {attention && <div className="mt-1 text-[12.5px] text-warn">{attention}</div>}
      </div>
      <div className="w-[170px] shrink-0 text-[12.5px]">
        {last ? (
          <button type="button" onClick={() => onOpen("history")} className="cursor-pointer border-0 bg-transparent p-0 text-left text-muted hover:text-text">
            {reportSummary(last)} · {relative(last.at, now)}
          </button>
        ) : (
          <span className="text-faint">not run yet</span>
        )}
      </div>
      <div className="flex shrink-0 items-center gap-1.5">
        <Switch checked={loop.enabled} onChange={onToggle} label={`${loop.name} enabled`} />
        <Button size="sm" variant="secondary" onClick={() => onOpen("preview")}>
          Preview
        </Button>
        <Button size="sm" variant="ghost" onClick={onRunNow}>
          Run now
        </Button>
        <Button size="sm" variant="ghost" onClick={() => onOpen("history")}>
          History
        </Button>
        <Button size="sm" variant="ghost" onClick={() => onOpen("settings")}>
          Settings
        </Button>
      </div>
    </li>
  );
}

/** One report — a dry run or a stored run — category by category, with every path and size. */
export function DiskCleanupReportView({ report }: { report: DiskCleanupReport }): ReactElement {
  return (
    <div className="space-y-3 text-[13px]">
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
                <li key={item.path} className="flex gap-3 font-mono text-[11.5px] text-muted">
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
                <li key={`${h.path}:${h.reason}`} className="truncate text-[11.5px] text-faint" title={h.path}>
                  kept: {h.path} ({heldReason(h.reason)})
                </li>
              ))}
            </ul>
          )}
          {(c.failed ?? []).map((f) => (
            <p key={f} className="m-0 mt-1 text-[11.5px] text-err">
              {f}
            </p>
          ))}
          {c.note && <p className="m-0 mt-1 text-[11.5px] text-faint">{c.note}</p>}
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
  const field = "rounded-lg border border-border bg-transparent px-2.5 py-1.5 text-[13px] text-text outline-none focus:border-border-strong";
  return (
    <div className="space-y-4 text-[13px]">
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
              <div className="text-[12px] text-faint">{c.hint}</div>
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
          <span className="mt-1 block text-[12px] text-faint">
            Only Cargo target/ dirs untouched for {settings.host_min_age_days} days go. Never ~/.cargo, caches, .git or anything outside these paths.
          </span>
        </label>
      )}
    </div>
  );
}

/** The dialog behind the row: preview (a dry run), settings, history. */
export function DiskCleanupDialog({ loop, tab: initialTab, onClose, onSaved }: { loop: Loop; tab: DiskCleanupTab; onClose: () => void; onSaved: () => void }): ReactElement {
  const ref = useRef<HTMLDialogElement>(null);
  const api = useApi();
  const toast = useToast();
  const [tab, setTab] = useState<DiskCleanupTab>(initialTab);
  const [preview, setPreview] = useState<DiskCleanupReport | null>(null);
  const [settings, setSettings] = useState<DiskCleanupSettings>(() => settingsOf(loop));
  const [minutes, setMinutes] = useState(loop.cadence.every === "interval" ? loop.cadence.minutes : 60);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const d = ref.current;
    if (d && !d.open) d.showModal?.();
  }, []);
  useEffect(() => {
    if (tab !== "preview") return;
    let live = true;
    setPreview(null);
    api.runDiskCleanup(loop.id, true).then(
      (r) => live && setPreview(r),
      (e) => live && toast(errorMessage(e), "error"),
    );
    return () => {
      live = false;
    };
  }, [api, loop.id, tab, toast]);

  const save = async (change: { enabled?: boolean }) => {
    setBusy(true);
    try {
      await api.updateLoop(loop.id, diskCleanupBody(loop, { ...change, minutes, settings }));
      toast(change.enabled ? "Disk cleanup is on" : "Disk cleanup saved");
      onSaved();
      ref.current?.close();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const history = loop.disk_cleanup?.history ?? [];
  return (
    <dialog ref={ref} onClose={onClose} aria-labelledby="disk-cleanup-title" className="m-auto w-[min(720px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50">
      <div className="flex items-center gap-3 border-b border-border px-5 py-3">
        <h2 id="disk-cleanup-title" className="min-w-0 flex-1 text-[16px] font-semibold">
          Disk cleanup
        </h2>
        <button type="button" onClick={() => ref.current?.close()} aria-label="Close" className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
          ✕
        </button>
      </div>
      <div className="flex gap-1.5 border-b border-border px-5 py-2">
        {(
          [
            ["preview", "Preview"],
            ["settings", "Settings"],
            ["history", "History"],
          ] as const
        ).map(([key, label]) => (
          <button
            key={key}
            type="button"
            aria-pressed={tab === key}
            onClick={() => setTab(key)}
            className={cx("cursor-pointer rounded-lg border px-2.5 py-1 text-[12.5px]", tab === key ? "border-accent bg-accent-soft text-text" : "border-border bg-transparent text-muted hover:text-text")}
          >
            {label}
          </button>
        ))}
      </div>
      <div className="scroll-thin max-h-[65vh] overflow-y-auto px-5 py-4">
        {tab === "preview" &&
          (preview ? (
            <DiskCleanupReportView report={preview} />
          ) : (
            <p className="flex items-center gap-2 text-[13px] text-muted">
              <Spinner /> Working out what a run would remove…
            </p>
          ))}
        {tab === "settings" && <DiskCleanupSettingsForm settings={settings} minutes={minutes} onChange={setSettings} onMinutes={setMinutes} />}
        {tab === "history" &&
          (history.length === 0 ? (
            <p className="text-[13px] text-faint">No runs yet.</p>
          ) : (
            <ul className="m-0 list-none space-y-4 p-0">
              {history.map((r) => (
                <li key={r.at}>
                  <div className="mb-1 text-[12px] text-faint">
                    {relative(r.at)} · {r.trigger === "low_disk" ? "low disk" : r.trigger}
                  </div>
                  <DiskCleanupReportView report={r} />
                </li>
              ))}
            </ul>
          ))}
      </div>
      <div className="flex items-center justify-end gap-2 border-t border-border px-5 py-3">
        <Button onClick={() => ref.current?.close()}>Close</Button>
        {tab === "settings" && (
          <Button variant="secondary" disabled={busy} onClick={() => void save({})}>
            {busy && <Spinner />} Save
          </Button>
        )}
        {!loop.enabled && tab !== "history" && (
          <Button variant="primary" disabled={busy || (tab === "preview" && !preview)} onClick={() => void save({ enabled: true })}>
            {busy && <Spinner />} Turn on disk cleanup
          </Button>
        )}
      </div>
    </dialog>
  );
}
