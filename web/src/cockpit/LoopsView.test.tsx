import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Api } from "../api";
import { ApiContext } from "../context";
import type { Loop } from "../types";
import { LoopBadge, LoopDialog, bodyOf, isLoopColony } from "./LoopsView";
import { describeLoop, endAtError, endAtFromInput, endAtInputValue } from "./loops";

const loop: Loop = {
  id: "loop_a",
  name: "Triage",
  org: "acme",
  repo: "acme/web",
  prompt: "Triage new issues",
  cadence: { every: "daily", hour: 7, minute: 0 },
  tz_offset_minutes: 120,
  model: null,
  subagent_model: null,
  autopilot: true,
  max_runs: 5,
  end_at: null,
  enabled: true,
  next_run_at: "2026-09-25T07:00:00Z",
  runs: 2,
  last_run: { session: "abc", at: "2026-09-24T07:00:00Z" },
  last_note: null,
  ended_reason: null,
  created_at: "2026-09-20T00:00:00Z",
};

describe("LoopsView", () => {
  it("badges only colonies a loop launched", () => {
    expect(isLoopColony({ origin: "loop:loop_a" })).toBe(true);
    expect(isLoopColony({ origin: "burn_down" })).toBe(false);
    expect(renderToStaticMarkup(<LoopBadge session={{ origin: "loop:loop_a" }} />)).toContain("loop");
    expect(renderToStaticMarkup(<LoopBadge session={{ origin: null }} />)).toBe("");
  });

  it("builds a full-replace body that keeps every setting and applies a change", () => {
    const body = bodyOf(loop, { enabled: false });
    expect(body).toMatchObject({ name: "Triage", repo: "acme/web", cadence: loop.cadence, max_runs: 5, enabled: false, autopilot: true });
    // A full replace must not quietly turn a map loop back into a colony loop.
    expect(bodyOf({ ...loop, kind: "map", repo: "acme/*" })).toMatchObject({ kind: "map", repo: "acme/*" });
    expect(bodyOf(loop).kind).toBe("colony");
  });

  it("carries the loop's end date in the form's Ends field", () => {
    // A local wall-clock time, so the rendered value does not shift with the test machine's zone.
    const end_at = new Date("2026-09-25T09:00").toISOString();
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={{} as Api}>
        <LoopDialog loop={{ ...loop, end_at }} org="acme" orgs={[]} repos={[]} onSave={async () => {}} onClose={() => {}} />
      </ApiContext.Provider>,
    );
    expect(html).toContain('type="datetime-local"');
    expect(html).toContain('aria-label="Ends"');
    expect(html).toContain('value="2026-09-25T09:00"');
  });

  it("converts the Ends field to the end_at it submits, and clears it when empty", () => {
    const iso = endAtFromInput("2026-09-25T09:00");
    expect(iso).toMatch(/Z$/);
    expect(endAtInputValue(iso)).toBe("2026-09-25T09:00");
    expect(endAtFromInput("")).toBeNull(); // an empty Ends clears the loop's end date
    expect(endAtInputValue(null)).toBe("");
  });

  it("refuses an end that is not after now", () => {
    const now = new Date("2026-09-25T08:00").getTime();
    expect(endAtError("2026-09-25T09:00", now)).toBeNull();
    expect(endAtError("2026-09-25T08:00", now)).toMatch(/after now/);
    expect(endAtError("2026-09-24T09:00", now)).toMatch(/after now/);
    expect(endAtError("", now)).toBeNull(); // no end date is always allowed
  });

  it("shows the day a dated loop ends in the list", () => {
    const end_at = new Date("2026-09-25T09:00").toISOString();
    expect(describeLoop({ ...loop, end_at })).toContain("ends 25 Sep 2026");
    expect(describeLoop(loop)).not.toContain("ends");
  });
});
