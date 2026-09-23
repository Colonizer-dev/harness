// The cockpit-global quota banner (issue #404): it banners a paused quota with words built from
// structured fields (never the backend's reason string), stays hidden without one, dismisses per
// pause (a new reset or scope re-shows it; a waiting-count change does not), and resume-all reaches
// exactly the parked colonies. Rendered to static markup: the test environment has no DOM, so the
// button clicks are pinned through the pure helpers instead.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Session, StatusQuota } from "../types";
import {
  QuotaBanner,
  dismissQuotaBanner,
  quotaBannerKey,
  quotaBannerText,
  quotaParkedSessions,
  quotaPauseKind,
  resumeQuotaParkedSessions,
  visibleQuotaBanner,
} from "./QuotaBanner";

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "s1",
    repo: "acme/webshop",
    org: "acme",
    issue: 42,
    issue_title: "Checkout fails for guest users",
    status: "running",
    branch: "colonizer/issue-42-s1",
    base: "main",
    parent: null,
    worktree: "/wt/s1",
    git_admin_dir: "/git/s1",
    sandbox: "colony-s1",
    mesh: null,
    agent: "claude-code",
    autopilot: false,
    pr_url: null,
    error: null,
    cost_usd: null,
    cleaned_up: false,
    keep_worktree: false,
    created_at: "2026-09-18T09:00:00Z",
    updated_at: "2026-09-18T09:10:00Z",
    attention: null,
    ...overrides,
  };
}

const quota = (overrides: Partial<StatusQuota> = {}): StatusQuota => ({
  paused: true,
  reason: "Claude session limit reached",
  reset_at: "09-23 07:54 UTC",
  reset_unix: 1_789_000_000,
  providers: ["anthropic"],
  ...overrides,
});

/** An account-level pause: no exhausted provider is named, so no provider name may show. */
const accountQuota = (overrides: Partial<StatusQuota> = {}): StatusQuota =>
  quota({ providers: [], ...overrides });

const parked = (id: string, overrides: Partial<Session> = {}) =>
  session({
    id,
    status: "stopped",
    attention: { reason: "provider_quota_exhausted", since: "2026-09-18T09:12:00Z", nudges: 0 },
    ...overrides,
  });

const markup = (quotaValue: StatusQuota | null | undefined, sessions: Session[]) =>
  renderToStaticMarkup(<QuotaBanner quota={quotaValue} sessions={sessions} onResumeAll={() => {}} onDismiss={() => {}} />);

describe("quotaParkedSessions", () => {
  it("parks stopped, quota-flagged colonies with a kept worktree — and nothing else", () => {
    const sessions = [
      parked("parked"),
      // A quota flag on a live colony is not parked; neither is a parked colony already cleaned up.
      session({ id: "live", attention: { reason: "provider_quota_exhausted", since: "2026-09-18T09:12:00Z", nudges: 0 } }),
      parked("cleaned", { cleaned_up: true }),
      // A plain stop (or a stall, or a failure) parks nothing: resume-all must not touch it.
      session({ id: "plain-stop", status: "stopped" }),
      session({ id: "failed", status: "failed", attention: { reason: "provider_quota_exhausted", since: "2026-09-18T09:12:00Z", nudges: 0 } }),
      session({ id: "stalled", status: "stopped", attention: { reason: "stalled", since: "2026-09-18T09:12:00Z", nudges: 1 } }),
    ];
    expect(quotaParkedSessions(sessions).map((s) => s.id)).toEqual(["parked"]);
  });
});

describe("quotaPauseKind", () => {
  it("derives provider scope from a named provider, account scope from none", () => {
    expect(quotaPauseKind(quota())).toBe("provider");
    expect(quotaPauseKind(accountQuota())).toBe("account");
  });

  it("prefers a backend kind field when one arrives", () => {
    expect(quotaPauseKind({ ...quota(), kind: "account" } as StatusQuota)).toBe("account");
    expect(quotaPauseKind({ ...accountQuota(), kind: "provider" } as StatusQuota)).toBe("provider");
  });
});

describe("QuotaBanner", () => {
  it("banners a provider pause with its reset words, provider names, and parked-colony count", () => {
    const out = markup(quota(), [parked("a"), parked("b"), session({ id: "live" })]);
    expect(out).toContain('role="status"');
    expect(out).toContain("Claude session limit reached on anthropic — resets 09-23 07:54 UTC. 2 paused colonies.");
    expect(out).toContain("Resume all (2)");
    expect(out).toContain("Dismiss");
  });

  it("builds the text from structured fields, never from the backend reason string", () => {
    // The reason carries the live waiting count and backend phrasing: neither may leak in.
    expect(quotaBannerText(quota({ reason: "every provider's quota is exhausted (3 waiting)" }), 1)).toBe(
      "Claude session limit reached on anthropic — resets 09-23 07:54 UTC. 1 paused colony.",
    );
    const out = markup(quota({ reason: "Claude session limit reached (3 waiting)" }), []);
    expect(out).not.toContain("(3 waiting)");
    expect(out).not.toContain("every provider's quota is exhausted");
  });

  it("names no providers for an account pause, and defaults the reset when none is named", () => {
    expect(quotaBannerText(accountQuota({ reset_at: null, reset_unix: null }), 0)).toBe(
      "Claude session limit reached — resets soon. 0 paused colonies.",
    );
    const out = markup(accountQuota({ reason: null, reset_at: null, reset_unix: null }), []);
    expect(out).toContain("Claude session limit reached — resets soon. 0 paused colonies.");
    expect(out).not.toContain("anthropic");
  });

  it("renders nothing without a paused quota, and disables resume-all with nothing parked", () => {
    expect(markup(null, [])).toBe("");
    expect(markup(quota({ paused: false }), [])).toBe("");
    expect(markup(undefined, [])).toBe("");
    const out = markup(quota(), [session({ id: "live" })]);
    expect(out).toContain("0 paused colonies.");
    expect(out).toContain("disabled");
    expect(out).not.toContain("Resume all (");
  });
});

describe("quota banner dismissal", () => {
  it("hides the banner until the pause changes: a new reset or scope is a new key", () => {
    const paused = quota();
    let dismissed: ReadonlySet<string> = new Set();
    expect(visibleQuotaBanner(paused, dismissed)).toBe(paused);
    dismissed = dismissQuotaBanner(dismissed, paused);
    expect(visibleQuotaBanner(paused, dismissed)).toBeNull();

    // The same pause stays hidden; a new reset re-shows it under a new key.
    expect(visibleQuotaBanner({ ...paused }, dismissed)).toBeNull();
    const reset = quota({ reset_at: "09-24 07:54 UTC", reset_unix: 1_789_086_400 });
    expect(quotaBannerKey(reset)).not.toBe(quotaBannerKey(paused));
    expect(visibleQuotaBanner(reset, dismissed)).toBe(reset);

    // A new pause scope re-shows it too: the same reset as an account pause is a new key.
    const account = accountQuota();
    expect(quotaBannerKey(account)).not.toBe(quotaBannerKey(paused));
    expect(visibleQuotaBanner(account, dismissed)).toBe(account);

    expect(visibleQuotaBanner(null, dismissed)).toBeNull();
    expect(visibleQuotaBanner(quota({ paused: false }), dismissed)).toBeNull();
  });

  it("keeps the key stable while the waiting count moves inside the reason", () => {
    const paused = quota({ reason: "Claude session limit reached (2 waiting)" });
    const dismissed = dismissQuotaBanner(new Set(), paused);
    // The count ticks up: same reset, same scope, same key — the banner stays hidden.
    expect(visibleQuotaBanner(quota({ reason: "Claude session limit reached (3 waiting)" }), dismissed)).toBeNull();
    expect(quotaBannerKey(quota({ reason: "something else entirely" }))).toBe(quotaBannerKey(paused));
  });
});

describe("resumeQuotaParkedSessions", () => {
  it("resumes exactly the parked ids, and settles per colony so one failure cannot block the rest", async () => {
    const resumed: string[] = [];
    const sessions = [parked("a"), parked("b"), session({ id: "plain-stop", status: "stopped" })];
    const settled = await resumeQuotaParkedSessions(sessions, async (id) => {
      resumed.push(id);
      if (id === "a") throw new Error("409 gone");
      return id;
    });
    expect(resumed).toEqual(["a", "b"]);
    expect(settled.map((r) => r.status)).toEqual(["rejected", "fulfilled"]);
  });

  it("resolves empty with nothing parked, without calling resume", async () => {
    let calls = 0;
    const settled = await resumeQuotaParkedSessions([session({ id: "live" })], async () => {
      calls += 1;
    });
    expect(settled).toEqual([]);
    expect(calls).toBe(0);
  });
});
