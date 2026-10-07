// The cockpit-global session/usage-limit banner (issue #404): when GET /api/status reports a
// paused quota, every cockpit view banners it above its own content — the overview hides its own
// scoped queue-paused banner while this one shows, so the pause banners exactly once. Resume-all
// reuses the per-session resume endpoint over the parked colonies; there is no bulk endpoint.
// Dismissal mirrors the storage alert's keyed pattern, so a new reset (or a new pause scope)
// re-shows the banner. It names the plan that ran out by its display name — "BytePlus plan limit
// reached"; only the Claude account's own cap is ever called Claude — with the roles it affects, the
// reset as a local time and a countdown, and Resume all only when a colony is actually parked.
// Rendered to static markup in the tests: no DOM.
import { useState, type ReactElement } from "react";

import { PROVIDER_CATALOG } from "../providerCatalog";
import { resetClock, resetWords, untilWords } from "../resetTime";
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
 * Colonies the quota pause parked — the frontend-visible half of the backend's
 * `resume_quota_parked` predicate (which also requires a worktree the status poll never shows).
 * A current mothership marks them with status `parked` (issue #213); older builds left them
 * `stopped` with the `provider_quota_exhausted` attention flag, which still counts so an old
 * list does not lose its resume-all. The per-colony Resume buttons already cover exactly these
 * (`!live && !cleaned_up && (parked || stopped || failed)`), so resume-all resumes nothing
 * they could not.
 */
export function quotaParkedSessions(sessions: Session[]): Session[] {
  return sessions.filter(
    (session) =>
      !session.cleaned_up &&
      (session.status === "parked" ||
        (session.status === "stopped" && session.attention?.reason === "provider_quota_exhausted")),
  );
}

/**
 * Identity of a quota pause for dismissal: a new reset, or a new pause scope, is a new banner.
 * The backend's `reason` is deliberately NOT part of the key: it embeds the live waiting count
 * (e.g. "… (3 waiting)"), so keying on it would re-show the banner every time the count moves.
 */
export const quotaBannerKey = (quota: StatusQuota): string =>
  `${quota.reset_unix ?? ""}|${quota.reset_at ?? ""}|${quotaPauseKind(quota)}`;

/**
 * The quota to banner, or null when there is none or the operator dismissed this one: a pause, or
 * the Claude account being out while its fallback model carries the work (issue #1130).
 */
export const visibleQuotaBanner = (
  quota: StatusQuota | null | undefined,
  dismissed: ReadonlySet<string>,
): StatusQuota | null =>
  quota && (quota.paused || quota.fallback) && !dismissed.has(quotaBannerKey(quota)) ? quota : null;

/**
 * The fallback banner's words — "Claude out, running on MiniMax until 19:51" — or null when the
 * account is not out with a fallback carrying it. The reset reads as a local time and a countdown,
 * as the pause banner's does; nothing is paused, so the second line says what keeps running.
 */
export function quotaFallbackParts(
  quota: StatusQuota,
  nowMs: number = Date.now(),
  timeZone?: string,
): { title: string; effect: string } | null {
  const fallback = quota.fallback;
  if (!fallback || quota.paused) return null;
  const reset = fallback.reset_unix ?? quota.reset_unix;
  let until = "";
  if (reset != null && reset * 1000 > nowMs) until = ` until ${resetClock(reset, nowMs, timeZone)} · in ${untilWords(reset, nowMs)}`;
  else if (fallback.reset_at ?? quota.reset_at) until = ` until ${fallback.reset_at ?? quota.reset_at}`;
  return {
    title: `Claude out, running on ${fallback.provider_name}${until}`,
    effect: `Roles that use Claude run on ${fallback.model} and go back to Claude by themselves at the reset. Restricted tasks need a trusted provider and park if it is not.`,
  };
}

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

/** One exhausted plan as the banner names it: display name and the roles routed to it. */
export interface QuotaBannerPlan {
  name: string;
  usedBy: string[];
}

/**
 * The plans the pause names. A current mothership sends `provider_details` (display names and the
 * roles on each); an older one only ids, which are looked up in the provider catalog so the banner
 * still says "BytePlus" rather than "byteplus". An account pause is the Claude account's own cap —
 * the one case the banner says "Claude".
 */
export function quotaBannerPlans(quota: StatusQuota): QuotaBannerPlan[] {
  const details = quota.provider_details ?? [];
  if (quotaPauseKind(quota) === "account") {
    const claude = details.find((detail) => detail.id === "anthropic");
    return [{ name: "Claude", usedBy: claude?.used_by ?? [] }];
  }
  const ids = (quota.providers ?? []).filter((id) => id.length > 0);
  const named = ids.length > 0 ? ids : details.map((detail) => detail.id);
  return named.map((id) => {
    const detail = details.find((d) => d.id === id);
    const name = detail?.name.trim() || PROVIDER_CATALOG.find((entry) => entry.id === id)?.name || id;
    return { name, usedBy: detail?.used_by ?? [] };
  });
}

/** "a, b and c". */
const listWords = (items: string[]): string =>
  items.length <= 1 ? (items[0] ?? "") : `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;

/**
 * The headline: "Claude session limit reached" for the account's own cap; "BytePlus plan limit
 * reached" for a provider (a name that already says "plan", like "Baidu Qianfan Coding Plan", is not
 * doubled); "BytePlus and MiniMax plan limits reached" for several.
 */
export function quotaBannerTitle(quota: StatusQuota): string {
  if (quotaPauseKind(quota) === "account") return "Claude session limit reached";
  const names = quotaBannerPlans(quota).map((plan) => plan.name);
  if (names.length === 0) return "Provider plan limit reached";
  if (names.length === 1) {
    const [name] = names;
    return /\bplan$/i.test(name) ? `${name} limit reached` : `${name} plan limit reached`;
  }
  return `${listWords(names)} plan limits reached`;
}

/** The banner's parts, each its own sentence; `quotaBannerText` joins them. */
export interface QuotaBannerParts {
  title: string;
  /** "Used by subagents and background." — omitted when no role is known to route there. */
  usedBy: string | null;
  /** "resets at 19:51 · in 2 h 10 min". */
  reset: string;
  /** What the pause means for the fleet. */
  effect: string;
  /** "2 colonies paused." — null with none, so the banner never says "0 paused colonies". */
  parked: string | null;
}

/**
 * The banner's own words, built from structured fields — never from the backend's `reason` string,
 * which carries the live waiting count. It names the provider by its display name (never "Claude"
 * unless the Claude account itself is out), says which roles it affects, gives the reset as a local
 * time and a countdown, and counts parked colonies only when there are some.
 */
export function quotaBannerParts(quota: StatusQuota, parked: number, nowMs: number = Date.now(), timeZone?: string): QuotaBannerParts {
  const plans = quotaBannerPlans(quota);
  const roles: string[] = [];
  for (const plan of plans) for (const role of plan.usedBy) if (!roles.includes(role)) roles.push(role);
  const account = quotaPauseKind(quota) === "account";
  const effect = account
    ? "Colonies on other providers keep running; new colonies wait in the queue until it resets."
    : `Colonies on other providers keep running; new colonies wait in the queue until ${plans.length > 1 ? "they reset" : "it resets"}.`;
  const reset = resetWords(quota, nowMs, timeZone);
  return {
    title: quotaBannerTitle(quota),
    usedBy: roles.length > 0 ? `Used by ${listWords(roles)}.` : null,
    reset: `${reset.charAt(0).toUpperCase()}${reset.slice(1)}.`,
    effect,
    parked: parked > 0 ? `${parked} ${parked === 1 ? "colony" : "colonies"} paused.` : null,
  };
}

/** The whole banner as one line of text (the tests' handle, and the banner's accessible label). */
export function quotaBannerText(quota: StatusQuota, parked: number, nowMs: number = Date.now(), timeZone?: string): string {
  const parts = quotaBannerParts(quota, parked, nowMs, timeZone);
  return [`${parts.title}.`, parts.usedBy, parts.reset, parts.effect, parts.parked].filter(Boolean).join(" ");
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
  const carried = quota ? quotaFallbackParts(quota) : null;
  if (quota && carried) {
    return (
      <div className="px-6 pt-4">
        <div
          role="status"
          aria-label={`${carried.title}. ${carried.effect}`}
          className="flex flex-wrap items-start gap-x-3 gap-y-1.5 rounded-md border border-warn bg-warn-soft px-3 py-2 text-sm text-warn"
        >
          <span className="min-w-0 flex-1 basis-64">
            <strong className="font-semibold">{carried.title}</strong>
            <span className="text-small-lg">{` · ${carried.effect}`}</span>
          </span>
          <button type="button" onClick={onDismiss} className="ml-auto shrink-0 cursor-pointer text-small-lg font-semibold hover:underline">
            Dismiss
          </button>
        </div>
      </div>
    );
  }
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
  const parts = quotaBannerParts(quota, parked.length);
  return (
    <div className="px-6 pt-4">
      <div
        role="status"
        aria-label={quotaBannerText(quota, parked.length)}
        className="flex flex-wrap items-start gap-x-3 gap-y-1.5 rounded-md border border-warn bg-warn-soft px-3 py-2 text-sm text-warn"
      >
        <span className="min-w-0 flex-1 basis-64">
          <strong className="font-semibold">{parts.title}</strong>
          <span className="text-small-lg">
            {" · "}
            {parts.reset}
            {parts.usedBy ? ` ${parts.usedBy}` : ""} {parts.effect}
            {parts.parked ? ` ${parts.parked}` : ""}
          </span>
        </span>
        <span className="ml-auto flex shrink-0 items-center gap-2">
          {parked.length > 0 ? (
            <button
              type="button"
              onClick={() => void resumeAll()}
              disabled={busy}
              title={`Resume ${parked.length === 1 ? "the parked colony" : `all ${parked.length} parked colonies`}`}
              className="cursor-pointer rounded-md border border-warn px-2 py-0.5 text-small-lg font-semibold hover:underline disabled:cursor-default disabled:opacity-50 disabled:hover:no-underline"
            >
              {busy ? "Resuming…" : `Resume all (${parked.length})`}
            </button>
          ) : null}
          <button
            type="button"
            onClick={onDismiss}
            className="cursor-pointer text-small-lg font-semibold hover:underline"
          >
            Dismiss
          </button>
        </span>
      </div>
    </div>
  );
}
