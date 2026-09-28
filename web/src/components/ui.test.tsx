// The status label and badge are the one place every colony reads its state from, so the
// suspended labels (issue #562) are pinned here: a colony whose microVM is stopped while its
// question is out reads as suspended, and one whose answer is already stored reads as resuming.
// Rendered through react-dom/server, because this codebase keeps tests off jsdom.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { session } from "../cockpit/testFixtures";
import type { Session } from "../types";
import { StatusBadge, occupiesSlot, parkedLabel, statusLabel, supersededTitle } from "./ui";

const suspended = {
  at: "2026-09-26T10:00:00Z",
  snapshot: null,
  reason: "waiting_for_answer",
  path: "session_resume",
};

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
      const resuming = session({ status, pending_answer: { text: "ship it" } });
      expect(statusLabel(resuming)).toBe("Resuming with your answer");
    }
  });

  it("keeps the suspended label while the colony still waits with its answer stored", () => {
    const held = session({ status: "waiting_for_answer", suspended, pending_answer: { text: "ship it" } });
    expect(statusLabel(held)).toBe("Suspended — resumes when you answer");
  });

  it("ignores a stale suspended flag once the colony is live again", () => {
    expect(statusLabel(session({ status: "running", suspended }))).toBe("Working");
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
