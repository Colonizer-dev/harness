// The merge-train loop card (issue #754): the opt-in helpers, the settings form and the last
// report. Rendered through react-dom/server like the rest of the cockpit's tests (no jsdom).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import type { MergeLoopReport, MergeLoopView } from "../types";
import { MergeLoopPanel, MergeLoopReportView } from "./MergeLoopCard";
import { actionLabel, defaultMergeLoopSettings, parseNames, reportRows, repoOptIn, setRepoCap, setRepoNever, setRepoOptIn, toggleRepos } from "./mergeLoop";

const report = (overrides: Partial<MergeLoopReport> = {}): MergeLoopReport => ({
  started_at: "2026-09-30T09:00:00Z",
  finished_at: "2026-09-30T09:20:00Z",
  dry_run: false,
  forced_dry_run: false,
  stopped: null,
  api_calls: 42,
  summary: "merged 1 · updated (CI running) 1 · red 1 · redo dispatched 1 · skipped 1",
  lines: [],
  repos: [
    {
      repo: "acme/web",
      main: "green at abc12345",
      paused: null,
      heal: [],
      items: [
        { session: "s4", pr_url: "https://github.com/acme/web/pull/4", title: "Draft thing", action: "skipped", reason: "the pull request is a draft" },
        { session: "s1", pr_url: "https://github.com/acme/web/pull/1", title: "Fix totals", action: "merged", reason: "squash-merged: behind main by 0" },
        { session: "s3", pr_url: "https://github.com/acme/web/pull/3", title: "Add lint", action: "red", reason: "failing: unit" },
        { session: "s2", pr_url: "https://github.com/acme/web/pull/2", title: "Dark mode", action: "updated", reason: "updated onto main; its CI was still running after 20 min" },
        { session: "s5", pr_url: "https://github.com/acme/web/pull/5", title: "Rework", action: "redo_dispatched", reason: "needs_redo (the mechanical rebase onto main conflicted)" },
      ],
    },
  ],
  ...overrides,
});

const view = (overrides: Partial<MergeLoopView> = {}): MergeLoopView => ({
  settings: defaultMergeLoopSettings(),
  next_run_at: null,
  running: false,
  writes_blocked: false,
  repos: {},
  last_report: null,
  history: [],
  ...overrides,
});

describe("merge-train loop settings", () => {
  it("defaults to off, hourly, nothing opted in, and every brake on", () => {
    const d = defaultMergeLoopSettings();
    expect(d.enabled).toBe(false);
    expect(d.cadence).toEqual({ every: "interval", minutes: 60 });
    expect(d.allow).toEqual([]);
    expect([d.max_merges, d.cooldown_secs, d.ci_wait_minutes]).toEqual([4, 120, 20]);
    expect([d.self_heal, d.revert_on_red, d.redo_on_conflict]).toEqual([false, false, false]);
    expect([d.resolve_conflicts, d.fix_red]).toEqual([false, false]);
    expect([d.resolve_attempts, d.fix_attempts]).toEqual([3, 2]);
  });

  it("switches a repository in and out, with never beating the allowlist and an org entry covering its repositories", () => {
    let s = defaultMergeLoopSettings();
    expect(repoOptIn(s, "acme/web")).toBe("off");
    s = setRepoOptIn(s, "Acme/Web", true);
    expect(s.allow).toEqual(["acme/web"]);
    expect(repoOptIn(s, "acme/web")).toBe("on");
    s = setRepoNever(s, "acme/web", true);
    expect(repoOptIn(s, "acme/web")).toBe("never");
    expect(s.allow).toEqual([]);
    s = setRepoOptIn(s, "acme/web", true);
    expect(s.never).toEqual([]);
    expect(repoOptIn({ ...s, allow: ["acme"] }, "acme/api")).toBe("org");
    expect(setRepoOptIn(s, "acme/web", false).allow).toEqual([]);
  });

  it("sets and clears a per-repository cap, and lists every repository worth a toggle", () => {
    let s = setRepoCap(defaultMergeLoopSettings(), "acme/web", 2);
    expect(s.repo_max_merges).toEqual({ "acme/web": 2 });
    s = setRepoCap(s, "acme/web", null);
    expect(s.repo_max_merges).toEqual({});
    expect(toggleRepos({ ...s, allow: ["acme", "other/fork"], never: ["up/stream"] }, ["acme/web"])).toEqual(["acme/web", "other/fork", "up/stream"]);
    expect(parseNames("e2e*, lint,, lint ")).toEqual(["e2e*", "lint"]);
  });
});

describe("merge-train loop report", () => {
  it("orders what moved first and reads each action, real or dry", () => {
    const rows = reportRows(report());
    expect(rows.map((r) => `${r.pr} ${r.label}`)).toEqual(["#1 merged", "#2 updated (CI running)", "#5 redo dispatched", "#3 red", "#4 skipped"]);
    expect(actionLabel("merged", true)).toBe("would merge");
    expect(actionLabel("skipped", true)).toBe("skipped");
  });

  it("renders the counts, every pull request with its reason, and a pause", () => {
    const out = renderToStaticMarkup(
      <MergeLoopReportView
        report={report({
          stopped: null,
          repos: [{ ...report().repos[0], paused: "main went red after the train merged https://github.com/acme/web/pull/1" }],
        })}
        now={Date.parse("2026-09-30T10:00:00Z")}
      />,
    );
    expect(out).toContain("merged 1 · updated (CI running) 1 · red 1 · redo dispatched 1 · skipped 1");
    expect(out).toContain('href="https://github.com/acme/web/pull/3"');
    expect(out).toContain("failing: unit");
    expect(out).toContain("the pull request is a draft");
    expect(out).toContain("paused — main went red");
  });

  it("says a forced dry run was the kill switch", () => {
    const out = renderToStaticMarkup(<MergeLoopReportView report={report({ dry_run: true, forced_dry_run: true })} />);
    expect(out).toContain("Last dry run");
    expect(out).toContain("COLONIZER_NO_EXTERNAL_EFFECTS");
    expect(out).toContain("Would merge");
  });

  it("files pull requests by what happened and puts identical reasons on one line", () => {
    const billing = "GitHub Actions did not start the checks: the job was not started because recent account payments have failed or your spending limit needs to be increased.";
    const waiting = Array.from({ length: 11 }, (_, i) => ({ session: `k${i}`, pr_url: `https://github.com/kontinuum-ai/kontinuum/pull/${300 + i}`, title: `PR ${i}`, action: "waiting" as const, reason: billing }));
    const out = renderToStaticMarkup(<MergeLoopReportView report={report({ repos: [{ repo: "kontinuum-ai/kontinuum", main: "green", paused: null, heal: [], items: waiting }] })} />);
    expect(out).toContain("11 PRs in kontinuum-ai/kontinuum");
    expect(out).toContain("GitHub Actions is blocked (billing)");
    expect(out).toContain('href="https://github.com/organizations/kontinuum-ai/settings/billing"');
    expect(out.split("payments have failed").length - 1).toBe(0);
  });
});

describe("MergeLoopPanel", () => {
  const panel = (v: MergeLoopView, draft = v.settings, dirty = false) =>
    renderToStaticMarkup(
      <ApiContext.Provider value={{} as Api}>
        <MergeLoopPanel view={v} draft={draft} repoNames={["acme/web", "acme/api"]} dirty={dirty} busy={false} open onChange={() => {}} onSave={() => {}} onRun={() => {}} />
      </ApiContext.Provider>,
    );

  it("shows the built-in loop off, with a dry run and no report yet", () => {
    const out = panel(view());
    expect(out).toContain("Merge train");
    expect(out).toContain("Built-in");
    expect(out).toContain("Not set up: add a repository");
    expect(out).toContain("Dry run");
    expect(out).toContain("Not run yet");
    expect(out).not.toContain(">Save changes<");
  });

  it("shows the cadence, the opted-in count, repository toggles and the last report", () => {
    const settings = { ...defaultMergeLoopSettings(), enabled: true, allow: ["acme/web"], never: ["acme/api"] };
    const out = panel(view({ settings, last_report: report(), repos: { "acme/web": { paused: "main is red", needs_redo: {} } } }), settings, true);
    expect(out).toContain("Every hour");
    expect(out).toContain("1 repository");
    expect(out).toContain("acme/web");
    expect(out).toContain("acme/api");
    expect(out).toContain("paused");
    expect(out).toContain(">Save changes<");
    expect(out).toContain("Fix totals");
    expect(out).toContain("Fix red pull requests with the colony");
    expect(out).toContain("Fix attempts");
  });

  it("warns when external writes are blocked", () => {
    expect(panel(view({ writes_blocked: true }))).toContain("every run is a dry run");
  });
});
