// The status label and badge are the one place every colony reads its state from, so the
// suspended labels (issue #562) are pinned here: a colony whose microVM is stopped while its
// question is out reads as suspended, one whose answer is already stored reads as resuming, and
// one that answered while suspended reads as queued for a slot (issue #667).
// Rendered through react-dom/server, because this codebase keeps tests off jsdom.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { session } from "../cockpit/testFixtures";
import type { Session } from "../types";
import { StatusBadge, isAnsweredWaiting, occupiesSlot, ordinal, parkedLabel, restorePlace, statusLabel } from "./ui";

const suspended = {
  at: "2026-09-26T10:00:00Z",
  snapshot: null,
  reason: "waiting_for_answer",
  path: "session_resume",
};

const answered = (answered_at?: string) => ({ question_id: "q1", prompt: "ship it?", answered_at });

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
