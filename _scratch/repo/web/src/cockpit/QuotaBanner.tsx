// The cockpit-global session/usage-limit banner (issue #404): when GET /api/status reports a
// paused quota, every cockpit view banners it above its own content — the overview hides its own
// scoped queue-paused banner while this one shows, so the pause banners exactly once. Resume-all
// reuses the per-session resume endpoint over the parked colonies; there is no bulk endpoint.
// Dismissal mirrors the storage alert's keyed pattern, so a new reset (or a new pause scope)
// re-shows the banner. Rendered to static markup in the tests: no DOM.
import { useState, type ReactElement } from "react";

import type { Session, StatusQuota } from "../types";

/** Which scope a quota pause covers: the whole account, or named exhausted providers. */
export type QuotaPauseKind = "account" | "provider";

/**
 * The pause's scope, which the banner text and the dismissal key both build from.
 *
 * Assumption: the backend's StatusQuota carries no `kind` field yet (only paused, reason,
 * reset_at, reset_unix, providers), so the frontend derives the scope: a pause naming exhausted
 * providers is provider-scoped, while one naming none is an account-level pause (e.g. a Claude
 * session limit). If the backend later adds `quota.kind` (`"account"` | `"provider"`), that wins
 * over the derivation — the cast below keeps compiling either way.
 */
export function quotaPauseKind(quota: StatusQuota): QuotaPauseKind {
  const withKind = quota as StatusQuota & { kind?: unknown };
  if (withKind.kind === "account" || withKind.kind === "provider") return withKind.kind;
  return (quota.providers ?? []).length > 0 ? "provider" : "account";
}

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

/**
 * Identity of a quota pause for dismissal: a new reset, or a new pause scope, is a new banner.
 * The backend's `reason` is deliberately NOT part of the key: it embeds the live waiting count
 * (e.g. "… (3 waiting)"), so keying on it would re-show the banner every time the count moves.
 */
export const quotaBannerKey = (quota: StatusQuota): string =>
  `${quota.reset_unix ?? ""}|${quota.reset_at ?? ""}|${quotaPauseKind(quota)}`;

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

/**
 * The banner's own words, built from structured fields — never from the backend's `reason` string,
 * which carries the live waiting count and provider-specific phrasing. An account-level pause names
 * no providers; a provider-scoped pause names the exhausted ones from `providers`.
 */
export function quotaBannerText(quota: StatusQuota, parked: number): string {
  const when = quota.reset_at ? `resets ${quota.reset_at}` : "resets soon";
  const colonies = `${parked} paused ${parked === 1 ? "colony" : "colonies"}`;
  if (quotaPauseKind(quota) === "provider") {
    const names = (quota.providers ?? []).filter((provider) => provider.length > 0);
    const where = names.length > 0 ? ` on ${names.join(", ")}` : "";
    return `Claude session limit reached${where} — ${when}. ${colonies}.`;
  }
  return `Claude session limit reached — ${when}. ${colonies}.`;
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
