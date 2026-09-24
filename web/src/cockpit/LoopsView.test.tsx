import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Loop } from "../types";
import { LoopBadge, bodyOf, isLoopColony } from "./LoopsView";

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
  });
});
