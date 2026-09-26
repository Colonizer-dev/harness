// The status label and badge are the one place every colony reads its state from, so the
// suspended labels (issue #562) are pinned here: a colony whose microVM is stopped while its
// question is out reads as suspended, and one whose answer is already stored reads as resuming.
// Rendered through react-dom/server, because this codebase keeps tests off jsdom.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { session } from "../cockpit/testFixtures";
import type { Session } from "../types";
import { StatusBadge, occupiesSlot, statusLabel } from "./ui";

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
