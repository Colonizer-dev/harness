import { describe, expect, it } from "vitest";

import type { Repo, Session } from "../../types";
import type { RepoIssue } from "../issuesList";
import { RECENTS_KEY, buildListing, classifyIntent, loadRecents, matchScore, moveSelection, nextSection, recallAsk, remember, words, type Result, type SpotlightData } from "./logic";

const session = (id: string, repo: string, issue: number, status: string, title: string): Session => ({ id, repo, issue, status, issue_title: title, created_at: "2026-10-01T00:00:00Z", updated_at: "2026-10-01T00:00:00Z" }) as unknown as Session;
const issue = (repo: string, number: number, title: string): RepoIssue => ({ repo, number, title, body: null, labels: [], author: null, updatedAt: "2026-10-01T00:00:00Z", url: "" });
const repo = (full_name: string): Repo => ({ full_name, description: null, private: false, fork: false, archived: false, open_issues_count: 3, pushed_at: "2026-10-01T00:00:00Z" });

const data = (extra: Partial<SpotlightData> = {}): SpotlightData => ({
  sessions: [session("c1", "acme/web", 7, "running", "Fix the checkout"), session("c2", "acme/web", 8, "queued", "Dark mode"), session("c3", "acme/api", 9, "stopped", "Rate limits")],
  repos: [repo("acme/web"), repo("acme/api")],
  orgs: ["acme"],
  issues: [issue("acme/web", 212, "Search ignores the size filter"), issue("acme/web", 45, "Show stock levels")],
  loops: [],
  chats: [],
  settings: [],
  updateAvailable: false,
  org: null,
  recents: [],
  ...extra,
});

describe("classifyIntent", () => {
  it("reads what the person means", () => {
    expect(classifyIntent("why is omarchy slow")).toBe("ask");
    expect(classifyIntent("is the queue stuck?")).toBe("ask");
    expect(classifyIntent("colonize #212")).toBe("do");
    expect(classifyIntent("stop checkout")).toBe("do");
    expect(classifyIntent("settings providers")).toBe("go");
    expect(classifyIntent("webshop")).toBe("go");
    expect(classifyIntent("the colonies that failed overnight on the api repo")).toBe("ask");
    expect(classifyIntent("")).toBe("go");
  });
});

describe("matchScore", () => {
  it("ranks a title that starts with the word over one that only contains it", () => {
    const ws = words("check");
    expect(matchScore("Checkout fails", ws)).toBeGreaterThan(matchScore("Fix the flaky checkout", ws));
    expect(matchScore("Fix the flaky checkout", ws)).toBeGreaterThan(matchScore("Support discount codes at xcheckout", ws));
    expect(matchScore("Nothing here", ws)).toBe(0);
    expect(matchScore("Alpha beta", words("alpha gamma"))).toBe(0);
  });
});

describe("buildListing", () => {
  it("leads with where you can go for a name, and always ends on the ask row", () => {
    const l = buildListing("checkout", data());
    expect(l.intent).toBe("go");
    expect(l.results[l.top].id).toBe("go:colony:c1");
    expect(l.results.at(-1)?.section).toBe("ask");
    expect(l.results.at(-1)?.title).toBe("Ask Colonizer: checkout");
  });

  it("picks the ask row for a sentence", () => {
    const l = buildListing("why is the checkout colony slow", data());
    expect(l.intent).toBe("ask");
    expect(l.results[l.top].section).toBe("ask");
  });

  it("turns 'colonize #212' into a launch held for approval, ahead of everything else", () => {
    const l = buildListing("colonize #212", data());
    const top = l.results[l.top];
    expect(top.section).toBe("do");
    expect(top.writes).toBe(true);
    expect(top.action).toEqual({ type: "propose", tool: "launch_colony", args: { repo: "acme/web", issue: 212 } });
    expect(l.results[0].id).toBe(top.id);
  });

  it("offers stop, resume and move-to-front on the right colonies", () => {
    const stop = buildListing("stop checkout", data()).results[0];
    expect(stop.action).toEqual({ type: "propose", tool: "stop_colony", args: { id: "c1" } });
    const resume = buildListing("resume rate", data()).results[0];
    expect(resume.action).toEqual({ type: "propose", tool: "resume_colony", args: { id: "c3" } });
    const front = buildListing("move dark to front", data()).results[0];
    expect(front.action).toEqual({ type: "propose", tool: "move_to_front", args: { id: "c2" } });
  });

  it("finds open issues by title and by number, and keeps Colonize reachable under Do", () => {
    expect(buildListing("size filter", data()).results.some((r) => r.id === "issue:acme/web#212")).toBe(true);
    const byNumber = buildListing("#45", data());
    expect(byNumber.results.find((r) => r.section === "issues")?.title).toBe("Show stock levels");
    const colonize = buildListing("colonize", data()).results.find((r) => r.id === "do:colonize");
    expect(colonize?.action.type).toBe("colonize");
  });

  it("shows recents and suggestions when the box is empty", () => {
    const recents = remember([], { id: "go:colony:c1", section: "go", title: "Fix the checkout", icon: "colony", score: 1, action: { type: "colony", id: "c1" } }, 1);
    const l = buildListing("", data({ recents }));
    expect(l.results[0].section).toBe("recent");
    expect(l.results.some((r) => r.section === "do")).toBe(true);
  });

  it("offers the update only when there is one", () => {
    expect(buildListing("update", data()).results.some((r) => r.id === "do:update")).toBe(false);
    expect(buildListing("update", data({ updateAvailable: true })).results.some((r) => r.id === "do:update")).toBe(true);
  });
});

describe("keyboard helpers", () => {
  const rows = (...sections: Result["section"][]): Result[] => sections.map((section, i) => ({ id: String(i), section, title: "", icon: "ask", score: 0, action: { type: "ask", text: "" } }));
  it("wraps arrow keys and cycles sections with Tab", () => {
    expect(moveSelection(0, 3, -1)).toBe(2);
    expect(moveSelection(2, 3, 1)).toBe(0);
    const r = rows("go", "go", "do", "issues", "ask");
    expect(nextSection(r, 0)).toBe(2);
    expect(nextSection(r, 2)).toBe(3);
    expect(nextSection(r, 4)).toBe(0);
    expect(nextSection(r, 0, true)).toBe(4);
  });
});

describe("recents", () => {
  it("keeps one entry per thing, newest first, and recalls asks with Up", () => {
    const ask = (text: string): Result => ({ id: "ask", section: "ask", title: `Ask Colonizer: ${text}`, icon: "ask", score: 0, action: { type: "ask", text } });
    let list = remember([], ask("why is omarchy slow"), 1);
    list = remember(list, ask("what failed today"), 2);
    list = remember(list, ask("Why is omarchy slow"), 3);
    expect(list.map((r) => r.title)).toEqual(["Why is omarchy slow", "what failed today"]);
    expect(recallAsk(list)).toBe("Why is omarchy slow");
    expect(recallAsk(list, 1)).toBe("what failed today");
    expect(loadRecents("not json")).toEqual([]);
    expect(loadRecents(JSON.stringify(list))).toHaveLength(2);
    expect(RECENTS_KEY).toContain("spotlight");
  });
});
