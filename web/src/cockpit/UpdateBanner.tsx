// The cockpit-global update banner (issue #1097). A release can carry `critical` and `fixes-running`
// notices (changelog.d/README.md), and GET /api/update answers the ones newer than this build as
// `notices`, each with the number of colonies its read-only probe matched here. While one is
// pending the update is a banner above every view rather than a quiet badge: the release, its
// line, and how many colonies are affected, with a button to the Updates pane where it is applied.
// After the update, colonies still running on the previous version's components that a probe
// matched are banner-worthy too, with "Restart on the new version". Everything else — routine
// releases, colonies behind on a routine update — stays in Settings → Updates. It clears itself, so
// there is nothing to dismiss. Absent on an older mothership. Rendered to static markup in the tests.
import type { ReactElement } from "react";

import type { Api } from "../api";
import type { BehindColony, UpdateNotice, UpdateStatus } from "../types";

/** The notices newer than this build, critical first. */
export function pendingNotices(update: UpdateStatus | null | undefined): UpdateNotice[] {
  const notices = update?.notices ?? [];
  return [...notices].sort((a, b) => Number(b.severity === "critical") - Number(a.severity === "critical"));
}

/** The colonies still on the previous version that a notice's probe matched. */
export function affectedBehind(update: UpdateStatus | null | undefined): BehindColony[] {
  return (update?.behind ?? []).filter((c) => c.affected_by.length > 0);
}

function colonies(n: number): string {
  return n === 1 ? "1 colony" : `${n} colonies`;
}

/** "Update to v0.2.7: fixes … 4 of your colonies are affected." — null with nothing pending. */
export function noticeText(update: UpdateStatus | null | undefined): string | null {
  const notices = pendingNotices(update);
  if (!notices.length) return null;
  const version = update?.latest?.version ?? notices[0].version;
  const lines = notices.map((n) => n.line.replace(/[.\s]+$/, ""));
  const affected = new Set(notices.flatMap((n) => n.affected?.colonies ?? []));
  const counted = notices.some((n) => n.affected !== null);
  let text = `Update to ${version}: ${lines.join("; ")}.`;
  if (affected.size > 0) text += ` ${affected.size === 1 ? "1 of your colonies is" : `${affected.size} of your colonies are`} affected.`;
  else if (counted) text += " None of your colonies is affected right now.";
  return text;
}

/** "2 colonies still run on the previous version and hit a bug it fixes: …" — null when none. */
export function behindText(update: UpdateStatus | null | undefined): string | null {
  const affected = affectedBehind(update);
  if (!affected.length) return null;
  const lines = [...new Set(affected.flatMap((c) => c.affected_by))].map((l) => l.replace(/[.\s]+$/, ""));
  return `${colonies(affected.length)} still ${affected.length === 1 ? "runs" : "run"} on the previous version and ${
    affected.length === 1 ? "hits" : "hit"
  } a bug it fixes: ${lines.join("; ")}. A restart keeps the worktree and the conversation.`;
}

/**
 * Restarts colonies on the new version through POST /api/update/restart and re-reads the update
 * status; `say` reports the outcome. Shared by the banner and the Updates pane.
 */
export async function restartOnNewVersion(
  api: Api,
  which: { ids: string[] } | { all: true },
  say: (message: string, tone?: "error") => void,
  onChanged?: (update: UpdateStatus) => void,
): Promise<void> {
  try {
    const answer = await api.restartOnNewVersion(which);
    const started = answer.restarting.length;
    const skipped = answer.skipped.length;
    say(
      started > 0
        ? `Restarting ${colonies(started)} on the new version${skipped ? ` (${skipped} skipped: ${answer.skipped[0].reason})` : ""}.`
        : `Nothing to restart${skipped ? `: ${answer.skipped[0].reason}` : ""}.`,
    );
  } catch (e) {
    say(e instanceof Error ? e.message : String(e), "error");
  }
  if (onChanged) {
    try {
      onChanged(await api.update());
    } catch {
      /* the next poll catches up */
    }
  }
}

/** The update banner: the pending notices, then the affected colonies left behind; nothing otherwise. */
export function UpdateBanner({
  update,
  onOpenUpdates,
  onRestart,
}: {
  update: UpdateStatus | null | undefined;
  /** Opens Settings → Updates, where the update is applied (or a source build is told how). */
  onOpenUpdates: () => void;
  /** Restarts these colonies on the new version. */
  onRestart: (ids: string[]) => void;
}): ReactElement | null {
  const notice = noticeText(update);
  const behind = behindText(update);
  if (!notice && !behind) return null;
  const critical = pendingNotices(update).some((n) => n.severity === "critical");
  const affected = affectedBehind(update).map((c) => c.id);
  const restarting = new Set(update?.restarts?.restarting ?? []);
  const busy = affected.length > 0 && affected.every((id) => restarting.has(id));
  const tone = critical ? "border-err bg-err-soft text-err" : "border-warn bg-warn-soft text-warn";
  const button = "cursor-pointer rounded-md px-2 py-0.5 text-[12.5px] font-semibold hover:underline disabled:cursor-default disabled:opacity-60";
  return (
    <div className="px-6 pt-4">
      {notice ? (
        <div role="alert" className={`flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-md border px-3 py-2 text-sm ${tone}`}>
          <span>{notice}</span>
          <button type="button" onClick={onOpenUpdates} className={`${button} border ${critical ? "border-err" : "border-warn"}`}>
            {update?.can_apply.ok ? "Review and update" : "Why not here"}
          </button>
        </div>
      ) : null}
      {behind ? (
        <div
          role="status"
          className={`${notice ? "mt-2 " : ""}flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-md border border-warn bg-warn-soft px-3 py-2 text-sm text-warn`}
        >
          <span>{behind}</span>
          <button type="button" disabled={busy} onClick={() => onRestart(affected.filter((id) => !restarting.has(id)))} className={`${button} border border-warn`}>
            {busy ? "Restarting…" : "Restart on the new version"}
          </button>
          <button type="button" onClick={onOpenUpdates} className={button}>
            Details
          </button>
        </div>
      ) : null}
    </div>
  );
}
