import { describe, it, expect } from "vitest";
import { leadModelLabel } from "./modelLabel";
import type { Session } from "./types";

function mockSession(overrides?: Partial<Session>): Session {
  return {
    id: "test-id",
    repo: "test/repo",
    org: "test-org",
    issue: 1,
    issue_title: "Test",
    status: "idle",
    branch: "test-branch",
    base: "main",
    worktree: "test-worktree",
    git_admin_dir: null,
    sandbox: "test-sandbox",
    mesh: null,
    agent: "test-agent",
    autopilot: false,
    pr_url: null,
    error: null,
    cost_usd: null,
    cleaned_up: false,
    keep_worktree: false,
    created_at: "2024-01-01T00:00:00Z",
    updated_at: "2024-01-01T00:00:00Z",
    ...overrides,
  };
}

describe("leadModelLabel", () => {
  it("returns null when neither model nor tier is present", () => {
    expect(leadModelLabel(mockSession())).toBeNull();
    expect(leadModelLabel(mockSession({ model_routing: null }))).toBeNull();
  });

  it("returns 'model · tier' when both are present", () => {
    const session = mockSession({
      model_routing: { model: "claude-opus-5-5", tier: "high" },
    });
    expect(leadModelLabel(session)).toBe("claude-opus-5-5 · high");
  });

  it("returns 'module model · tier' when only tier is present in model_routing", () => {
    const session = mockSession({
      model_routing: { tier: "medium" },
    });
    expect(leadModelLabel(session)).toBe("module model · medium");
  });

  it("returns model alone when only model_routing.model is present", () => {
    const session = mockSession({
      model_routing: { model: "claude-opus-5-5" },
    });
    expect(leadModelLabel(session)).toBe("claude-opus-5-5");
  });

  it("falls back to model_tier when model_routing.tier is absent", () => {
    const session = mockSession({
      model_tier: "low",
      model_routing: { model: "claude-opus-5-5" },
    });
    expect(leadModelLabel(session)).toBe("claude-opus-5-5 · low");
  });
});
