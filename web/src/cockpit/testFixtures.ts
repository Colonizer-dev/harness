// Shared fixtures for the cockpit's static-markup tests: one running colony, one working
// settler, one map-refreshing loop, each tweakable per test.
import type { Loop, Session } from "../types";
import type { SubagentView } from "../sessionStream";

export function session(overrides: Partial<Session> = {}): Session {
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
    cleaned_up: false, keep_worktree: false,
    created_at: "2026-09-18T09:00:00Z",
    updated_at: "2026-09-18T09:10:00Z",
    attention: null,
    ...overrides,
  };
}

/** One enabled map loop keeping acme/webshop's architecture map fresh, tweakable per test. */
export function mapLoop(overrides: Partial<Loop> = {}): Loop {
  return {
    id: "loop_m",
    name: "Keep the map of acme/webshop fresh",
    org: "acme",
    repo: "acme/webshop",
    prompt: "",
    cadence: { every: "every_days", days: 14, hour: 1, minute: 0 },
    kind: "map",
    tz_offset_minutes: 0,
    model: null,
    subagent_model: null,
    autopilot: true,
    max_runs: null,
    end_at: null,
    enabled: true,
    next_run_at: null,
    runs: 0,
    last_run: null,
    last_note: null,
    ended_reason: null,
    created_at: "2026-09-20T00:00:00Z",
    ...overrides,
  };
}

export function settlerView(overrides: Partial<SubagentView> = {}): SubagentView {
  return {
    agent: { id: "a1", name: "scout" },
    name: "Scout 1",
    role: "scout",
    state: "working",
    current: { name: "Read", input: { file_path: "src/cockpit/nest.ts" } },
    last: null,
    steps: 1,
    tools: [],
    errors: 0,
    report: "",
    crew: null,
    ...overrides,
  };
}
