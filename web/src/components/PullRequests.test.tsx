// The merge steward's Pull requests list (issue #1172): the state of each pull request in words,
// Merge now only where it can work, and the org banner for blocked Actions. Rendered to static
// markup like the rest of the cockpit's tests (no jsdom).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { StewardBanner } from "../cockpit/StewardBanner";
import type { StewardPr } from "../types";
import { PHASES, PullRequestRows, canMergeNow, prLabel } from "./PullRequests";

const now = new Date("2026-10-07T12:00:00Z");

const pr = (overrides: Partial<StewardPr> = {}): StewardPr => ({
  session: "c1",
  repo: "acme/api",
  url: "https://github.com/acme/api/pull/7",
  title: "Fix the cart",
  colony_status: "pr_opened",
  state: "waiting",
  reason: "its checks are still running",
  since: "2026-10-07T11:50:00Z",
  ...overrides,
});

const rows = (prs: StewardPr[], busy: string | null = null) =>
  renderToStaticMarkup(<PullRequestRows prs={prs} busy={busy} onMerge={() => {}} now={now} />);

describe("PullRequestRows", () => {
  it("says every state in words", () => {
    expect(Object.keys(PHASES)).toEqual(["waiting", "merging", "rebasing", "fixing", "ci_blocked", "needs_attention"]);
    for (const [state, { label }] of Object.entries(PHASES)) {
      expect(rows([pr({ state: state as StewardPr["state"] })])).toContain(`>${label}</span>`);
    }
  });

  it("names the pull request, why it is where it is, and how long ago", () => {
    const out = rows([pr()]);
    expect(out).toContain("acme/api#7");
    expect(out).toContain('href="https://github.com/acme/api/pull/7"');
    expect(out).toContain("its checks are still running");
    expect(out).toContain("10m ago");
    expect(out).toContain('aria-label="Merge acme/api#7 now"');
  });

  it("offers Merge now only to a finished colony the steward is not already merging", () => {
    expect(canMergeNow(pr())).toBe(true);
    expect(canMergeNow(pr({ state: "merging" }))).toBe(false);
    expect(canMergeNow(pr({ colony_status: "running", state: "fixing" }))).toBe(false);
    expect(rows([pr({ state: "fixing", colony_status: "running" })])).toContain('disabled=""');
    expect(rows([pr()])).not.toContain('disabled=""');
  });

  it("disables every button while one merge is in flight", () => {
    expect(rows([pr()], "https://github.com/acme/api/pull/7")).toContain('disabled=""');
  });

  it("says so when an org has no open pull requests", () => {
    expect(rows([])).toContain("No open pull requests");
  });

  it("labels a URL that is not a pull request as itself", () => {
    expect(prLabel("https://example.com/x")).toBe("https://example.com/x");
  });
});

describe("StewardBanner", () => {
  const blocked = {
    org: "Kontinuum-ai",
    since: "2026-10-07T11:00:00Z",
    prs: 2,
    reason: "every failed job ended in under 10s with no steps",
    message: "GitHub Actions is blocked for Kontinuum-ai (billing or spending limit); its checks fail without running.",
  };

  it("renders one line per blocked org", () => {
    const out = renderToStaticMarkup(<StewardBanner steward={{ ci_blocked: [blocked, { ...blocked, org: "World-360", prs: 1, message: "GitHub Actions is blocked for World-360 (billing or spending limit); its checks fail without running." }] }} />);
    expect(out).toContain("GitHub Actions is blocked for Kontinuum-ai");
    expect(out).toContain("2 pull requests wait");
    expect(out).toContain("GitHub Actions is blocked for World-360");
    expect(out).toContain("1 pull request waits");
  });

  it("renders nothing when no org is blocked, or on an older mothership", () => {
    expect(renderToStaticMarkup(<StewardBanner steward={{ ci_blocked: [] }} />)).toBe("");
    expect(renderToStaticMarkup(<StewardBanner steward={undefined} />)).toBe("");
  });
});
