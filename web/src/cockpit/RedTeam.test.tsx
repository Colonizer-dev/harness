// The red-team wizard, history and workspace-row buttons that replaced the Overview's red-team card,
// plus the Compare switch's empty-previous-period reading. Static markup (no jsdom): steps and
// schedules are pinned through props, since nothing here can click.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import type { OrgEntry } from "../orgs";
import type { RedTeamRun, RedTeamSchedule, Session } from "../types";
import { HistoryBody } from "./RedTeamHistory";
import { WizardBody } from "./RedTeamWizard";
import { OverviewView } from "./OverviewView";
import { compareDelta } from "./dash";
import { describeCadence, estimateCost, runCost, toUtcCadence } from "./redTeamPlan";

const api = createMockApi();
const noop = () => {};

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "s1",
    repo: "acme/webshop",
    org: "acme",
    issue: 42,
    issue_title: "Checkout fails",
    status: "merged",
    branch: "b",
    base: "main",
    parent: null,
    worktree: "/wt",
    git_admin_dir: "/git",
    sandbox: "sb",
    mesh: null,
    agent: "claude-code",
    autopilot: false,
    pr_url: null,
    error: null,
    cost_usd: 2,
    cleaned_up: false,
    keep_worktree: false,
    created_at: new Date(Date.now() - 86_400_000).toISOString(),
    updated_at: new Date().toISOString(),
    attention: null,
    ...overrides,
  };
}

function run(overrides: Partial<RedTeamRun> = {}): RedTeamRun {
  return {
    id: "rt_1",
    repo: "acme/webshop",
    org: "acme",
    state: "done",
    swarm_size: 2,
    modules: ["general"],
    autofix: false,
    hunters: [
      { session_id: "h1", title: "hunter 1", module: "general", version: null, focus: "input validation and injection" },
      { session_id: "h2", title: "hunter 2", module: "general", version: null, focus: "concurrency and race conditions" },
    ],
    counts: { found: 5, validated: 3, rejected: 1, filed: 2 },
    created_at: "2026-09-20T10:00:00Z",
    started_at: "2026-09-20T10:00:00Z",
    ended_at: "2026-09-20T12:00:00Z",
    gate_reason: null,
    hunter: "swarm",
    model: "claude-opus-5-5",
    subagent_model: "zai/glm-5.3",
    schedule_id: "rts_1",
    ...overrides,
  };
}

const hunterSessions = [session({ id: "h1", cost_usd: 4 }), session({ id: "h2", cost_usd: 6 })];

describe("red-team plan helpers", () => {
  it("a run costs its hunters' spend; an estimate scales the average hunter to the new swarm", () => {
    expect(runCost(run(), hunterSessions)).toBe(10);
    expect(runCost(run(), [])).toBeNull();
    // 10 over 2 hunters = 5 a hunter; 4 hunters × 3 repositories = 60.
    expect(estimateCost([run()], hunterSessions, 4, 3)).toEqual({ low: 60, basis: "runs" });
    expect(estimateCost([], [session({ cost_usd: 3 })], 2, 1)).toEqual({ low: 6, basis: "colonies" });
    expect(estimateCost([], [], 2, 1)).toBeNull();
  });

  it("a local weekly or monthly choice becomes the UTC cadence of that same local moment", () => {
    const now = new Date(2026, 8, 24, 12, 0);
    const weekly = toUtcCadence({ every: "weekly", weekday: 0, time: "09:30" }, now);
    const localMonday = new Date(2026, 8, 28, 9, 30);
    expect(weekly).toEqual({ every: "weekly", weekday: (localMonday.getUTCDay() + 6) % 7, hour: localMonday.getUTCHours(), minute: localMonday.getUTCMinutes() });
    const monthly = toUtcCadence({ every: "monthly", day: 15, time: "06:00" }, now);
    const local15 = new Date(2026, 8, 15, 6, 0);
    expect(monthly).toEqual({ every: "monthly", day: local15.getUTCDate(), hour: local15.getUTCHours(), minute: local15.getUTCMinutes() });
    expect(toUtcCadence({ every: "once" }, now)).toBeNull();
    expect(describeCadence(weekly!, now)).toMatch(/^Every Monday at /);
    expect(describeCadence({ every: "monthly", day: 31, hour: 6, minute: 0 }, now)).toContain("the month's last");
  });
});

describe("the red-team wizard", () => {
  const wizard = (step: 0 | 1 | 2, runs: RedTeamRun[] = []) =>
    renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <WizardBody org="acme" sessions={hunterSessions} runs={runs} onClose={noop} onDone={noop} onOpenHistory={noop} initialStep={step} />
      </ApiContext.Provider>,
    );

  it("step 1 offers the swarm and shows Strix and Shannon as coming soon, not startable", () => {
    const html = wizard(0);
    expect(html).toContain("Colony swarm");
    expect(html).toMatch(/Strix[\s\S]*Coming soon/);
    expect(html).toMatch(/Shannon[\s\S]*Coming soon/);
    expect(html.match(/disabled=""/g)?.length).toBeGreaterThanOrEqual(2);
  });

  it("step 2 picks the hunter and subagent models", () => {
    const html = wizard(1);
    expect(html).toContain("Hunter model");
    expect(html).toContain("Subagent model");
    expect(html).toContain("Hunters per repository");
  });

  it("step 3 warns that it is expensive, estimates from past runs, keeps autofix off and offers once / weekly / monthly", () => {
    const html = wizard(2, [run()]);
    expect(html).toContain("Red-team runs are expensive");
    expect(html).toContain("from your past red-team runs");
    expect(html).toMatch(/<input type="checkbox" class="mt-0.5"\/>/);
    expect(html).toContain("Once, now");
    expect(html).toContain("Weekly");
    expect(html).toContain("Monthly");
  });
});

describe("the red-team history", () => {
  const schedule: RedTeamSchedule = {
    id: "rts_1",
    org: "acme",
    repos: ["acme/webshop", "acme/api"],
    hunter: "swarm",
    swarm_size: 3,
    model: null,
    subagent_model: null,
    autofix: false,
    cadence: { every: "weekly", weekday: 0, hour: 2, minute: 0 },
    enabled: true,
    next_run_at: "2026-09-28T02:00:00Z",
    last_run_at: null,
    last_result: null,
    created_at: "2026-09-24T00:00:00Z",
  };

  it("lists the org's schedules and runs with findings, models and cost; other orgs stay out", () => {
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <HistoryBody
          org="acme"
          sessions={hunterSessions}
          runs={[run(), run({ id: "rt_2", org: "other", repo: "other/x" }), run({ id: "rt_3", state: "running", hunters: [] })]}
          onClose={noop}
          onStop={async () => {}}
          onOpenColony={noop}
          onNew={noop}
          initialSchedules={[schedule]}
        />
      </ApiContext.Provider>,
    );
    expect(html).toContain("2 runs · 1 live");
    expect(html).toContain("Every Monday at");
    expect(html).toContain("webshop, api · 3 hunters");
    expect(html).toContain("5 found · 3 validated · 2 filed · 1 rejected");
    expect(html).toContain("claude-opus-5-5 / zai/glm-5.3");
    expect(html).toContain("$10.00");
    expect(html).toContain("Stop run");
    expect(html).not.toContain("other/x");
  });
});

describe("the overview's workspace rows", () => {
  const acme: OrgEntry = { org: "acme", live: 0, queued: 0, total: 1, pending: 0, avatar: null };
  const overview = () =>
    renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <OverviewView sessions={[session()]} orgs={[acme]} cost={null} runs={[run({ state: "running" })]} onOpenColony={noop} />
      </ApiContext.Provider>,
    );

  it("each row carries Red team (with its live-run count) and history; the old card is gone", () => {
    const html = overview();
    expect(html).toContain('aria-label="start a red team on acme"');
    expect(html).toContain('aria-label="red-team history for acme"');
    expect(html).not.toContain("Adversarial raids");
    expect(html).not.toContain("arm one above");
  });

  it("Compare with an empty previous period says so instead of showing nothing", () => {
    const html = overview();
    expect(html).toContain('role="switch" aria-checked="true"');
    expect(html).toContain("No activity in the previous 30d yet");
    expect(html).toContain("prev 30d · no activity");
    expect(html).toContain(">new<");
  });

  it("compareDelta reads new over an empty previous period and a percentage otherwise", () => {
    expect(compareDelta(3, 0)).toEqual({ text: "new", d: 1 });
    expect(compareDelta(3, null)).toEqual({ text: "new", d: 1 });
    expect(compareDelta(0, 0)).toBeUndefined();
    expect(compareDelta(6, 3)?.text).toBe("+100%");
  });
});
