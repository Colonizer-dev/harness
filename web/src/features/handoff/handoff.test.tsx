// Hand-off logic (issue #738): the two-agent fixture sessions parse into the launch form's prefill,
// the untrusted-file guards hold, the "Continue locally" commands name the right agent, and the mock
// refuses an upload the way the server does.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import claudeCode from "./fixtures/claude-code.json?raw";
import codex from "./fixtures/codex.json?raw";
import { createMockApi } from "../../mock";
import { HANDOFF_FILE, MAX_HANDOFF_BYTES, continueLocallyCommands, inferAgent, locallyCommandsFor, parseHandoffFile, repoValid } from "./handoff";
import { ContinueLocallyPanel } from "./ContinueLocally";
import type { SimpleTranscript } from "./types";

/** The transcript of a parsed fixture, failing the test rather than returning an error union. */
function ok(text: string): SimpleTranscript {
  const parsed = parseHandoffFile(text);
  if ("error" in parsed) throw new Error(parsed.error);
  return parsed.transcript;
}

describe("parseHandoffFile (issue #738)", () => {
  it("prefills branch and title from a Claude Code Simple JSON export", () => {
    const parsed = parseHandoffFile(claudeCode);
    expect(parsed).toMatchObject({ branch: "fix/guest-checkout", title: "Fix guest checkout for anonymous users" });
    expect(inferAgent(ok(claudeCode))).toBe("claude_code");
  });

  it("prefills branch and title from a Codex Simple JSON export", () => {
    const parsed = parseHandoffFile(codex);
    expect(parsed).toMatchObject({ branch: "main", title: "Add idempotency keys to refunds" });
    expect(inferAgent(ok(codex))).toBe("codex");
  });

  it("refuses a file over the 2 MiB cap before looking at it", () => {
    expect(parseHandoffFile(JSON.stringify({ messages: [], pad: "x".repeat(MAX_HANDOFF_BYTES) }))).toMatchObject({ error: expect.stringMatching(/larger than 2 MiB/) });
  });

  it("refuses a JSON value that is not a Simple JSON object with a messages array", () => {
    const noMessages = /messages array/;
    expect(parseHandoffFile('{"issues": []}')).toMatchObject({ error: expect.stringMatching(noMessages) });
    expect(parseHandoffFile("[1,2,3]")).toMatchObject({ error: expect.stringMatching(noMessages) });
    expect(parseHandoffFile("not json")).toMatchObject({ error: expect.stringMatching(/valid JSON/) });
  });

  it("treats a blank git_branch or title as no prefill", () => {
    expect(parseHandoffFile(JSON.stringify({ git_branch: "  ", title: "", messages: [] }))).toMatchObject({ branch: null, title: null });
  });
});

describe("repoValid", () => {
  it("accepts owner/name and rejects anything else", () => {
    expect([repoValid("acme/webshop"), repoValid(" acme/webshop ")]).toEqual([true, true]);
    expect([repoValid("webshop"), repoValid("acme/webshop/extra"), repoValid("")]).toEqual([false, false, false]);
  });
});

describe("continueLocallyCommands (issue #738)", () => {
  it("fetches and switches the colony branch, then replays the export with the right agent", () => {
    expect(continueLocallyCommands({ branch: "colonizer/issue-42-demo1234", withAgent: "claude_code" })).toEqual([
      "git fetch origin colonizer/issue-42-demo1234 && git switch colonizer/issue-42-demo1234",
      `txcript continue ./${HANDOFF_FILE} --with claude_code`,
    ]);
    expect(locallyCommandsFor({ branch: "colonizer/session-abc", agent: "codex" })[1]).toBe("txcript continue ./colony.json --with codex");
  });

  it("prefers the agent an exported transcript reveals over the colony's own setting", () => {
    expect(locallyCommandsFor({ branch: "colonizer/session-abc", agent: "claude-code" }, ok(codex))[1]).toBe("txcript continue ./colony.json --with codex");
  });
});

describe("ContinueLocallyPanel", () => {
  it("shows the file, the repository and both commands", () => {
    const lines = continueLocallyCommands({ branch: "colonizer/session-abc", withAgent: "codex" });
    const html = renderToStaticMarkup(<ContinueLocallyPanel lines={lines} repo="acme/webshop" />);
    expect(html).toContain(HANDOFF_FILE);
    expect(html).toContain("acme/webshop");
    expect(html).toContain("git fetch origin colonizer/session-abc");
    expect(html).toContain("txcript continue ./colony.json --with codex");
  });
});

describe("mock hand-off (issue #738)", () => {
  const transcript = { git_branch: "main", title: "Continue me", messages: [{ role: "user" as const, content: "hi" }] };

  it("creates a colony from an upload, starting on the named branch", async () => {
    const session = await createMockApi().handoffSession({ repo: "acme/webshop", branch: "fix/guest-checkout", transcript });
    expect(session).toMatchObject({ repo: "acme/webshop", base: "fix/guest-checkout", issue: null, issue_title: "Continue me" });
  });

  it("serves a colony's conversation as Simple JSON for the download", async () => {
    const doc = await createMockApi().sessionHandoff("demo1234");
    expect(doc.git_branch).toBe("colonizer/issue-42-demo1234");
    expect(Array.isArray(doc.messages)).toBe(true);
  });

  it("refuses a bad repository, a non-transcript and an oversize upload, like the server", async () => {
    const api = createMockApi();
    await expect(api.handoffSession({ repo: "webshop", transcript })).rejects.toMatchObject({ status: 400 });
    await expect(api.handoffSession({ repo: "acme/webshop", transcript: {} as never })).rejects.toMatchObject({ status: 400 });
    const big = { messages: [], pad: "x".repeat(MAX_HANDOFF_BYTES) };
    await expect(api.handoffSession({ repo: "acme/webshop", transcript: big as never })).rejects.toMatchObject({ status: 413 });
  });

  it("answers 404 for a colony that does not exist", async () => {
    await expect(createMockApi().sessionHandoff("nope")).rejects.toMatchObject({ status: 404 });
  });
});
