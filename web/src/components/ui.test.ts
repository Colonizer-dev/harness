// The parked-colony helpers behind the overview rows: the quota-exhausted attention line, its
// UTC resume-time words, and the parked predicate itself. Pure, so they run as-is in vitest.
import { describe, expect, it } from "vitest";

import { attentionText, formatResumeTime, isParked } from "./ui";
import type { Attention, Session } from "../types";

const parked = (overrides: Partial<Session> = {}): Pick<Session, "attention" | "cleaned_up"> => ({
  attention: { reason: "provider_quota_exhausted", since: "2026-09-18T09:12:00Z", nudges: 0 },
  cleaned_up: false,
  ...overrides,
});

describe("attentionText provider_quota_exhausted", () => {
  it("names the resume time when the upstream error gave one", () => {
    const attention: Attention = {
      reason: "provider_quota_exhausted",
      since: "2026-09-18T09:12:00Z",
      nudges: 0,
      resumes_at: "2026-09-23T07:54:00Z",
    };
    expect(attentionText(attention)).toBe("Out of tokens — parked, resumes 09-23 07:54 UTC");
  });

  it("falls back to the quota reset when no time was named", () => {
    expect(attentionText({ reason: "provider_quota_exhausted", since: "2026-09-18T09:12:00Z", nudges: 0 })).toBe(
      "Out of tokens — parked until the quota resets",
    );
  });

  it("ignores a resume time it cannot parse", () => {
    expect(
      attentionText({ reason: "provider_quota_exhausted", since: "2026-09-18T09:12:00Z", nudges: 0, resumes_at: "soon" }),
    ).toBe("Out of tokens — parked until the quota resets");
  });
});

describe("formatResumeTime", () => {
  it("reads an RFC 3339 timestamp in UTC, whatever the local zone", () => {
    expect(formatResumeTime("2026-09-23T07:54:00Z")).toBe("09-23 07:54 UTC");
  });

  it("is null for absent or unparseable input", () => {
    expect(formatResumeTime(null)).toBeNull();
    expect(formatResumeTime(undefined)).toBeNull();
    expect(formatResumeTime("soon")).toBeNull();
  });
});

describe("isParked", () => {
  it("parks quota-exhausted and hold-timeout flags with a kept worktree", () => {
    expect(isParked(parked())).toBe(true);
    expect(isParked(parked({ attention: { reason: "hold_timeout", since: "2026-09-18T09:12:00Z", nudges: 0 } }))).toBe(true);
  });

  it("is not parked once cleaned up, or for any other reason", () => {
    expect(isParked(parked({ cleaned_up: true }))).toBe(false);
    expect(isParked(parked({ attention: { reason: "stalled", since: "2026-09-18T09:12:00Z", nudges: 1 } }))).toBe(false);
    expect(isParked(parked({ attention: null }))).toBe(false);
  });
});
