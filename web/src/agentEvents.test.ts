// The agent event types against the contract: every event docs/agent-events.schema.json defines is a
// variant of AgentEventBody, and every line of the runner's fixture is one of them, so a type added to
// the schema without the web following it fails here rather than reaching the browser untyped.

// The schema and the fixture are read from disk at test time. Those are node builtins — vitest runs in
// plain node and resolves them fine, but this tsconfig types a browser build and carries no
// @types/node, so tsc must look away from exactly these two imports.
// @ts-expect-error node:fs — no @types/node in this browser-facing tsconfig
import { readFileSync } from "node:fs";
// @ts-expect-error node:url — no @types/node in this browser-facing tsconfig
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { initialStreamState, reduceFrame } from "./sessionStream";
import type { AgentEventBody } from "./types";

const read = (path: string): string => readFileSync(fileURLToPath(new URL(path, import.meta.url)), "utf8");

// `satisfies` makes tsc fail on a variant missing here or a key that is not a variant.
const WEB_TYPES = {
  status: true,
  user_message: true,
  assistant_text_delta: true,
  assistant_text: true,
  thinking: true,
  tool_call: true,
  tool_result: true,
  question: true,
  question_answered: true,
  turn_end: true,
  log: true,
  memory_proposal: true,
  finding: true,
} satisfies Record<AgentEventBody["type"], true>;

describe("agent event types", () => {
  it("cover every event the schema defines, and nothing else", () => {
    const schema = JSON.parse(read("../../docs/agent-events.schema.json")) as {
      $defs: Record<string, { properties?: { type?: { const?: string } } }>;
    };
    const schemaTypes = Object.values(schema.$defs)
      .map((def) => def.properties?.type?.const)
      .filter((t): t is string => typeof t === "string");
    expect(schemaTypes.sort()).toEqual(Object.keys(WEB_TYPES).sort());
  });

  it("cover every line of the runner's fixture", () => {
    const lines = read("../../modules/agents/claude-code/test/fixtures/events.jsonl").split("\n").filter((l) => l.trim());
    const types = lines.map((l) => (JSON.parse(l) as { type: string }).type);
    expect(types.filter((t) => !(t in WEB_TYPES))).toEqual([]);
  });

  it("a memory proposal or a finding on the stream moves the watermark and nothing else", () => {
    const bodies: AgentEventBody[] = [
      { type: "memory_proposal", scope: "repo", title: "Use --locked", content: "Run cargo with --locked.", tags: ["tests"] },
      { type: "memory_proposal", title: "Scope defaults to repo", content: "No scope, no tags." },
      { type: "finding", title: "README promises a flag", body: "The flag is gone.", evidence: "A subagent read main.rs." },
    ];
    const start = initialStreamState();
    const end = bodies.reduce((state, body, i) => reduceFrame(state, { seq: i + 1, ...body }), start);
    expect(end).toEqual({ ...start, lastSeq: 3 });
  });
});
