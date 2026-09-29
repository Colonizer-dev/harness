// The merge train block (issue #671) at the bottom of the publish module's settings: one row per
// repository — next up as a link, waiting-on-CI and needs-rebase counts, skipped with its reason,
// the last merge. Rendering through react-dom/server, because this codebase keeps tests off
// jsdom; renderToStaticMarkup runs no effects, so the fetch in MergeTrainSection never fires and
// the block renders nothing — exactly the loading and older-mothership path.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { MergeTrain, visibleRepos } from "./MergeTrain";
import type { MergeTrainRepo } from "../types";

const repo = (overrides: Partial<MergeTrainRepo> = {}): MergeTrainRepo => ({
  repo: "acme/webshop",
  state: "on",
  base: "main",
  base_ci: "green",
  checked_at: "2026-09-28T10:00:00Z",
  last_merge: { pr_url: "https://github.com/acme/webshop/pull/90", at: "2026-09-28T09:00:00Z" },
  prs: [
    { session: "s1", pr_url: "https://github.com/acme/webshop/pull/91", title: "Fix checkout totals", status: "next", reason: "first in line; merges on a green tick" },
    { session: "s2", pr_url: "https://github.com/acme/webshop/pull/92", title: "Add dark mode", status: "waiting_ci", reason: "the pull request's checks are still running" },
    { session: "s3", pr_url: "https://github.com/acme/webshop/pull/93", title: "Rework sitemap", status: "needs_rebase", reason: "the pull request is behind its base branch; the watcher's auto-rebase path brings it up to date" },
    { session: "s4", pr_url: "https://github.com/acme/webshop/pull/94", title: "Bump bundler", status: "skipped", reason: "conflicts with the sitemap rework" },
  ],
  ...overrides,
});

const markup = (repos: MergeTrainRepo[]) => renderToStaticMarkup(<MergeTrain repos={repos} now={new Date("2026-09-28T10:05:00Z")} />);

describe("MergeTrain", () => {
  it("renders the next pull request as a link, with the queue as counts", () => {
    const out = markup([repo()]);
    expect(out).toContain("Merge train");
    expect(out).toContain("acme/webshop");
    expect(out).toContain('href="https://github.com/acme/webshop/pull/91"');
    expect(out).toContain("Fix checkout totals");
    expect(out).toContain("1 waiting on CI");
    expect(out).toContain("1 needs rebase");
    expect(out).toContain("last merged");
    expect(out).toContain("base CI green");
  });

  it("says why a skipped pull request is held out of the train", () => {
    const out = markup([repo()]);
    expect(out).toContain("Bump bundler");
    expect(out).toContain("conflicts with the sitemap rework");
  });

  it("renders nothing when no repository rides the train, and hides rows that are off without pull requests", () => {
    expect(markup([])).toBe("");
    expect(visibleRepos([repo({ state: "off", prs: [] }), repo({ repo: "acme/empty", state: "off", prs: [] })]).map((r) => r.repo)).toEqual([]);
    // But a repository that still has pull requests queued keeps its row even once turned off.
    expect(markup([repo({ state: "off" })])).toContain("acme/webshop");
  });

  it("renders an off repository whose base could not be read without ever saying null", () => {
    const out = markup([
      repo({
        state: "off",
        base: null,
        base_ci: "unknown",
        prs: [{ session: "s1", pr_url: "https://github.com/acme/webshop/pull/91", title: "Fix checkout totals", status: "skipped", reason: "the merge train is off for this repository" }],
      }),
    ]);
    expect(out).toContain("acme/webshop");
    expect(out).toContain("base CI unknown");
    expect(out).not.toContain("null");
  });
});
