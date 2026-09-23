// The cockpit-global session/usage-limit banner (issue #404): when GET /api/status reports a
// paused quota, every cockpit view banners it above its own content — the overview's own scoped
// banner stays where it is. Resume-all reuses the per-session resume endpoint over the parked
// colonies; there is no bulk endpoint. Dismissal mirrors the storage alert's keyed pattern, so a
// new reset (or a new reason) re-shows the banner. Rendered to static markup in the tests: no DOM.
import { useState, type ReactElement } from "react";

import type { Session, StatusQuota } from "../types";

/**
 * Colonies the quota pause parked: stopped, flagged `provider_quota_exhausted`, worktree kept — the
 * frontend-visible half of the backend's `resume_quota_parked` predicate (which also requires a
 * worktree the status poll never shows). The per-colony Resume buttons already cover exactly these
 * (`!live && !cleaned_up && (stopped || failed)`), so resume-all resumes nothing they could not.
 */
export function quotaParkedSessions(sessions: Session[]): Session[] {
  return sessions.filter(
    (session) =>
      session.status === "stopped" &&
      session.attention?.reason === "provider_quota_exhausted" &&
      !session.cleaned_up,
  );
}

/** Identity of a quota pause for dismissal: a new reset (or a new reason) is a new banner. */
export const quotaBannerKey = (quota: StatusQuota): string =>
  `${quota.reset_unix ?? ""}|${quota.reset_at ?? ""}|${quota.reason ?? ""}`;

/** The paused quota to banner, or null when there is none or the operator dismissed this one. */
export const visibleQuotaBanner = (
  quota: StatusQuota | null | undefined,
  dismissed: ReadonlySet<string>,
): StatusQuota | null =>
  quota && quota.paused && !dismissed.has(quotaBannerKey(quota)) ? quota : null;

/** `dismissed` plus `quota`'s key; earlier keys stay, so a re-paused queue re-shows the banner. */
export const dismissQuotaBanner = (dismissed: ReadonlySet<string>, quota: StatusQuota): ReadonlySet<string> =>
  new Set(dismissed).add(quotaBannerKey(quota));

/**
 * Resume every parked colony through `resume`, settled per colony so one 409 cannot block the rest —
 * the cockpit's 4 s poll is the backstop for the ones that fail, as with single resumes.
 */
export function resumeQuotaParkedSessions(
  sessions: Session[],
  resume: (id: string) => Promise<unknown>,
): Promise<PromiseSettledResult<unknown>[]> {
  return Promise.allSettled(quotaParkedSessions(sessions).map((session) => resume(session.id)));
}

/** The banner's own words: the queue holder's reason when it named one, the reset when it named one. */
export function quotaBannerText(quota: StatusQuota, parked: number): string {
  const what = quota.reason ?? "Claude session limit reached";
  const when = quota.reset_at ? `resets ${quota.reset_at}` : "resets soon";
  const colonies = `${parked} paused ${parked === 1 ? "colony" : "colonies"}`;
  return `${what} — ${when}. ${colonies}.`;
}

export function QuotaBanner({
  quota,
  sessions,
  onResumeAll,
  onDismiss,
}: {
  /** The paused quota; null or unpaused renders nothing. */
  quota: StatusQuota | null | undefined;
  /** Every colony the mothership knows, so the banner can count (and resume) the parked ones. */
  sessions: Session[];
  onResumeAll: () => Promise<void> | void;
  onDismiss: () => void;
}): ReactElement | null {
  const [busy, setBusy] = useState(false);
  if (!quota?.paused) return null;
  const parked = quotaParkedSessions(sessions);
  const resumeAll = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await onResumeAll();
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="px-6 pt-4">
      <div role="status" className="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-md border border-warn bg-warn-soft px-3 py-2 text-sm text-warn">
        <span>{quotaBannerText(quota, parked.length)}</span>
        <span className="ml-auto flex items-center gap-2">
          <button
            type="button"
            onClick={() => void resumeAll()}
            disabled={parked.length === 0 || busy}
            title={parked.length === 0 ? "No quota-parked colonies to resume" : `Resume ${parked.length === 1 ? "the parked colony" : `all ${parked.length} parked colonies`}`}
            className="cursor-pointer rounded-md border border-warn px-2 py-0.5 text-[12.5px] font-semibold hover:underline disabled:cursor-default disabled:opacity-50 disabled:hover:no-underline"
          >
            {busy ? "Resuming…" : parked.length > 0 ? `Resume all (${parked.length})` : "Resume all"}
          </button>
          <button
            type="button"
            onClick={onDismiss}
            className="cursor-pointer text-[12.5px] font-semibold hover:underline"
          >
            Dismiss
          </button>
        </span>
      </div>
    </div>
  );
}
