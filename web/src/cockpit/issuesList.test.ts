import { describe, expect, it } from "vitest";

import type { Session } from "../types";
import { NO_FILTERS, groupByRepo, issueFits, issuePriority, issueState, listOrder, loadHidden, plainSummary, saveHidden, statusCounts, type RepoIssue } from "./issuesList";

const s = (over: Partial<Session>): Session => ({ id: "c", repo: "acme/web", issue: 1, status: "running", pr_url: null, created_at: "2026-10-01T00:00:00Z", updated_at: "2026-10-01T00:00:00Z", ...over }) as unknown as Session;
const i = (number: number, over: Partial<RepoIssue> = {}): RepoIssue => ({ repo: "acme/web", number, title: `Issue ${number}`, body: null, labels: [], author: null, updatedAt: "2026-10-01T00:00:00Z", url: "", ...over });

describe("issueState", () => {
  it("reads where an issue stands off its colonies", () => {
    expect(issueState([], "acme/web", 1).status).toBe("new");
    expect(issueState([s({})], "acme/web", 1)).toMatchObject({ status: "colonizing", live: true });
    expect(issueState([s({ status: "queued" })], "acme/web", 1)).toMatchObject({ status: "colonizing", queued: true });
    expect(issueState([s({ status: "waiting_for_answer" })], "acme/web", 1).status).toBe("blocked");
    expect(issueState([s({ status: "pr_opened", pr_url: "https://x/pr/2" })], "acme/web", 1)).toMatchObject({ status: "pr_open", prUrl: "https://x/pr/2" });
    expect(issueState([s({ status: "merged" })], "acme/web", 1).status).toBe("done");
    expect(issueState([s({ status: "stopped" })], "acme/web", 1).status).toBe("new");
    expect(issueState([s({ issue: 2 })], "acme/web", 1).status).toBe("new");
  });
});

describe("plainSummary", () => {
  it("is one plain line", () => {
    expect(plainSummary({ title: "t", body: "Guests get **Something went wrong** on `Pay`.\n\nSteps:\n1. Open" })).toBe("Guests get Something went wrong on Pay.");
    expect(plainSummary({ title: "t", body: null })).toBe("No description yet");
    expect(plainSummary({ title: "t", body: "x".repeat(300) }, 50)).toHaveLength(50);
  });
});

describe("priority, filters and groups", () => {
  it("reads priority from labels", () => {
    expect(issuePriority(i(1, { labels: [{ name: "P0", color: "" }] }))).toBe("high");
    expect(issuePriority(i(1, { labels: [{ name: "priority: low", color: "" }] }))).toBe("low");
    expect(issuePriority(i(1))).toBe("normal");
  });
  it("filters by text, status, label and priority together", () => {
    const bug = i(5, { title: "Cart badge", labels: [{ name: "bug", color: "" }, { name: "urgent", color: "" }] });
    expect(issueFits(bug, "new", "cart", NO_FILTERS)).toBe(true);
    expect(issueFits(bug, "new", "#5", NO_FILTERS)).toBe(true);
    expect(issueFits(bug, "new", "nope", NO_FILTERS)).toBe(false);
    expect(issueFits(bug, "colonizing", "", { ...NO_FILTERS, statuses: ["new"] })).toBe(false);
    expect(issueFits(bug, "new", "", { ...NO_FILTERS, labels: ["bug"], priority: "high" })).toBe(true);
    expect(issueFits(bug, "new", "", { ...NO_FILTERS, priority: "low" })).toBe(false);
  });
  it("counts, groups and orders", () => {
    expect(statusCounts(["new", "new", "blocked"])).toMatchObject({ new: 2, blocked: 1, done: 0 });
    const g = groupByRepo([i(1), i(2, { repo: "acme/api" }), i(3)]);
    expect(g.map((x) => [x.repo, x.rows.length])).toEqual([["acme/web", 2], ["acme/api", 1]]);
    expect(listOrder({ state: "blocked", updatedAt: "2026-01-01" }, { state: "new", updatedAt: "2026-02-01" })).toBeLessThan(0);
  });
  it("keeps the hidden set in the browser", () => {
    expect([...loadHidden(saveHidden(new Set(["a#1", "b#2"])))]).toEqual(["a#1", "b#2"]);
    expect(loadHidden("garbage").size).toBe(0);
  });
});
