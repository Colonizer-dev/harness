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
  quotaBannerPlans,
  quotaBannerText,
  quotaBannerTitle,
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

/** 2026-10-05 19:51:58 UTC: the reset in the operator's screenshot. */
const RESET = Date.UTC(2026, 9, 5, 19, 51, 58) / 1000;
/** Two hours and ten minutes before it. */
const NOW = (RESET - (2 * 3600 + 10 * 60)) * 1000;

const quota = (overrides: Partial<StatusQuota> = {}): StatusQuota => ({
  paused: true,
  reason: "queue paused — BytePlus plan exhausted, resets 10-05 19:51:58 (3 waiting)",
  reset_at: "10-05 19:51:58",
  reset_unix: RESET,
  providers: ["byteplus"],
  kind: "provider",
  provider_details: [{ id: "byteplus", name: "BytePlus", used_by: ["subagents", "background"] }],
  ...overrides,
});

/** An account-level pause: no exhausted provider is named, so no provider name may show. */
const accountQuota = (overrides: Partial<StatusQuota> = {}): StatusQuota =>
  quota({
    providers: [],
    kind: undefined,
    provider_details: [{ id: "anthropic", name: "Claude", used_by: ["orchestrator"] }],
    ...overrides,
  });

const parked = (id: string, overrides: Partial<Session> = {}) =>
  session({
    id,
    status: "stopped",
    attention: { reason: "provider_quota_exhausted", since: "2026-09-18T09:12:00Z", nudges: 0 },
    ...overrides,
  });

/** How a current mothership says it (issue #213): status `parked` with the park record attached. */
const nativelyParked = (id: string, overrides: Partial<Session> = {}) =>
  session({
    id,
    status: "parked",
    parked: { at: "2026-09-18T09:12:00Z", reason: "provider_quota_exhausted", resets_at: "2026-09-19T07:54:00Z", vm_kept: true },
    ...overrides,
  });

const markup = (quotaValue: StatusQuota | null | undefined, sessions: Session[]) =>
  renderToStaticMarkup(<QuotaBanner quota={quotaValue} sessions={sessions} onResumeAll={() => {}} onDismiss={() => {}} />);

describe("quotaParkedSessions", () => {
  it("parks stopped, quota-flagged colonies with a kept worktree — and nothing else", () => {
    const sessions = [
      parked("parked"),
      nativelyParked("native"),
      // A quota flag on a live colony is not parked; neither is a parked colony already cleaned up.
      session({ id: "live", attention: { reason: "provider_quota_exhausted", since: "2026-09-18T09:12:00Z", nudges: 0 } }),
      parked("cleaned", { cleaned_up: true }),
      nativelyParked("native-cleaned", { cleaned_up: true }),
      // A plain stop (or a stall, or a failure) parks nothing: resume-all must not touch it.
      session({ id: "plain-stop", status: "stopped" }),
      session({ id: "failed", status: "failed", attention: { reason: "provider_quota_exhausted", since: "2026-09-18T09:12:00Z", nudges: 0 } }),
      session({ id: "stalled", status: "stopped", attention: { reason: "stalled", since: "2026-09-18T09:12:00Z", nudges: 1 } }),
    ];
    expect(quotaParkedSessions(sessions).map((s) => s.id)).toEqual(["parked", "native"]);
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

describe("quotaBannerText", () => {
  const text = (q: StatusQuota, parkedCount: number) => quotaBannerText(q, parkedCount, NOW, "UTC");

  it("names a non-Claude provider by its display name, never as Claude", () => {
    const out = text(quota(), 0);
    expect(out).toBe(
      "BytePlus plan limit reached. Used by subagents and background. Resets at 19:51 · in 2 h 10 min. " +
        "Colonies on other providers keep running; new colonies wait in the queue until it resets.",
    );
    expect(out).not.toContain("Claude");
    expect(out).not.toContain("byteplus");
  });

  it("says nothing about paused colonies when none are, and counts them when some are", () => {
    expect(text(quota(), 0)).not.toMatch(/paused colon|0 colonies/);
    expect(text(quota(), 1)).toMatch(/ 1 colony paused\.$/);
    expect(text(quota(), 3)).toMatch(/ 3 colonies paused\.$/);
  });

  it("falls back to the provider catalog's name when an older mothership sends ids only", () => {
    const old = quota({ provider_details: undefined, providers: ["byteplus", "my-proxy"] });
    expect(quotaBannerTitle(old)).toBe("BytePlus and my-proxy plan limits reached");
    expect(text(old, 0)).not.toContain("Used by");
    expect(text(old, 0)).toContain("until they reset");
  });

  it("does not double a name that already says plan", () => {
    const q = quota({ providers: ["qf"], provider_details: [{ id: "qf", name: "Baidu Qianfan Coding Plan", used_by: [] }] });
    expect(quotaBannerTitle(q)).toBe("Baidu Qianfan Coding Plan limit reached");
  });

  it("calls the Claude account's own cap a Claude session limit, with the roles on Claude", () => {
    expect(text(accountQuota(), 2)).toBe(
      "Claude session limit reached. Used by orchestrator. Resets at 19:51 · in 2 h 10 min. " +
        "Colonies on other providers keep running; new colonies wait in the queue until it resets. 2 colonies paused.",
    );
    expect(quotaBannerPlans(accountQuota({ provider_details: undefined }))).toEqual([{ name: "Claude", usedBy: [] }]);
  });

  it("quotes the provider's own reset words without a timestamp, and says when there are none", () => {
    expect(text(quota({ reset_unix: null }), 0)).toContain("Resets 10-05 19:51:58.");
    expect(text(quota({ reset_unix: null, reset_at: null }), 0)).toContain("No reset time given.");
  });

  it("builds the text from structured fields, never from the backend reason string", () => {
    expect(text(quota({ reason: "every provider's quota is exhausted (3 waiting)" }), 1)).not.toContain("waiting)");
  });
});

describe("QuotaBanner", () => {
  it("banners a provider pause with Resume all when colonies are parked", () => {
    const out = markup(quota(), [parked("a"), nativelyParked("b"), session({ id: "live" })]);
    expect(out).toContain('role="status"');
    expect(out).toContain("BytePlus plan limit reached");
    expect(out).toContain("2 colonies paused.");
    expect(out).toContain("Resume all (2)");
    expect(out).toContain("Dismiss");
    expect(out).not.toContain("Claude");
  });

  it("drops Resume all and the count with nothing parked", () => {
    const out = markup(quota(), [session({ id: "live" })]);
    expect(out).toContain("BytePlus plan limit reached");
    expect(out).not.toContain("Resume all");
    expect(out).not.toContain("paused colonies");
    expect(out).not.toContain("0 colonies");
    expect(out).toContain("Dismiss");
  });

  it("renders nothing without a paused quota", () => {
    expect(markup(null, [])).toBe("");
    expect(markup(quota({ paused: false }), [])).toBe("");
    expect(markup(undefined, [])).toBe("");
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
