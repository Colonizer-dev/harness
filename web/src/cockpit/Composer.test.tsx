import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import type { Repo } from "../types";
import { Composer, appendHeard, composerRepos, defaultRepo } from "./Composer";

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
