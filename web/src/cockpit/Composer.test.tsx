import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import type { Issue, Repo } from "../types";
import { session } from "./testFixtures";
import { Composer, MicButton, appendHeard, composerRepos, defaultRepo, mentionedIssue, suggestedIssues } from "./Composer";

const repo = (full_name: string, pushed_at: string | null, archived = false): Repo => ({
  full_name,
  description: null,
  private: false,
  fork: false,
  archived,
  open_issues_count: 0,
  pushed_at,
});

const repos = [repo("acme/old", "2026-01-01"), repo("acme/new", "2026-09-01"), repo("octo/site", "2026-09-10"), repo("acme/gone", "2026-09-20", true)];

describe("composerRepos", () => {
  it("offers the workspace's unarchived repositories, freshest first", () => {
    expect(composerRepos(repos, "ACME").map((r) => r.full_name)).toEqual(["acme/new", "acme/old"]);
    expect(composerRepos(repos, null).map((r) => r.full_name)).toEqual(["octo/site", "acme/new", "acme/old"]);
  });
});

describe("defaultRepo", () => {
  const choices = composerRepos(repos, "acme");
  it("keeps the last repository launched on while it is in scope, else the freshest", () => {
    expect(defaultRepo(choices, "acme/old")).toBe("acme/old");
    expect(defaultRepo(choices, "octo/site")).toBe("acme/new");
    expect(defaultRepo([], null)).toBeNull();
  });
});

describe("appendHeard", () => {
  it("joins dictation onto what was typed with one space", () => {
    expect(appendHeard("", " fix the login bug ")).toBe("fix the login bug");
    expect(appendHeard("Fix the login bug  ", "and add a test")).toBe("Fix the login bug and add a test");
    expect(appendHeard("typed", "  ")).toBe("typed");
  });
});

const issue = (number: number, updatedAt: string): Issue => ({ number, title: `Issue ${number}`, body: null, labels: [], author: null, updatedAt, url: "" });
const issues = [issue(1, "2026-09-01"), issue(2, "2026-09-20"), issue(3, "2026-09-10")];

describe("mentionedIssue", () => {
  it("links a #number the repository has open, and ignores the rest", () => {
    expect(mentionedIssue("fix #3 please", issues)?.number).toBe(3);
    expect(mentionedIssue("(#2) then", issues)?.number).toBe(2);
    expect(mentionedIssue("fix #99", issues)).toBeNull();
    expect(mentionedIssue("colour#3", issues)).toBeNull();
  });
});

describe("suggestedIssues", () => {
  it("suggests the freshest open issues that no live colony holds", () => {
    const sessions = [session({ repo: "acme/api", issue: 2, status: "running" }), session({ id: "b", repo: "acme/api", issue: 3, status: "merged" })];
    expect(suggestedIssues(issues, sessions, "acme/api").map((i) => i.number)).toEqual([3, 1]);
    expect(suggestedIssues(issues, [], "acme/api", 2).map((i) => i.number)).toEqual([2, 3]);
  });
});

describe("Composer", () => {
  const render = (githubConnected = true) =>
    renderToStaticMarkup(
      <ApiContext.Provider value={{} as Api}>
        <Composer org={null} repos={repos} githubConnected={githubConnected} autopilotDefault onCreated={() => {}} />
      </ApiContext.Provider>,
    );

  it("rests as a quiet pill with its shortcut", () => {
    const html = render();
    expect(html).toContain("Describe a task for a new colony…");
    expect(html).toContain("⌘K");
    expect(html).toContain('data-open="false"');
  });
});

describe("MicButton", () => {
  it("names the service it will use, and says when it is busy", () => {
    const idle = renderToStaticMarkup(<MicButton listening={false} label="Groq · whisper-large-v3-turbo" onClick={() => {}} />);
    expect(idle).toContain('aria-label="speak a task (Groq · whisper-large-v3-turbo)"');
    expect(idle).toContain('title="Speak — Groq · whisper-large-v3-turbo"');
    const busy = renderToStaticMarkup(<MicButton listening={false} busy label="Groq" onClick={() => {}} />);
    expect(busy).toContain('title="Transcribing…"');
    expect(busy).toContain("disabled");
    expect(renderToStaticMarkup(<MicButton listening label="x" onClick={() => {}} />)).toContain('aria-pressed="true"');
  });
});
