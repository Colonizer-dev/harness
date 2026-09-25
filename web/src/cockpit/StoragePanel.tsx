// The storage panel (issue #220): data-dir usage by category, free space against the warn and
// floor thresholds, and one row per reclaimable colony with a Clean up button each (the same
// confirm SessionView uses). The microsandbox home row is informational — it holds the shared
// image cache, so it is never offered for cleanup. Below that sits the log archive (issue #496):
// its size, and a preview-then-apply retention form. The pure view is what the tests render.
import { useEffect, useState, type ReactElement } from "react";

import { ApiError } from "../api";
import { IconExternal, IconSettings, IconTrash } from "../components/icons";
import { diskSize } from "../components/SessionView";
import type { SectionId } from "../components/SettingsDialog";
import { Button, SESSION_STATUS, Spinner, timeAgo } from "../components/ui";
import { errorMessage, useApi, useToast } from "../context";
import type { ArchiveListing, RetentionPlan, RetentionRequest, StorageSummary } from "../types";

/** The colony-cleanup confirm, word for word the one SessionView's Clean up button uses. */
export const CLEANUP_CONFIRM =
  "Delete this colony's worktree and local branch? Pushed branches are not affected. The worktree is gone, so the colony becomes unresumable.";

/** Bytes (and colonies) reclaimable from finished colonies whose work already landed as a pull request; rows without one are the no-changes pile, named separately below. */
export function prReclaimable(summary: StorageSummary): { bytes: number; colonies: number } {
  const rows = summary.reclaimable.filter((row) => row.pr_url != null);
  return { bytes: rows.reduce((total, row) => total + row.bytes, 0), colonies: rows.length };
}

/** A preview's one-line readout: what the pass would take — or, when the single-copy rule held
 *  every candidate back, that nothing goes until deleting the only copy is allowed. The backend
 *  never answers both a non-empty `remove` and a `kept_single_copy` count, so the held-back case
 *  is just `kept_single_copy > 0`. */
export function retentionSummary(plan: RetentionPlan): string {
  if (plan.kept_single_copy > 0) {
    const bundle = plan.kept_single_copy === 1 ? "bundle is" : "bundles are";
    return `Nothing would be removed: ${plan.kept_single_copy} ${bundle} the only copy (tick "Allow deleting the only copy")`;
  }
  return `This would remove ${plan.count} ${plan.count === 1 ? "bundle" : "bundles"}, ${diskSize(plan.bytes)}`;
}

const ARCHIVE_FIELD = "w-16 rounded-md border border-border bg-transparent px-1.5 py-0.5 text-right font-mono text-[11px] text-text outline-none focus:border-border-strong";

/** A field's number, or null when it is blank or not a usable non-negative number (the request sends null = unset). */
const numeric = (value: string): number | null => {
  const n = Number(value);
  return value.trim() === "" || !Number.isFinite(n) || n < 0 ? null : n;
};

/** The archive's Automatic cleanup form (issue #496): keep N days and/or cap at X GB, with a
 *  Preview that words the plan and an Apply that only goes out once a preview is on the table,
 *  carrying its bundle list as `expect`. Changing any input voids the preview; a 409 — the
 *  archive moved under us — does the same and asks for a fresh one. */
function RetentionForm({ onRun, onApplied }: { onRun: (body: RetentionRequest) => Promise<RetentionPlan>; onApplied: () => void }): ReactElement {
  const [keepDays, setKeepDays] = useState("");
  const [maxGb, setMaxGb] = useState("");
  const [singleCopy, setSingleCopy] = useState(false);
  const [preview, setPreview] = useState<RetentionPlan | null>(null);
  const [stale, setStale] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  /** Any input change voids the preview it would have been applied against. */
  const retool = (set: () => void) => {
    set();
    setPreview(null);
    setStale(false);
  };

  const run = async (dryRun: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const plan = await onRun({
        keep_days: numeric(keepDays),
        max_gb: numeric(maxGb),
        allow_single_copy: singleCopy,
        dry_run: dryRun,
        expect: dryRun ? null : (preview?.remove.map((row) => row.bundle) ?? null),
      });
      if (dryRun) {
        setPreview(plan);
        setStale(false);
      } else {
        setPreview(null);
        onApplied();
      }
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 409) {
        setPreview(null);
        setStale(true);
      } else {
        setError(errorMessage(cause));
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-[12.5px] text-muted">
      <span className="text-faint">Automatic cleanup</span>
      <label className="flex items-center gap-1">
        keep
        <input type="number" min={0} value={keepDays} placeholder="—" onChange={(e) => retool(() => setKeepDays(e.target.value))} className={ARCHIVE_FIELD} />
        days
      </label>
      <label className="flex items-center gap-1">
        cap
        <input type="number" min={0} value={maxGb} placeholder="—" onChange={(e) => retool(() => setMaxGb(e.target.value))} className={ARCHIVE_FIELD} />
        GB
      </label>
      <label className="flex items-center gap-1" title="A bundle that is the only copy of a colony's logs is never deleted unless this is set">
        <input type="checkbox" checked={singleCopy} onChange={(e) => retool(() => setSingleCopy(e.target.checked))} />
        Allow deleting the only copy
      </label>
      <Button size="sm" disabled={busy} onClick={() => void run(true)}>
        Preview
      </Button>
      <Button size="sm" variant="danger" disabled={busy || preview == null} onClick={() => void run(false)}>
        Apply
      </Button>
      {stale && <span className="text-warn">The archive changed since the preview — preview again.</span>}
      {error && <span className="text-warn">{error}</span>}
      {preview && !stale && <span className="font-mono text-[11px] text-faint">{retentionSummary(preview)}</span>}
    </div>
  );
}

export function StoragePanelView({
  summary,
  onOpenColony,
  onCleanup,
  cleaningId,
  onOpenSettings,
  archive = null,
  onRetention,
  onArchiveChanged,
}: {
  summary: StorageSummary;
  /** Jumps to the colony, the same navigation the overview's colony rows use. */
  onOpenColony: (id: string) => void;
  onCleanup: (id: string) => void;
  /** The colony whose cleanup is in flight, if any; its button spins while the rest wait. */
  cleaningId: string | null;
  /** Opens settings at a section; the header gear shows only when given. */
  onOpenSettings?: (section: SectionId) => void;
  /** GET /api/archive's listing; null (an older mothership) hides the whole log-archive section. */
  archive?: ArchiveListing | null;
  /** Runs a retention pass — preview (`dry_run`) or apply; absent in the pure view. */
  onRetention?: (body: RetentionRequest) => Promise<RetentionPlan>;
  /** Called after an apply, so the container refetches the archive. */
  onArchiveChanged?: () => void;
}): ReactElement {
  const pr = prReclaimable(summary);
  const noChanges = summary.reclaimable.filter((row) => row.pr_url == null);
  const noChangesBytes = noChanges.reduce((total, row) => total + row.bytes, 0);
  const categories: { label: string; bytes: number | null; note?: string }[] = [
    { label: "worktrees", bytes: summary.totals.worktrees_bytes },
    { label: "repos", bytes: summary.totals.repos_bytes },
    { label: "sessions", bytes: summary.totals.sessions_bytes },
    { label: "microsandbox home", bytes: summary.totals.microsandbox_bytes, note: "holds the shared image cache · kept" },
  ];
  const limits = [
    summary.warn_free_bytes > 0 ? `warn below ${diskSize(summary.warn_free_bytes)}` : null,
    summary.min_free_bytes > 0 ? `floor ${diskSize(summary.min_free_bytes)}` : null,
  ].filter((part): part is string => part !== null);
  return (
    <section aria-label="Storage" className="flex flex-col overflow-hidden rounded-xl border border-border bg-panel">
      <div className="flex items-center justify-between border-b border-border px-3.5 py-2">
        <span className="font-mono text-[10.5px] tracking-[0.12em] text-faint">STORAGE</span>
        {onOpenSettings && (
          <button
            type="button"
            aria-label="Storage settings"
            title="Storage settings (warn and floor thresholds)"
            onClick={() => onOpenSettings("module:sandbox")}
            className="cursor-pointer rounded-md p-1 text-faint transition-colors hover:bg-panel-2 hover:text-text"
          >
            <IconSettings size={14} />
          </button>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 px-3.5 py-2 font-mono text-[11px] text-faint">
        {categories.map((category) => (
          <span key={category.label} title={category.note ?? `bytes under ${category.label}`}>
            {category.label} {category.bytes != null ? diskSize(category.bytes) : "—"}
            {category.note && <span className="text-faint"> · {category.note}</span>}
          </span>
        ))}
        <span title="free bytes on the data dir's volume at the queue's last check" className="ml-auto">
          {summary.free_bytes != null ? `${diskSize(summary.free_bytes)} free` : "no disk reading yet"}
          {limits.length > 0 && ` · ${limits.join(" · ")}`}
        </span>
      </div>
      {summary.admission_paused && (
        <div role="status" className="mx-3.5 mb-2 rounded-md border border-warn bg-warn-soft px-3 py-2 text-[12.5px] text-warn">
          Queue paused — low disk. Running colonies keep running; admission resumes when space
          returns. Unpushed work is never deleted.
        </div>
      )}
      <div className="border-t border-border px-3.5 py-2 text-[12.5px] text-muted">
        {pr.colonies > 0 ? (
          <>
            {diskSize(pr.bytes)} reclaimable from {pr.colonies} {pr.colonies === 1 ? "colony" : "colonies"} that opened{" "}
            {pr.colonies === 1 ? "a PR" : "PRs"}
          </>
        ) : (
          <>nothing reclaimable from colonies that opened a PR</>
        )}
        {noChanges.length > 0 && <>{" "}· {diskSize(noChangesBytes)} from {noChanges.length} with no changes</>}
      </div>
      {summary.reclaimable.map((row) => (
        <div key={row.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-border px-3.5 py-2">
          <button
            type="button"
            onClick={() => onOpenColony(row.id)}
            title={`Open colony ${row.id}`}
            className="cursor-pointer truncate font-mono text-[12px] font-semibold text-accent hover:underline"
          >
            {row.id}
          </button>
          <span className="font-mono text-[11px] text-faint">{SESSION_STATUS[row.status]?.label ?? row.status}</span>
          {row.due && <span title="past the auto-reclaim retention window" className="font-mono text-[11px] text-warn">past retention</span>}
          {row.pr_url && (
            <a
              href={row.pr_url}
              target="_blank"
              rel="noreferrer"
              title="the pull request this colony's work landed as"
              className="inline-flex items-center gap-1 font-mono text-[11px] text-accent hover:underline"
            >
              PR <IconExternal size={11} />
            </a>
          )}
          <span title={`last activity ${row.updated_at}`} className="font-mono text-[11px] text-faint">
            {timeAgo(row.updated_at)}
          </span>
          <span className="ml-auto inline-flex items-center gap-2.5">
            <span className="font-mono text-[11px] text-faint">{diskSize(row.bytes)}</span>
            <Button variant="danger" disabled={cleaningId !== null} onClick={() => onCleanup(row.id)}>
              {cleaningId === row.id ? <Spinner /> : <IconTrash size={13} />} Clean up
            </Button>
          </span>
        </div>
      ))}
      {archive && (
        <div className="border-t border-border px-3.5 py-2">
          <div className="flex flex-wrap items-center gap-x-3 font-mono text-[11px] text-faint">
            <span className="text-[10.5px] tracking-[0.12em]">LOG ARCHIVE</span>
            <span title={`archived colony logs under ${archive.root}`}>
              {archive.count} {archive.count === 1 ? "bundle" : "bundles"} · {diskSize(archive.bytes)}
            </span>
          </div>
          {onRetention && <RetentionForm onRun={onRetention} onApplied={() => onArchiveChanged?.()} />}
        </div>
      )}
    </section>
  );
}

export function StoragePanel({
  onOpenColony,
  onOpenSettings,
  liveStorage = null,
}: {
  onOpenColony: (id: string) => void;
  onOpenSettings?: (section: SectionId) => void;
  liveStorage?: StorageSummary | null;
}): ReactElement | null {
  const api = useApi();
  const toast = useToast();
  const [summary, setSummary] = useState<StorageSummary | null>(null);
  const [cleaningId, setCleaningId] = useState<string | null>(null);
  const [archive, setArchive] = useState<ArchiveListing | null>(null);

  useEffect(() => {
    let cancelled = false;
    // One fetch: the archive only moves when a colony is deleted or retention is applied, and
    // the apply refetches below. An older mothership has no /api/archive: the section stays hidden.
    api
      .archive()
      .then((loaded) => {
        if (!cancelled) setArchive(loaded);
      })
      .catch(() => {
        if (!cancelled) setArchive(null);
      });
    return () => {
      cancelled = true;
    };
  }, [api]);

  /** Runs a retention pass; an apply refetches the listing so the section's counts keep up. */
  const runRetention = async (body: RetentionRequest) => {
    const plan = await api.archiveRetention(body);
    if (!body.dry_run) setArchive(await api.archive());
    return plan;
  };

  useEffect(() => {
    // A pushed /api/stream frame overrides the fetch (issue #446); when the stream drops its
    // override clears and this refetches, so the panel never freezes on a stale push.
    if (liveStorage) {
      setSummary(liveStorage);
      return;
    }
    let cancelled = false;
    // One fetch and no poll: each GET /api/storage walks the worktrees and the microsandbox
    // home, and a cleanup refetches below. (No status prop reaches this panel to refetch on.)
    api
      .storageSummary()
      .then((loaded) => {
        if (!cancelled) setSummary(loaded);
      })
      .catch(() => {
        // An older mothership has no /api/storage: stay hidden like the first poll did.
        if (!cancelled) setSummary(null);
      });
    return () => {
      cancelled = true;
    };
  }, [api, liveStorage]);

  if (!summary) return null;

  const cleanup = async (id: string) => {
    if (!window.confirm(CLEANUP_CONFIRM)) return;
    setCleaningId(id);
    try {
      await api.cleanupSession(id);
      toast("Worktree cleaned up");
      setSummary(await api.storageSummary());
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setCleaningId(null);
    }
  };

  return (
    <StoragePanelView
      summary={summary}
      onOpenColony={onOpenColony}
      onCleanup={(id) => void cleanup(id)}
      cleaningId={cleaningId}
      onOpenSettings={onOpenSettings}
      archive={archive}
      onRetention={(body) => runRetention(body)}
      onArchiveChanged={() => void api.archive().then(setArchive).catch(() => {})}
    />
  );
}
