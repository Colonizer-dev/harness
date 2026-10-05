// The cockpit's mirror of the exec policy save check (issue #924), driven by the fixture the runner's
// parsePolicy and the mothership's Rust port share, so the form refuses exactly what the server will.
// @ts-expect-error node:fs — no @types/node in this browser-facing tsconfig
import { readFileSync } from "node:fs";
// @ts-expect-error node:url — no @types/node in this browser-facing tsconfig
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { execPolicyProblem } from "./execPolicy";

const fixture = JSON.parse(
  readFileSync(fileURLToPath(new URL("../../modules/agents/claude-code/test/fixtures/execpolicy-valid.json", import.meta.url)), "utf8"),
) as { cases: { policy: string; valid: boolean }[] };

describe("execPolicyProblem", () => {
  it("accepts exactly the shared fixture's valid policies", () => {
    expect(fixture.cases.length).toBeGreaterThan(10);
    // Blank is no policy at all in the form, not an invalid one.
    for (const { policy, valid } of fixture.cases.filter((c) => c.policy.trim() !== ""))
      expect(execPolicyProblem(policy) === null, policy).toBe(valid);
  });

  it("names the problem in the server's words", () => {
    expect(execPolicyProblem("")).toBeNull();
    expect(execPolicyProblem("{not json")).toMatch(/^the exec policy is not valid JSON/);
    expect(execPolicyProblem('{"rules": [{"id": "x", "decision": "maybe", "command": "ls"}]}')).toBe(
      'exec policy rule "x": "decision" must be "deny", "ask" or "allow"',
    );
    expect(execPolicyProblem('{"rules": [{"decision": "deny"}]}')).toBe(
      'exec policy rule 1: a rule needs "command", "script", "touches" or "writes_outside", or it would match every command',
    );
  });
});
