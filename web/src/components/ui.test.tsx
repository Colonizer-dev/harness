// The status label and badge are the one place every colony reads its state from, so the
// suspended labels (issue #562) are pinned here: a colony whose microVM is stopped while its
// question is out reads as suspended, one whose answer is already stored reads as resuming, and
// one that answered while suspended reads as queued for a slot (issue #667).
// Rendered through react-dom/server, because this codebase keeps tests off jsdom.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { session } from "../cockpit/testFixtures";
import type { Attention, Session } from "../types";
import { AttentionBadge, StatusBadge, attentionText, autoFixLine, isAnsweredWaiting, occupiesSlot, ordinal, parkedLabel, restorePlace, statusLabel, supersededHeld, supersededTitle } from "./ui";

const suspended = {
  at: "2026-09-26T10:00:00Z",
  snapshot: null,
  reason: "waiting_for_answer",
  path: "session_resume",
};

const answered = (answered_at?: string) => ({ question_id: "q1", prompt: "ship it?", answered_at });

/** A supersession record (issue #673), kept or not. */
const supersededBy = (kept: boolean): NonNullable<Session["superseded"]> => ({
  by: "merged",
  pr_url: "https://github.com/acme/repo/pull/9",
  pr: 9,
  title: "Fix the login",
  reason: "issue",
  at: "2026-09-26T10:30:00Z",
  kept,
});

describe("statusLabel", () => {
  it("keeps the plain status label when the colony is not suspended", () => {
    expect(statusLabel(session({ status: "waiting_for_answer" }))).toBe("Needs your answer");
    expect(statusLabel(session({ status: "running" }))).toBe("Working");
  });

  it("reads a waiting colony whose microVM is stopped as suspended", () => {
    expect(statusLabel(session({ status: "waiting_for_answer", suspended }))).toBe("Suspended — resumes when you answer");
  });

  it("reads a colony whose answer is already stored and a boot underway as resuming", () => {
    for (const status of ["queued", "starting"] as const) {
      const resuming = session({ status, pending_answer: answered() });
      expect(statusLabel(resuming)).toBe("Resuming with your answer");
    }
  });

  it("reads a colony that answered while suspended as queued for a slot, not as waiting on you", () => {
    const held = session({ status: "waiting_for_answer", suspended, pending_answer: answered("2026-09-26T10:05:00Z") });
    expect(statusLabel(held)).toBe("Answered · resumes when a slot frees");
  });

  it("reads an answered colony a merge superseded as held until kept, not as resuming on a free slot (issue #673)", () => {
    const answeredWaiting = { status: "waiting_for_answer" as const, suspended, pending_answer: answered("2026-09-26T10:05:00Z") };
    expect(statusLabel(session({ ...answeredWaiting, superseded: supersededBy(false) }))).toBe("Answered · held until kept");
    expect(statusLabel(session({ ...answeredWaiting, superseded: supersededBy(true) }))).toBe("Answered · resumes when a slot frees");
  });

  it("keeps the suspended label while a colony without a stored answer still waits", () => {
    expect(statusLabel(session({ status: "waiting_for_answer", suspended }))).toBe("Suspended — resumes when you answer");
  });

  it("reads a warm-up (issue #701) as warming while it boots and ready once the VM is up", () => {
    const warming = { requested_at: "2026-09-26T10:05:00Z", started_at: "2026-09-26T10:05:30Z", ready_at: null };
    expect(statusLabel(session({ status: "starting", suspended, prewarm: warming }))).toBe("Warming up…");
    expect(statusLabel(session({ status: "running", suspended, prewarm: { ...warming, ready_at: "2026-09-26T10:07:00Z" } }))).toBe(
      "Ready — waiting for your answer",
    );
  });

  it("reads a warm-up that was only requested — no boot admitted yet — as still suspended", () => {
    const warming = { requested_at: "2026-09-26T10:05:00Z", started_at: null, ready_at: null };
    expect(statusLabel(session({ status: "waiting_for_answer", suspended, prewarm: warming }))).toBe("Suspended — resumes when you answer");
  });

  it("keeps the answered labels when an answer is held during a warm-up", () => {
    const warming = { requested_at: "2026-09-26T10:05:00Z", started_at: "2026-09-26T10:05:30Z", ready_at: null };
    expect(statusLabel(session({ status: "starting", suspended, pending_answer: answered(), prewarm: warming }))).toBe("Resuming with your answer");
  });

  it("ignores a stale suspended flag once the colony is live again", () => {
    expect(statusLabel(session({ status: "running", suspended }))).toBe("Working");
  });
});

describe("isAnsweredWaiting", () => {
  it("is the derived state alone: suspended, answer stored, still waiting_for_answer", () => {
    expect(isAnsweredWaiting(session({ status: "waiting_for_answer", suspended, pending_answer: answered() }))).toBe(true);
  });

  it("is false without any leg of the state", () => {
    expect(isAnsweredWaiting(session({ status: "waiting_for_answer" }))).toBe(false);
    expect(isAnsweredWaiting(session({ status: "waiting_for_answer", suspended }))).toBe(false);
    expect(isAnsweredWaiting(session({ status: "waiting_for_answer", pending_answer: answered() }))).toBe(false);
  });

  it("is false once the colony left the state", () => {
    expect(isAnsweredWaiting(session({ status: "queued", suspended, pending_answer: answered() }))).toBe(false);
    expect(isAnsweredWaiting(session({ status: "running", suspended, pending_answer: answered() }))).toBe(false);
  });
});

describe("restorePlace", () => {
  // A list in answer order; `answered_at` is what the queue reads.
  const waiting = (id: string, at: string, answered_at?: string) =>
    session({ id, status: "waiting_for_answer", suspended: { ...suspended, at }, pending_answer: answered(answered_at) });

  it("ranks the answered by answered_at, oldest first", () => {
    const sessions = [
      waiting("late", "2026-09-26T11:00:00Z", "2026-09-26T11:00:00Z"),
      waiting("early", "2026-09-26T10:00:00Z", "2026-09-26T10:00:00Z"),
      waiting("mid", "2026-09-26T12:00:00Z", "2026-09-26T10:30:00Z"),
    ];
    expect(restorePlace(sessions[1], sessions)).toBe(1);
    expect(restorePlace(sessions[2], sessions)).toBe(2);
    expect(restorePlace(sessions[0], sessions)).toBe(3);
  });

  it("falls back to suspended.at when the answer carries no answered_at yet", () => {
    const sessions = [waiting("b", "2026-09-26T11:00:00Z"), waiting("a", "2026-09-26T10:00:00Z")];
    expect(restorePlace(sessions[0], sessions)).toBe(2);
    expect(restorePlace(sessions[1], sessions)).toBe(1);
  });

  it("counts only answered-waiting colonies: fresh launches and plain suspensions stay out of line", () => {
    const me = waiting("me", "2026-09-26T11:00:00Z", "2026-09-26T11:00:00Z");
    const sessions = [
      session({ id: "unanswered", status: "waiting_for_answer", suspended: { ...suspended, at: "2026-09-26T09:00:00Z" } }),
      session({ id: "fresh", status: "queued", created_at: "2026-09-26T09:30:00Z" }),
      me,
    ];
    expect(restorePlace(me, sessions)).toBe(1);
  });

  it("leaves a superseded colony that is not kept out of the line, and takes it back once kept (issue #673)", () => {
    const held = { ...waiting("held", "2026-09-26T09:00:00Z", "2026-09-26T09:00:00Z"), superseded: supersededBy(false) };
    const me = waiting("me", "2026-09-26T11:00:00Z", "2026-09-26T11:00:00Z");
    expect(restorePlace(held, [held, me])).toBe(null);
    expect(restorePlace(me, [held, me])).toBe(1);
    const kept = { ...held, superseded: supersededBy(true) };
    expect(restorePlace(kept, [kept, me])).toBe(1);
    expect(restorePlace(me, [kept, me])).toBe(2);
  });

  it("is null outside the derived state", () => {
    expect(restorePlace(session({ status: "waiting_for_answer", suspended }), [session({ status: "queued" })])).toBe(null);
    expect(restorePlace(session({ status: "queued", pending_answer: answered() }), [])).toBe(null);
  });
});

describe("ordinal", () => {
  it("suffixes 1st, 2nd and 3rd, then th", () => {
    expect([1, 2, 3, 4, 9, 10, 14, 20].map(ordinal)).toEqual(["1st", "2nd", "3rd", "4th", "9th", "10th", "14th", "20th"]);
  });

  it("leaves 11th through 13th alone, wherever they appear", () => {
    expect([11, 12, 13, 111, 112, 113, 211, 212, 213].map(ordinal)).toEqual([
      "11th", "12th", "13th", "111th", "112th", "113th", "211th", "212th", "213th",
    ]);
  });

  it("keeps the teens' suffix past twenty", () => {
    expect([21, 22, 23, 101].map(ordinal)).toEqual(["21st", "22nd", "23rd", "101st"]);
  });
});

describe("parkedLabel", () => {
  const parked = (overrides: Partial<NonNullable<Session["parked"]>> = {}) => ({
    at: "2026-09-26T10:00:00Z",
    reason: "provider_quota_exhausted",
    vm_kept: true,
    ...overrides,
  });

  it("says the park reason in human words", () => {
    expect(parkedLabel(parked())).toBe("provider quota exhausted");
    expect(parkedLabel(parked({ reason: "hold_timeout" }))).toBe("hold timed out");
  });

  it("names the reset in local words when the provider gave one", () => {
    const line = parkedLabel(parked({ resets_at: "2026-09-27T14:05:00Z" }));
    // The exact clock words shift with the test run's locale and timezone; the shape must not.
    expect(line).toMatch(/^provider quota exhausted · resumes .+/);
    expect(line).toBe(parkedLabel({ ...parked(), resets_at: "2026-09-27T16:05:00+02:00" }));
  });

  it("drops the resume half without a reset, and an unparseable one too", () => {
    expect(parkedLabel(parked({ resets_at: undefined }))).toBe("provider quota exhausted");
    expect(parkedLabel(parked({ resets_at: "not a timestamp" }))).toBe("provider quota exhausted");
  });

  it("shows an unknown reason spelled out, and nothing without a park record", () => {
    expect(parkedLabel(parked({ reason: "something_new" }))).toBe("something new");
    expect(parkedLabel(null)).toBe("");
    expect(parkedLabel(undefined)).toBe("");
  });
});

describe("occupiesSlot", () => {
  it("counts live and publishing colonies, but a suspended colony frees its slot", () => {
    expect(occupiesSlot(session({ status: "running" }))).toBe(true);
    expect(occupiesSlot(session({ status: "waiting_for_answer" }))).toBe(true);
    expect(occupiesSlot(session({ status: "publishing" }))).toBe(true);
    expect(occupiesSlot(session({ status: "waiting_for_answer", suspended }))).toBe(false);
    expect(occupiesSlot(session({ status: "stopped" }))).toBe(false);
    expect(occupiesSlot(session({ status: "queued" }))).toBe(false);
  });

  it("counts a warm-up once its boot is admitted, not while it still queues for a slot (issue #701)", () => {
    const warming = { requested_at: "2026-09-26T10:05:00Z", started_at: "2026-09-26T10:05:30Z", ready_at: null };
    expect(occupiesSlot(session({ status: "starting", suspended, prewarm: warming }))).toBe(true);
    expect(occupiesSlot(session({ status: "waiting_for_answer", suspended, prewarm: { ...warming, started_at: null } }))).toBe(false);
    // A stale warm-up on a colony that is not live holds nothing, whatever the record says.
    expect(occupiesSlot(session({ status: "stopped", prewarm: warming }))).toBe(false);
  });
});

describe("StatusBadge", () => {
  const badge = (overrides: Partial<Session>) => renderToStaticMarkup(<StatusBadge session={session(overrides)} />);

  it("shows the suspended label without the live pulse — the microVM is stopped", () => {
    const out = badge({ status: "waiting_for_answer", suspended });
    expect(out).toContain("Suspended — resumes when you answer");
    expect(out).not.toContain("pulse-soft");
  });

  it("still pulses a plain waiting colony, whose microVM is up", () => {
    const out = badge({ status: "waiting_for_answer" });
    expect(out).toContain("Needs your answer");
    expect(out).toContain("pulse-soft");
  });

  it("drops the needs-you accent once the colony has answered and is queued for a slot", () => {
    const out = badge({ status: "waiting_for_answer", suspended, pending_answer: answered("2026-09-26T10:05:00Z") });
    expect(out).toContain("Answered · resumes when a slot frees");
    expect(out).not.toContain("bg-accent-soft");
    expect(out).not.toContain("pulse-soft");
  });
});

describe("supersededHeld", () => {
  it("is true only for a supersession that is not kept", () => {
    expect(supersededHeld(session({}))).toBe(false);
    expect(supersededHeld(session({ superseded: supersededBy(false) }))).toBe(true);
    expect(supersededHeld(session({ superseded: supersededBy(true) }))).toBe(false);
  });
});

// The supersession tooltip (issue #673) is shared by the colony view's badge and banner and the
// sidebar's compact badge, so the words live in one place.
describe("supersededTitle", () => {
  const superseded = (reason: string, title = "Fix the login"): NonNullable<Session["superseded"]> => ({
    by: "merged",
    pr_url: "https://github.com/acme/repo/pull/9",
    title,
    reason: reason as "files",
    at: "2026-09-28T10:00:00Z",
    kept: false,
  });

  it("names the overlap reason and the colony whose merge covered the work, spelling unknown reasons out", () => {
    expect(supersededTitle(superseded("issue"))).toBe('same issue — covered by "Fix the login"');
    expect(supersededTitle(superseded("supply_chain", "Bump lodash"))).toBe('same supply-chain target — covered by "Bump lodash"');
    expect(supersededTitle(superseded("files"))).toBe('overlapping files — covered by "Fix the login"');
    expect(supersededTitle(superseded("something_new"))).toBe('something new — covered by "Fix the login"');
  });
});

describe("attentionText", () => {
  const attention = (overrides: Partial<Attention>): Attention => ({ reason: "stalled", since: "2026-09-26T10:00:00Z", nudges: 1, ...overrides });

  it("says why an autopilot hold happened when the mothership sent a detail (issue #672)", () => {
    expect(
      attentionText(attention({ reason: "autopilot_held", detail: "`cargo test` exited 101 in a fresh checkout; failing: a::b (last 200 lines in out/verify-cargo-test.log)" })),
    ).toBe("`cargo test` exited 101 in a fresh checkout; failing: a::b (last 200 lines in out/verify-cargo-test.log)");
  });

  it("keeps the plain hold label when there is no detail to show", () => {
    expect(attentionText(attention({ reason: "autopilot_held" }))).toBe("Autopilot held the PR");
    expect(attentionText(attention({ reason: "autopilot_held", detail: "  " }))).toBe("Autopilot held the PR");
  });

  it("leaves other reasons alone even when a detail is present: only a hold reads it", () => {
    expect(attentionText(attention({ reason: "stalled", nudges: 2, detail: "ignored" }))).toBe("No progress, nudged 2×");
  });

  it("names the quota, account and turn-lost flags a queue row can carry (issue #1127)", () => {
    expect(attentionText(attention({ reason: "provider_quota_exhausted" }))).toBe("Provider out of quota — waiting for the plan to refill");
    expect(attentionText(attention({ reason: "waiting_for_account" }))).toBe("Its Claude account needs sign-in again — it resumes when the account works");
    expect(attentionText(attention({ reason: "turn_lost_after_subagent" }))).toBe("Its turn never resumed after a subagent — the watchdog re-drove it");
  });

  it("puts the hold's detail in the attention badge's tooltip beside the suspended-answer badge", () => {
    const detail = "`web: npm test` exited 1 in a fresh checkout; failing: renders the banner";
    const out = renderToStaticMarkup(<AttentionBadge attention={attention({ reason: "autopilot_held", detail })} />);
    expect(out).toContain("title=\"`web: npm test` exited 1 in a fresh checkout; failing: renders the banner\"");
    // The detail does not change the answered-waiting derivation the badge sits next to (issue #667).
    const held = session({ status: "waiting_for_answer", suspended, pending_answer: answered("2026-09-26T10:05:00Z"), attention: attention({ reason: "autopilot_held", detail }) });
    expect(isAnsweredWaiting(held)).toBe(true);
    expect(statusLabel(held)).toBe("Answered · resumes when a slot frees");
  });
});

// Issue #1191: the colony header's line for what the watchdog playbook fixed by itself.
describe("autoFixLine", () => {
  const fix = (signature: string) => ({ signature, action: "send_message", at: "2026-10-07T10:00:00Z", detail: "sent" });

  it("is absent when nothing was fixed", () => {
    expect(autoFixLine({})).toBeNull();
    expect(autoFixLine({ auto_fixes: [] })).toBeNull();
  });

  it("names each signature once, with a count when it repeated", () => {
    expect(autoFixLine({ auto_fixes: [fix("pr_md_write")] })).toBe("auto-fixed: pr_md_write");
    expect(autoFixLine({ auto_fixes: [fix("pr_md_write"), fix("provider_unavailable"), fix("pr_md_write")] })).toBe(
      "auto-fixed: pr_md_write ×2, provider_unavailable",
    );
  });

  it("leaves a looping stop to the attention flag", () => {
    expect(autoFixLine({ auto_fixes: [fix("looping")] })).toBeNull();
    const looping: Attention = { reason: "looping", since: "2026-10-07T10:00:00Z", nudges: 0, signature: "pr_md_write" };
    expect(attentionText(looping)).toBe("Looping on pr_md_write — stopped, resume to continue");
  });
});
