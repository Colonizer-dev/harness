// Shared fixtures for the cockpit's static-markup tests: one running colony, one working
// settler, each tweakable per test.
import type { Session } from "../types";
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
