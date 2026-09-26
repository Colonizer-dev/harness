import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { Api } from "../api";
import { ApiContext } from "../context";
import { mockDrafts } from "../mock";
import type { CreatedIssue, NewSessionRequest, Repo, Session } from "../types";
import {
  COLONIZE_ORIGIN,
  ColonizeButton,
  ColonizePane,
  ColonizeProvider,
  DRAFT_START,
  dispatchIssues,
  draftReducer,
  draftRepo,
  fileDrafts,
  filterIssues,
  isColonizeShortcut,
  issueKey,
  keptDrafts,
  labelCounts,
  mergeIssues,
  preselect,
  roughIssueCount,
  scopeRepos,
  selectable,
  summarize,
  toggleAll,
  type DraftState,
  type ScopedIssue,
} from "./Colonize";

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

const actions = {
  repos: [repo("acme/web", 12, "2026-09-20T00:00:00Z"), repo("acme/api", 3, "2026-09-19T00:00:00Z"), repo("other/x", 40, "2026-09-20T00:00:00Z")],
  org: "acme",
  sessions: [] as Session[],
  githubConnected: true,
  autopilotDefault: true,
  onCreated: () => {},
  onOpenColony: () => {},
};

describe("the Colonize button", () => {
  it("is titled Colonize, carries the ant and the scope's GitHub count, and opens a dialog", () => {
    const html = renderToStaticMarkup(
      <ColonizeProvider {...actions}>
        <ColonizeButton />
      </ColonizeProvider>,
    );
    expect(html).toContain(">Colonize</span>");
    expect(html).toContain("ant-glyph");
    expect(html).not.toContain("Send colonies");
    expect(html).toContain('aria-haspopup="dialog"');
    expect(html).toMatch(/aria-label="Colonize · about 15 open issues/);
    expect(html).toContain(">15</span>");
  });

  it("renders nothing outside a provider", () => {
    expect(renderToStaticMarkup(<ColonizeButton />)).toBe("");
  });
});

const web = (n: number, title: string, labels: string[] = []): ScopedIssue => ({ ...issue("acme/web", n, title, labels), updatedAt: `2026-09-${String(10 + (n % 15)).padStart(2, "0")}T00:00:00Z` });

const pane = (extra: Partial<Parameters<typeof ColonizePane>[0]> = {}) =>
  renderToStaticMarkup(
    <ApiContext.Provider value={{} as Api}>
      <ColonizePane
        {...actions}
        scope={scopeRepos(actions.repos, "acme")}
        onLoadedCount={() => {}}
        onClose={() => {}}
        preloaded={{ "acme/web": Array.from({ length: 12 }, (_, i) => web(i + 1, `Web task ${i + 1}`, i % 3 === 0 ? ["bug"] : [])), "acme/api": [issue("acme/api", 3, "Api task")] }}
        {...extra}
      />
    </ApiContext.Provider>,
  );

describe("the Colonize pane", () => {
  it("shows the free-form box above the issue list, paged ten at a time", () => {
    const html = pane();
    expect(html).toContain('aria-label="colonize"');
    expect(html).toContain(">Colonize</h2>");
    expect(html).toMatch(/<textarea[^>]*aria-label="describe new work"/);
    expect(html).toContain("Dispatch right after creating");
    expect(html).toMatch(/checked=""\/>Dispatch right after creating/);
    expect(html).toContain("Draft issues");
    expect(html).toContain("Launch without an issue");
    // The box comes first, then the list.
    expect(html.indexOf("describe new work")).toBeLessThan(html.indexOf('aria-label="issues"'));
    // 13 issues across the scope, ten on the first page.
    expect(html).toContain("13 shown · 0 selected");
    expect(html.match(/id="colonize-acme-/g)?.length).toBe(10);
    expect(html).toContain("1–10 of 13");
    expect(html).toContain('aria-label="filter by label"');
    expect(html).toContain("Dispatch 0 colonies");
  });

  it("asks which repository when the scope has several and none is chosen", () => {
    const confirm: DraftState = {
      text: "Add dark mode",
      stage: { step: "confirm", drafts: [{ id: 0, title: "Add dark mode", body: "Toggle in the header.", keep: true }], repo: null, note: "Drafted by mock/summary" },
      error: null,
    };
    const html = pane({ initialDraft: confirm });
    expect(html).toContain('aria-label="confirm drafted issues"');
    expect(html).toContain("One issue drafted");
    expect(html).toContain("Drafted by mock/summary");
    expect(html).toContain("Which repository?");
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>.*Create 1 issue and dispatch/);
    expect(html).toContain('value="Add dark mode"');
    expect(html).toContain("Toggle in the header.");
  });

  it("offers the drafts for an edit and a create once the repository is known", () => {
    const confirm: DraftState = {
      text: "two things",
      stage: {
        step: "confirm",
        drafts: [
          { id: 0, title: "Fix the login", body: "", keep: true },
          { id: 1, title: "Add dark mode", body: "", keep: true },
        ],
        repo: "acme/web",
        note: null,
      },
      error: null,
    };
    const html = pane({ initialDraft: confirm });
    expect(html).toContain("2 issues drafted");
    expect(html).toContain('aria-label="create draft 2"');
    expect(html).toMatch(/<option value="acme\/web" selected="">/);
    expect(html).toMatch(/<button type="button" class="ant-glyph-host[^"]*">.*Create 2 issues and dispatch/);
  });
});

describe("⌘K", () => {
  const key = (extra: Partial<Parameters<typeof isColonizeShortcut>[0]> = {}) => ({ metaKey: true, ctrlKey: false, altKey: false, key: "k", defaultPrevented: false, target: null, ...extra });
  it("opens Colonize from anywhere but the code editor and the terminal", () => {
    expect(isColonizeShortcut(key())).toBe(true);
    expect(isColonizeShortcut(key({ metaKey: false, ctrlKey: true, key: "K" }))).toBe(true);
    expect(isColonizeShortcut(key({ altKey: true }))).toBe(false);
    expect(isColonizeShortcut(key({ key: "j" }))).toBe(false);
    expect(isColonizeShortcut(key({ defaultPrevented: true }))).toBe(false);
    const inside = (selector: string) => ({ closest: (s: string) => (s.includes(selector) ? {} : null) }) as unknown as EventTarget;
    expect(isColonizeShortcut(key({ target: inside(".monaco-editor") }))).toBe(false);
    expect(isColonizeShortcut(key({ target: inside(".xterm") }))).toBe(false);
    expect(isColonizeShortcut(key({ target: { closest: () => null } as unknown as EventTarget }))).toBe(true);
  });
});

describe("from text to issues to colonies", () => {
  it("picks the pane's repository, or the scope's only one, and otherwise asks", () => {
    const two = scopeRepos(actions.repos, "acme");
    expect(draftRepo(two, "acme/api")).toBe("acme/api");
    expect(draftRepo(two, "*")).toBeNull();
    expect(draftRepo(two.slice(0, 1), "*")).toBe("acme/web");
  });

  it("steps write → drafting → confirm → creating → write, keeping edits and dropping unticked drafts", () => {
    let s = draftReducer(DRAFT_START, { type: "drafting" });
    expect(s).toBe(DRAFT_START); // nothing to draft yet
    s = draftReducer(s, { type: "text", text: "- fix the login\n- add dark mode" });
    s = draftReducer(s, { type: "drafting" });
    expect(s.stage.step).toBe("drafting");
    s = draftReducer(s, { type: "drafted", drafts: [{ title: "Fix the login", body: "a" }, { title: "Add dark mode", body: "b" }], repo: null, note: null });
    expect(s.stage.step).toBe("confirm");
    // No repository yet: Create does nothing.
    expect(draftReducer(s, { type: "creating" })).toBe(s);
    s = draftReducer(s, { type: "repo", repo: "acme/web" });
    s = draftReducer(s, { type: "edit", id: 0, patch: { title: "Fix the guest login" } });
    s = draftReducer(s, { type: "edit", id: 1, patch: { keep: false } });
    if (s.stage.step !== "confirm") throw new Error("not confirming");
    expect(keptDrafts(s.stage.drafts).map((d) => d.title)).toEqual(["Fix the guest login"]);
    s = draftReducer(s, { type: "creating" });
    expect(s.stage.step).toBe("creating");
    s = draftReducer(s, { type: "created", failed: [] });
    expect(s).toEqual(DRAFT_START);
  });

  it("keeps only the drafts that failed to file, so a retry does not file the rest twice", () => {
    let s: DraftState = {
      text: "x",
      stage: {
        step: "creating",
        drafts: [
          { id: 0, title: "A", body: "", keep: true },
          { id: 1, title: "B", body: "", keep: true },
        ],
        repo: "acme/web",
        note: null,
      },
      error: null,
    };
    s = draftReducer(s, { type: "created", failed: [{ title: "B", message: "gh: 502" }] });
    if (s.stage.step !== "confirm") throw new Error("not back on confirm");
    expect(keptDrafts(s.stage.drafts).map((d) => d.title)).toEqual(["B"]);
    expect(s.error).toBe("B: gh: 502");
    expect(s.text).toBe("x");
  });

  it("files the drafts, puts them first in the list pre-selected, and dispatches exactly those", async () => {
    const filed: { repo: string; title: string }[] = [];
    const launched: NewSessionRequest[] = [];
    let next = 100;
    const api = {
      createIssue: async (repo: string, body: { title: string; body: string }): Promise<CreatedIssue> => {
        if (body.title === "Broken") throw new Error("gh could not file the issue");
        filed.push({ repo, title: body.title });
        const number = next++;
        return { repo, number, title: body.title, url: `https://github.com/${repo}/issues/${number}` };
      },
      createSession: async (req: NewSessionRequest): Promise<Session> => {
        launched.push(req);
        return { id: `c${req.issue}`, status: req.issue === 101 ? "queued" : "starting" } as unknown as Session;
      },
    };
    const now = new Date("2026-09-26T12:00:00Z");
    const out = await fileDrafts(api, "acme/web", [{ title: "Fix the login", body: "a" }, { title: "Broken", body: "" }, { title: "Add dark mode", body: "b" }], now);
    expect(filed.map((f) => f.title)).toEqual(["Fix the login", "Add dark mode"]);
    expect(out.created.map((i) => [i.repo, i.number, i.title])).toEqual([
      ["acme/web", 100, "Fix the login"],
      ["acme/web", 101, "Add dark mode"],
    ]);
    expect(out.failed).toEqual([{ title: "Broken", message: "gh could not file the issue" }]);

    // The new issues sort first in the list and are the whole selection.
    const list = mergeIssues([web(1, "old"), web(2, "older")], out.created);
    expect(list.slice(0, 2).map((i) => i.number)).toEqual([100, 101]);
    expect(mergeIssues(list, out.created)).toHaveLength(4);
    const picked = preselect(out.created);
    expect([...picked]).toEqual(["acme/web#100", "acme/web#101"]);

    const heard: string[] = [];
    const results = await dispatchIssues(api, list.filter((i) => picked.has(issueKey(i.repo, i.number))), { instructions: "  keep it small ", autopilot: false }, (key, r) => heard.push(`${key}:${r.state}`));
    expect(launched).toEqual([
      { repo: "acme/web", issue: 100, title: "Fix the login", instructions: "keep it small", autopilot: false, origin: COLONIZE_ORIGIN },
      { repo: "acme/web", issue: 101, title: "Add dark mode", instructions: "keep it small", autopilot: false, origin: COLONIZE_ORIGIN },
    ]);
    expect(heard).toEqual(["acme/web#100:started", "acme/web#101:queued"]);
    expect(summarize(results)).toBe("1 started · 1 queued");
  });

  it("the mock drafts one issue per listed task, else one from the text", () => {
    expect(mockDrafts("please make the export a zip. With images.").issues.map((d) => d.title)).toEqual(["Make the export a zip"]);
    expect(mockDrafts("Two things:\n- fix the login\n- add dark mode").issues.map((d) => d.title)).toEqual(["Fix the login", "Add dark mode"]);
  });
});
