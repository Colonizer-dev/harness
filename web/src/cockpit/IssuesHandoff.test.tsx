import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { Repo, Session } from "../types";
import { Header } from "./Header";
import {
  filterIssues,
  issueKey,
  labelCounts,
  roughIssueCount,
  scopeRepos,
  selectable,
  summarize,
  toggleAll,
  type ScopedIssue,
} from "./IssuesHandoff";

const repo = (full_name: string, open_issues_count: number, pushed_at: string, extra: Partial<Repo> = {}): Repo => ({
  full_name,
  description: null,
  private: false,
  fork: false,
  archived: false,
  open_issues_count,
  pushed_at,
  ...extra,
});

const issue = (repoName: string, number: number, title: string, labels: string[] = []): ScopedIssue => ({
  repo: repoName,
  number,
  title,
  body: null,
  labels: labels.map((name) => ({ name, color: "000000" })),
  author: { login: "someone" },
  updatedAt: `2026-09-2${number % 10}T00:00:00Z`,
  url: `https://github.com/${repoName}/issues/${number}`,
});

const colony = (repoName: string, number: number, status: string): Session =>
  ({ id: `s-${number}`, repo: repoName, issue: number, status }) as unknown as Session;

describe("scopeRepos", () => {
  const repos = [
    repo("acme/old", 3, "2026-09-01T00:00:00Z"),
    repo("acme/new", 5, "2026-09-20T00:00:00Z"),
    repo("acme/gone", 9, "2026-09-21T00:00:00Z", { archived: true }),
    repo("acme/noissues", 2, "2026-09-22T00:00:00Z", { has_issues: false }),
    repo("other/x", 7, "2026-09-23T00:00:00Z"),
  ];

  it("keeps the workspace's live repositories with issues, newest push first", () => {
    expect(scopeRepos(repos, "Acme").map((r) => r.full_name)).toEqual(["acme/new", "acme/old"]);
    expect(scopeRepos(repos, null).map((r) => r.full_name)).toEqual(["other/x", "acme/new", "acme/old"]);
  });

  it("counts GitHub's open issues across the scope", () => {
    expect(roughIssueCount(scopeRepos(repos, "acme"))).toBe(8);
  });
});

describe("issue filters and selection", () => {
  const issues = [issue("acme/web", 1, "Fix login", ["bug"]), issue("acme/web", 2, "Add dark mode", ["feature", "ui"]), issue("acme/api", 12, "Login rate limit", ["bug", "ui"])];

  it("searches title or number and requires every chosen label", () => {
    expect(filterIssues(issues, "login", new Set()).map((i) => i.number)).toEqual([1, 12]);
    expect(filterIssues(issues, "#12", new Set()).map((i) => i.number)).toEqual([12]);
    expect(filterIssues(issues, "", new Set(["bug", "ui"])).map((i) => i.number)).toEqual([12]);
    expect(labelCounts(issues)).toEqual([
      ["bug", 2],
      ["ui", 2],
      ["feature", 1],
    ]);
  });

  it("disables issues a live colony already holds, and frees finished ones", () => {
    const sessions = [colony("acme/web", 1, "running"), colony("acme/web", 2, "merged"), colony("acme/api", 12, "pr_opened")];
    expect(selectable(issues, sessions).map((i) => i.number)).toEqual([2]);
  });

  it("select all toggles the shown selectable issues and keeps hidden picks", () => {
    const hidden = issueKey("acme/other", 99);
    const shown = issues.slice(0, 2);
    const on = toggleAll(new Set([hidden]), shown);
    expect([...on].sort()).toEqual([hidden, "acme/web#1", "acme/web#2"].sort());
    const off = toggleAll(on, shown);
    expect([...off]).toEqual([hidden]);
  });

  it("summarizes a hand-off by outcome", () => {
    expect(
      summarize({
        a: { state: "started", sessionId: "1" },
        b: { state: "queued", sessionId: "2" },
        c: { state: "held", message: "409" },
        d: { state: "error", message: "boom" },
        e: { state: "started", sessionId: "3" },
      }),
    ).toBe("2 started · 1 queued · 1 already held · 1 failed");
  });
});

describe("Header issues button", () => {
  it("shows the GitHub count for the scope and opens a dialog", () => {
    const html = renderToStaticMarkup(
      <Header
        orgs={[]}
        selectedOrg="acme"
        onSelectOrg={() => {}}
        needByOrg={{}}
        statusError={false}
        issues={{
          repos: [repo("acme/web", 12, "2026-09-20T00:00:00Z"), repo("other/x", 40, "2026-09-20T00:00:00Z")],
          org: "acme",
          sessions: [],
          githubConnected: true,
          autopilotDefault: true,
          onCreated: () => {},
          onOpenColony: () => {},
        }}
      />,
    );
    expect(html).toContain('aria-haspopup="dialog"');
    expect(html).toMatch(/aria-label="about 12 open issues/);
    expect(html).toContain(">12</span>");
  });
});
