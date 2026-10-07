import { describe, expect, it } from "vitest";

import { autoRetrying, expectsAnswer, gatewayHeld, needsYouLine, questionOf, retryLine, watchdogFlagged } from "./questions";
import { session } from "./testFixtures";

describe("questionOf", () => {
  it("takes the question out of a waiting diagnosis", () => {
    expect(questionOf({ state: "waiting_on_human", text: "waiting for an answer: Retry the subagents?" })).toBe("Retry the subagents?");
  });

  it("says nothing when the diagnosis names no question", () => {
    expect(questionOf({ state: "waiting_on_human", text: "waiting for an answer" })).toBeNull();
    expect(questionOf({ state: "waiting_on_human", text: "idle, waiting for the next message" })).toBeNull();
    expect(questionOf({ state: "working", text: "waiting for an answer: stale" } as never)).toBeNull();
    expect(questionOf(null)).toBeNull();
  });
});

describe("watchdogFlagged", () => {
  it("is not the watchdog when the flag only records an open question", () => {
    expect(watchdogFlagged(session({ attention: { reason: "waiting_for_answer", nudges: 0 } as never }))).toBe(false);
    expect(watchdogFlagged(session({ attention: { reason: "stalled", nudges: 2 } as never }))).toBe(true);
    expect(watchdogFlagged(session({ attention: null as never }))).toBe(false);
  });
});

// Issue #1093: a colony held on a gateway error was carded "the watchdog flagged this colony" next to
// "answer", with no question pending. The line names the real cause instead.
const HELD = {
  reason: "autopilot_held",
  since: "2026-10-06T08:00:00Z",
  nudges: 0,
  cause: "gateway_error",
  detail: "Stopped on repeated gateway errors (502, connection to Anthropic); 3 automatic retries did not get through",
} as const;
const RETRYING = {
  reason: "provider_retry",
  since: "2026-10-06T08:00:00Z",
  nudges: 0,
  cause: "gateway_error",
  summary: "Stopped on a model gateway error (502, connection to Anthropic)",
  detail: "Stopped on a model gateway error (502, connection to Anthropic): retrying automatically (attempt 1 of 3)",
  retry_at: "2026-10-06T08:04:00Z",
  attempt: 1,
  max_attempts: 3,
} as const;

describe("watchdogFlagged (issue #1093)", () => {
  it("is the watchdog only for the watchdog's own reasons", () => {
    expect(watchdogFlagged(session({ status: "idle", attention: HELD }))).toBe(false);
    expect(watchdogFlagged(session({ status: "parked", attention: RETRYING }))).toBe(false);
    expect(watchdogFlagged(session({ attention: { reason: "nudges_exhausted", since: "", nudges: 3 } }))).toBe(true);
    expect(watchdogFlagged(session({ attention: { reason: "control_defeat", since: "", nudges: 0 } }))).toBe(true);
  });
});

describe("needsYouLine", () => {
  it("names a gateway hold by its cause, never the watchdog or a question", () => {
    const line = needsYouLine(session({ status: "idle", attention: HELD }));
    expect(line).toBe(HELD.detail);
    expect(line).not.toContain("watchdog");
    expect(line).not.toContain("question");
  });

  it("keeps the watchdog's and the question's own wording where they apply", () => {
    expect(needsYouLine(session({ status: "running", attention: { reason: "stalled", since: "", nudges: 2 } }))).toBe("the watchdog flagged this colony");
    expect(needsYouLine(session({ status: "waiting_for_answer" }))).toBe("the colony asked you a question");
    expect(needsYouLine(session({ status: "waiting_for_answer" }), "the colony is waiting on your answer")).toBe("the colony is waiting on your answer");
  });

  it("an older hold without a cause reads as the hold", () => {
    expect(needsYouLine(session({ status: "idle", attention: { reason: "autopilot_held", since: "", nudges: 0 } }))).toBe("Autopilot held the PR");
  });
});

describe("expectsAnswer", () => {
  it("only a waiting colony is expected to have a question", () => {
    expect(expectsAnswer(session({ status: "waiting_for_answer" }))).toBe(true);
    expect(expectsAnswer(session({ status: "idle", attention: { reason: "waiting_for_answer", since: "", nudges: 0 } }))).toBe(true);
    expect(expectsAnswer(session({ status: "idle", attention: HELD }))).toBe(false);
  });
});

describe("the gateway retry card", () => {
  it("tells a pending automatic retry from a hold whose retries ran out", () => {
    expect(autoRetrying(session({ status: "parked", attention: RETRYING }))).toBe(true);
    expect(gatewayHeld(session({ status: "parked", attention: RETRYING }))).toBe(false);
    expect(autoRetrying(session({ status: "idle", attention: HELD }))).toBe(false);
    expect(gatewayHeld(session({ status: "idle", attention: HELD }))).toBe(true);
  });

  it("counts down to the next attempt", () => {
    const now = Date.parse("2026-10-06T08:00:30Z");
    expect(retryLine(RETRYING, now)).toBe("Stopped on a model gateway error (502, connection to Anthropic): retrying in 4 min");
    expect(retryLine(RETRYING, Date.parse("2026-10-06T08:05:00Z"))).toBe(
      "Stopped on a model gateway error (502, connection to Anthropic): retrying now",
    );
    expect(retryLine({ ...RETRYING, retry_at: undefined, summary: undefined }, now)).toBe("Stopped on a model gateway error: retrying automatically");
  });
});
