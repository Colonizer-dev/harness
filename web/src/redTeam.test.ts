// The red-team helpers (issue #212): every run state has a label and a tone, the gate message
// is honest about the live count and prefers the server's reason, and the raid target resolves
// hunters against the session list by session_id.
import { describe, expect, it } from "vitest";

import { RED_TEAM_STATE, RED_TEAM_SYNTHESIS, gateMessage, isActive, isGated, isRaiding, liveCount, raidTarget } from "./redTeam";
import type { RedTeamRun, Session } from "./types";

const STATES = ["armed", "waiting", "running", "draining", "done", "stopped"] as const;
const SYNTH_STATES = ["pending", "running", "done", "failed"] as const;
const TONES = ["neutral", "info", "ok", "warn", "err", "accent"];

function run(over: Partial<RedTeamRun> = {}): RedTeamRun {
  return {
    id: "rt-1", repo: "acme/webshop", org: "acme", state: "running", swarm_size: 3, modules: [],
    autofix: false, hunters: [], counts: { found: 0, validated: 0, rejected: 0, filed: 0, merged: null },
    created_at: "2026-09-20T09:00:00Z", started_at: null, ended_at: null, gate_reason: null, synthesis: null, ...over,
  };
}

function session(id: string, status: Session["status"] = "running"): Session {
  return {
    id, repo: "acme/webshop", org: "acme", issue: 42, issue_title: "Checkout fails for guest users",
    status, branch: `colonizer/issue-42-${id}`, base: "main", parent: null,
    worktree: `/home/you/wt/${id}`, git_admin_dir: null, sandbox: `colony-${id}`, mesh: null,
    agent: "claude-code", autopilot: false, pr_url: null, error: null, cost_usd: null, cleaned_up: false, keep_worktree: false,
    created_at: "2026-09-20T08:00:00Z", updated_at: "2026-09-20T08:00:00Z",
  };
}

describe("RED_TEAM_STATE", () => {
  it("covers all six states with a label and a valid tone", () => {
    for (const state of STATES) {
      expect(typeof RED_TEAM_STATE[state].label).toBe("string");
      expect(TONES).toContain(RED_TEAM_STATE[state].tone);
    }
    expect(RED_TEAM_STATE.armed.tone).toBe("warn");
    expect(RED_TEAM_STATE.running.tone).toBe("err");
    expect(RED_TEAM_STATE.done.tone).toBe("ok");
  });
});

describe("RED_TEAM_SYNTHESIS", () => {
  it("covers all four states with a label and a valid tone", () => {
    for (const state of SYNTH_STATES) {
      expect(typeof RED_TEAM_SYNTHESIS[state].label).toBe("string");
      expect(TONES).toContain(RED_TEAM_SYNTHESIS[state].tone);
    }
    expect(RED_TEAM_SYNTHESIS.pending.tone).toBe("warn");
    expect(RED_TEAM_SYNTHESIS.done.tone).toBe("ok");
    expect(RED_TEAM_SYNTHESIS.failed.tone).toBe("err");
  });
});

describe("liveCount", () => {
  it("counts only sessions whose microVM is up", () => {
    expect(liveCount([session("a", "running"), session("b", "idle"), session("c", "stopped"), session("d", "pr_opened")])).toBe(2);
  });

  it("is zero for an empty nest", () => {
    expect(liveCount([])).toBe(0);
  });
});

describe("gateMessage", () => {
  it("prefers the server's own reason", () => {
    expect(gateMessage(run({ state: "armed", gate_reason: "a raid is already running on this repo" }), 2)).toBe(
      "a raid is already running on this repo",
    );
  });

  it("states the live count when the server said nothing", () => {
    expect(gateMessage(run({ state: "armed" }), 2)).toBe("armed — waiting for the nest to empty (2 live)");
  });
});

describe("state classification", () => {
  it("raids while running or draining, is gated while armed or waiting, active until terminal", () => {
    for (const state of STATES) {
      expect(isRaiding(run({ state }))).toBe(state === "running" || state === "draining");
      expect(isGated(run({ state }))).toBe(state === "armed" || state === "waiting");
      expect(isActive(run({ state }))).toBe(state !== "done" && state !== "stopped");
    }
  });
});

describe("raidTarget", () => {
  const hunters = [
    { session_id: "demo1234", title: "guest checkout", module: "checkout", version: "1.2.0", focus: "cart" },
    { session_id: "stall5678", title: "dark mode email", module: "email", version: null, focus: "templates" },
    { session_id: "gone9999", title: "who?", module: "x", version: null, focus: "y" },
  ];

  it("joins hunters to their sessions by session_id", () => {
    const found = raidTarget(run({ hunters }), [session("demo1234"), session("stall5678"), session("other")]).map((s) => s.id);
    expect(found).toEqual(["demo1234", "stall5678"]);
  });

  it("is empty when no hunter has a session", () => {
    expect(raidTarget(run({ hunters }), [])).toEqual([]);
  });
});