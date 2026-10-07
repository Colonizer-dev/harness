// The cockpit-global Claude account banner (issue #984): when GET /api/status reports an account
// that needs the owner — a token the API rejected, or a plan that hit its usage limit — every
// cockpit view banners it above its own content, one line per account. `Sign in` reuses the
// settings route: it opens the Accounts page (the Connections section), where the sign-in runs.
// Absent field on an older mothership reads as no alerts. Rendered to static markup in the tests.
import type { ReactElement } from "react";

import type { AccountAlert } from "../features/providers/types";

/**
 * One alert's words, built from structured fields. The waiting sentence is left off at zero
 * colonies, so a lone alert is one clean line; the colony/colonies plural follows the count.
 */
export function accountAlertText(alert: AccountAlert): string {
  const cause =
    alert.state === "limited"
      ? `Claude account ${alert.account} hit its usage limit.`
      : `Claude account ${alert.account} needs you to sign in again.`;
  if (alert.waiting <= 0) return cause;
  return `${cause} ${alert.waiting} ${alert.waiting === 1 ? "colony is" : "colonies are"} waiting on it.`;
}

/** The account banner: one line per alert, nothing when there are none. */
export function AccountBanner({
  alerts,
  onSignIn,
}: {
  /** Accounts that need the owner; an empty or missing list renders nothing. */
  alerts: AccountAlert[] | null | undefined;
  /** Opens the Accounts page (the Connections settings section), where the sign-in runs. */
  onSignIn: () => void;
}): ReactElement | null {
  if (!alerts || alerts.length === 0) return null;
  return (
    <div className="px-6 pt-4">
      <div className="flex flex-col gap-1.5">
        {alerts.map((alert) => (
          <div
            key={`${alert.account}:${alert.state}`}
            role="status"
            className="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-md border border-warn bg-warn-soft px-3 py-2 text-sm text-warn"
          >
            <span>{accountAlertText(alert)}</span>
            <button
              type="button"
              onClick={onSignIn}
              className="cursor-pointer rounded-md border border-warn px-2 py-0.5 text-small-lg font-semibold hover:underline"
            >
              Sign in
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}
