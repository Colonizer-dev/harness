// The storage panel (issue #220): data-dir usage by category, free space against the warn and
// floor thresholds, and one row per reclaimable colony with a Clean up button each (the same
// confirm SessionView uses). The microsandbox home row is informational — it holds the shared
// image cache, so it is never offered for cleanup. The pure view is what the tests render.
import { useEffect, useState, type ReactElement } from "react";

import { IconExternal, IconTrash } from "../components/icons";
import { diskSize } from "../components/SessionView";
import { Button, SESSION_STATUS, Spinner, timeAgo } from "../components/ui";
import { errorMessage, useApi, useToast } from "../context";
import type { StorageSummary } from "../types";

/** The colony-cleanup confirm, word for word the one SessionView's Clean up button uses. */
export const CLEANUP_CONFIRM =
  "Delete this colony's worktree and local branch? Pushed branches are not affected. The worktree is gone, so the colony becomes unresumable.";

/** Bytes (and colonies) reclaimable from finished colonies whose work already landed as a pull request; rows without one are the no-changes pile, named separately below. */
export function prReclaimable(summary: StorageSummary): { bytes: number; colonies: number } {
  const rows = summary.reclaimable.filter((row) => row.pr_url != null);
  return { bytes: rows.reduce((total, row) => total + row.bytes, 0), colonies: rows.length };
}

export function StoragePanelView({
  summary,
  onOpenColony,
  onCleanup,
  cleaningId,
}: {
  summary: StorageSummary;
  /** Jumps to the colony, the same navigation the overview's colony rows use. */
  onOpenColony: (id: string) => void;
  onCleanup: (id: string) => void;
  /** The colony whose cleanup is in flight, if any; its button spins while the rest wait. */
  cleaningId: string | null;
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
      <div className="border-b border-border px-3.5 py-2 font-mono text-[10.5px] tracking-[0.12em] text-faint">STORAGE</div>
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
    </section>
  );
}

export function StoragePanel({ onOpenColony }: { onOpenColony: (id: string) => void }): ReactElement | null {
  const api = useApi();
  const toast = useToast();
  const [summary, setSummary] = useState<StorageSummary | null>(null);
  const [cleaningId, setCleaningId] = useState<string | null>(null);

  useEffect(() => {
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
  }, [api]);

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
    />
  );
}
