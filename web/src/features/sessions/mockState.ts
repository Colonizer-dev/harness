// The mock's per-call state slice for the sessions feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { BurnDownStatus, FindingRecord, Session } from "../../types";
import { ago, now } from "../../mockShared";
import type { MockState } from "../../mockState";

export type SessionsMockState = {
    burnDown: BurnDownStatus;
    FINDINGS: FindingRecord[];
    colonyActivity: (kind: string, s: Session) => void;
};

export function installSessionsMockState(ms: MockState): void {
  // The burn-down /api/burn-down payload (issue #210): mid-burn, ~1 day from the reset. `setBurnDown`
  // flips the payload below when the operator stops it; the session list carries one `origin:
  // "burn_down"` colony (`sessions.set(burn.session.id, burn)` above) against this same clock.
  ms.burnDown = {
    enabled: true,
    state: "burning",
    estimate: true,
    now: now(),
    next_reset: new Date(Date.now() + 86_400_000).toISOString(),
    window_start: null,
    spent_usd: 140,
    allowance_usd: 200,
    remaining_usd: 60,
    reserve_usd: 10,
    colonies: { live: 1, queued: 1, total: 3 },
    launches_needed: 6,
    launches_done: 3,
  };
  ms.FINDINGS = [
    { session: "demo1234", title: "Checkout fails for guest users", state: "validated", severity: "high", ts: ago(60 * 24 * 6) },
    { session: "demo1234", title: "Checkout fails for guest users", state: "filed", issue: "https://github.com/acme/webshop/issues/88", ts: ago(60 * 24 * 6) },
    { session: "demo1234", title: "Checkout fails for guest users", state: "fix_colony", fix_session: "fix-demo1234-1", ts: ago(60 * 24 * 5) },
    { session: "demo1234", title: "Checkout fails for guest users", state: "review", review_session: "rev-demo1234-1", verdict: "pass", ts: ago(60 * 24 * 4) },
    { session: "demo1234", title: "Checkout fails for guest users", state: "merged", pr: "https://github.com/acme/webshop/pull/215", ts: ago(60 * 24 * 3) },
    { session: "demo1234", title: "Free-shipping threshold shows the cart subtotal", state: "validated", severity: "medium", ts: ago(60 * 24 * 2) },
    { session: "demo1234", title: "Free-shipping threshold shows the cart subtotal", state: "rejected", reason: "does not reproduce on the staging sandbox", ts: ago(60 * 24 * 2) },
  ];

  ms.colonyActivity = (kind: string, s: Session) =>
    ms.logActivity({ kind, actor: "you", via: "cockpit", org: s.org, repo: s.repo, issue: s.issue, colony: s.id, title: s.issue_title, pr_url: s.pr_url });
}
