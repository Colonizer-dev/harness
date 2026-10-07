// The red-team wizard, history and workspace-row buttons that replaced the Overview's red-team card,
// plus the Compare switch's empty-previous-period reading. Static markup (no jsdom): steps and
// schedules are pinned through props, since nothing here can click.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import type { OrgEntry } from "../orgs";
import type { RedTeamRun, RedTeamSchedule, Session } from "../types";
import { HistoryBody, SecurityReport } from "./RedTeamHistory";
import { RepoList, WizardBody } from "./RedTeamWizard";
import { CancelRunButton, cancelPrompt } from "./RedTeamCancel";
import { OverviewView } from "./OverviewView";
import { compareDelta } from "./dash";
import { activeLine, activeRunFor, describeCadence, estimateCost, historyLine, plural, presetOf, runCost, sortForRedTeam, toUtcCadence } from "./redTeamPlan";
import type { PreScan } from "../types";

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
    counts: { found: 5, validated: 3, rejected: 1, filed: 2, merged: null },
    synthesis: null,
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

  it("step 1 offers the swarm and Shannon as startable, and Strix alone as coming soon", () => {
    const html = wizard(0);
    expect(html).toContain("Colony swarm");
    expect(html).toMatch(/Shannon[\s\S]*Ready[\s\S]*runs in a colony/);
    // No pill wraps and no name truncates: the pill sits on its own line under the name.
    expect(html).not.toContain("Ready · runs in a colony");
    expect(html).toMatch(/whitespace-nowrap[^"]*"[^>]*>Coming soon/);
    expect(html).not.toMatch(/<span class="truncate">(Colony swarm|Strix|Shannon)/);
    expect(html).toContain("grid-cols-1");
    expect(html).toMatch(/Strix[\s\S]*Coming soon/);
    // Swarm is picked by default and Shannon is selectable (an enabled, unpressed button); Strix alone is disabled.
    expect(html).toMatch(/<button type="button" aria-pressed="true" class="[^"]*">[\s\S]*?Colony swarm/);
    expect(html).toMatch(/<button type="button" aria-pressed="false" class="[^"]*">[\s\S]*?Shannon/);
    expect(html).toMatch(/<button type="button" aria-pressed="false" disabled=""[\s\S]*?Strix/);
  });

  it("picking Shannon selects its card, drops the swarm size and counts one colony per repository", () => {
    // Static markup cannot click, so the picks are pinned through the same props as the step and preset.
    const pinned = (step: 0 | 1 | 2) =>
      renderToStaticMarkup(
        <ApiContext.Provider value={api}>
          <WizardBody org="acme" sessions={hunterSessions} runs={[]} onClose={noop} onDone={noop} onOpenHistory={noop} initialStep={step} initialHunter="shannon" />
        </ApiContext.Provider>,
      );
    expect(pinned(0)).toMatch(/<button type="button" aria-pressed="true" class="[^"]*">[\s\S]*?Shannon/);
    const models = pinned(1);
    expect(models).not.toContain("Hunters per repository");
    expect(models).toContain("there is no swarm size to set");
    expect(pinned(2)).toMatch(/\(1 per repository × \d+\)/);
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

describe("the security preset", () => {
  const wizardWith = (step: 0 | 2, preset?: "general" | "security") =>
    renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <WizardBody org="acme" sessions={hunterSessions} runs={[]} onClose={noop} onDone={noop} onOpenHistory={noop} initialStep={step} initialPreset={preset} />
      </ApiContext.Provider>,
    );

  it("the wizard's first step offers a preset picker with general picked by default", () => {
    const html = wizardWith(0);
    expect(html).toContain('aria-label="Preset"');
    expect(html).toMatch(/role="radio" aria-checked="true"[^>]*>[\s\S]*?General/);
    expect(html).toMatch(/role="radio" aria-checked="false"[^>]*>[\s\S]*?Security/);
    expect(html).toContain("pre-scan before launch");
  });

  it("picking security checks it, and the review step says what the pre-scan does", () => {
    expect(wizardWith(0, "security")).toMatch(/role="radio" aria-checked="true"[^>]*>[\s\S]*?Security/);
    const review = wizardWith(2, "security");
    expect(review).toContain("pre-scans the repository");
    expect(review).toContain("no model tokens");
    expect(review).toContain("· security");
    expect(wizardWith(2)).not.toContain("pre-scans the repository");
  });

  it("the mock API keeps the preset a start names, and a run with none reads as general", async () => {
    const mock = createMockApi();
    const started = await mock.startRedTeamRun({ repo: "acme/fresh-repo", arm: true, preset: "security" });
    expect(started.preset).toBe("security");
    expect(presetOf(run())).toBe("general");
  });

  const prescan: PreScan = {
    ran_at: "2026-09-30T10:00:00Z",
    commit: "0123456789abcdef",
    secret_scanner: "builtin",
    notes: ["gitleaks is not installed on the host; secrets were scanned with the built-in provider-prefix fallback, which knows fewer key shapes"],
    leads: [
      { id: "P1", check: "string_built_sql", focus: 3, path: "src/db.js", line: 12, commit: null, message: "SQL text built from strings in code — lead: check whether user input reaches it unparameterised" },
      { id: "P2", check: "env_file", focus: 2, path: ".env", line: null, commit: null, message: "`.env` is committed — lead: env files usually hold real values" },
    ],
    checklist: [
      { id: "key_rotation", title: "Keys rotated after any exposure", status: "needs_review", evidence: "1 possible exposure(s) in .env; rotation happens at each provider and is not verifiable from the repo" },
      { id: "backups_restore_tested", title: "Backups restore-tested", status: "not_verifiable", evidence: "no backup job found in repo; not verifiable from the repo" },
    ],
  };

  it("the report's pre-scan section lists leads as leads with their focus, and says which secret scanner ran", () => {
    const html = renderToStaticMarkup(<SecurityReport prescan={prescan} />);
    expect(html).toContain("Pre-scan leads · 2 · secrets by the built-in fallback");
    expect(html).toContain("not a confirmed vulnerability");
    expect(html).toContain("gitleaks is not installed");
    expect(html).toContain("src/db.js:12");
    expect(html).toContain("→ input handling and injection");
    expect(html).toContain("→ sessions, tokens and secrets");
  });

  it("the operator checklist shows evidence or not-verifiable, and nothing is ever passed or ticked", () => {
    const html = renderToStaticMarkup(<SecurityReport prescan={prescan} />);
    expect(html).toContain("Operator checklist");
    expect(html).toContain("Needs review");
    expect(html).toContain("Not verifiable from the repo");
    expect(html).toContain("no backup job found in repo");
    expect(html.toLowerCase()).not.toContain("passed");
    expect(html).not.toContain("checked=");
  });

  it("the history shows the sections and a security badge on a security run only", () => {
    const history = (runs: RedTeamRun[]) =>
      renderToStaticMarkup(
        <ApiContext.Provider value={api}>
          <HistoryBody org="acme" sessions={hunterSessions} runs={runs} onClose={noop} onOpenColony={noop} onNew={noop} initialSchedules={[]} />
        </ApiContext.Provider>,
      );
    const security = history([run({ preset: "security", prescan })]);
    expect(security).toContain(">security<");
    expect(security).toContain("Pre-scan leads");
    expect(security).toContain("Operator checklist");
    const general = history([run()]);
    expect(general).not.toContain("Pre-scan leads");
    expect(general).not.toContain("Operator checklist");
    expect(general).not.toContain(">security<");
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

  it("a run's row shows its merged count and synthesis state — re-run when done, retry when failed, neither while pending — over the count legend", () => {
    const row = (synthesis: RedTeamRun["synthesis"], merged: number | null = null) =>
      renderToStaticMarkup(
        <ApiContext.Provider value={api}>
          <HistoryBody
            org="acme"
            sessions={hunterSessions}
            runs={[run({ counts: { found: 5, validated: 3, rejected: 1, filed: 2, merged }, synthesis })]}
            onClose={noop}
            onSynthesize={async () => {}}
            onOpenColony={noop}
            onNew={noop}
            initialSchedules={[schedule]}
          />
        </ApiContext.Provider>,
      );
    const done = row({ state: "done", session_id: "synth1", report: "/r/report.json", reason: null, superseded: [] }, 3);
    expect(done).toContain("5 found · 3 merged · 3 validated · 2 filed");
    expect(done).toContain("Synthesis done");
    expect(done).toContain("synthesis colony");
    expect(done).toContain("Re-run synthesis");
    expect(done).toContain("found — raw findings summed across hunters; a defect two hunters report counts twice");
    expect(done).toContain("merged — distinct defects after the synthesis step deduplicates across hunters");
    expect(done).toContain("validated / rejected — the validator verdicts on hunter findings");
    expect(done).toContain("filed — findings filed as, or matched to, a GitHub issue");
    const failed = row({ state: "failed", session_id: "synth2", report: null, reason: "the synthesis colony ran out of budget", superseded: [] });
    expect(failed).toContain("Synthesis failed");
    expect(failed).toContain("the synthesis colony ran out of budget");
    expect(failed).toContain("Retry synthesis");
    expect(failed).not.toContain("Re-run synthesis");
    const pending = row({ state: "pending", session_id: "synth3", report: null, reason: null, superseded: [] });
    expect(pending).toContain("Synthesis queued");
    expect(pending).not.toContain("Retry synthesis");
    expect(pending).not.toContain("Re-run synthesis");
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

  it("Compare with an empty previous period says so in the tooltip and the ghost key, not on every figure", () => {
    const html = overview();
    expect(html).toContain('role="switch" aria-checked="true"');
    expect(html).toContain("No activity in the previous 30d yet");
    expect(html).toContain("prev 30d · no activity");
    expect(html).not.toContain(">new<");
    expect(html).not.toContain("prev 30d empty");
  });

  it("compareDelta gives no chip over an empty previous period and a percentage otherwise", () => {
    expect(compareDelta(3, 0)).toBeUndefined();
    expect(compareDelta(3, null)).toBeUndefined();
    expect(compareDelta(0, 0)).toBeUndefined();
    expect(compareDelta(6, 3)?.text).toBe("+100%");
  });
});

describe("the red-team repository list (#1145)", () => {
  const NOW = Date.parse("2026-10-07T12:00:00Z");
  const repo = (name: string, pushed: string | null = "2026-10-07T10:00:00Z") => ({
    full_name: `acme/${name}`,
    description: null,
    private: false,
    fork: false,
    archived: false,
    open_issues_count: 0,
    pushed_at: pushed,
  });
  const live = run({ id: "rt_live", repo: "acme/harness", state: "running", ended_at: null, counts: { found: 0, validated: 0, rejected: 0, filed: 0, merged: null } });
  const old = run({ id: "rt_old", repo: "acme/old", state: "done", ended_at: "2026-10-04T12:00:00Z", counts: { found: 4, validated: 3, rejected: 0, filed: 2, merged: null } });
  const newer = run({ id: "rt_new", repo: "acme/newer", state: "cancelled", ended_at: "2026-10-06T12:00:00Z", counts: { found: 1, validated: 1, rejected: 0, filed: 0, merged: null } });
  const repos = [repo("harness"), repo("newer"), repo("old"), repo("fresh")];
  const list = (runs: RedTeamRun[], picked: string[] = []) =>
    renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <RepoList org="acme" repos={repos} runs={runs} sessions={[session({ id: "h1", status: "merged" }), session({ id: "h2", status: "running" })]} picked={picked} onPick={noop} onCancel={async () => {}} onOpenRun={noop} now={NOW} />
      </ApiContext.Provider>,
    );

  it("a repository with an active run is a disabled row with its status, a link and a Cancel run button", () => {
    const html = list([live]);
    expect(html).toMatch(/<input[^>]*aria-label="harness"[^>]*disabled=""/);
    expect(html).toMatch(/run in progress · started \d\d:\d\d · 1\/2 hunters done/);
    expect(html).toContain("View run");
    expect(html).toContain("Cancel run");
    // Other rows stay enabled and have no Cancel.
    expect(html).toMatch(/<input[^>]*aria-label="fresh"(?![^>]*disabled)/);
  });

  it("All skips the active repository and says so", () => {
    const html = list([live]);
    expect(html).toContain("All 3");
    expect(html).toContain("3 of 4: harness already has a run");
    // A picked-but-active repo is never shown ticked.
    expect(list([live], ["acme/harness"])).not.toMatch(/aria-label="harness"[^>]*checked/);
  });

  it("each row reads its red-team history, never an all-time colony count, and pluralises", () => {
    const html = list([old, newer]);
    expect(html).toContain("never hunted");
    expect(html).toContain("last hunted 3 d ago · 4 findings (2 filed)");
    expect(html).toContain("last hunted 1 d ago · 1 finding (0 filed)");
    expect(html).toContain("pushed 2 h ago");
    expect(html).not.toMatch(/\d+ colonies/);
    expect(plural(1, "colony", "colonies")).toBe("1 colony");
    expect(plural(0, "hunter")).toBe("0 hunters");
  });

  it("sorts active runs last, then never hunted, then the oldest hunt first", () => {
    const runs = [live, old, newer];
    expect(sortForRedTeam(repos, runs).map((r) => r.full_name)).toEqual(["acme/fresh", "acme/old", "acme/newer", "acme/harness"]);
    expect(activeRunFor(runs, "acme/harness")?.id).toBe("rt_live");
    expect(activeRunFor(runs, "acme/old")).toBeNull();
    expect(historyLine([], "acme/x", null, NOW)).toBe("never hunted");
    expect(activeLine(run({ state: "armed" }), [])).toContain("waiting for the nest to empty");
  });

  it("the cancel confirm names the hunters and says the findings are kept", () => {
    expect(cancelPrompt(run({ repo: "acme/harness", swarm_size: 8, hunters: [] }))).toBe("Stop 8 hunters on harness? Findings so far are kept.");
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <CancelRunButton run={live} onCancel={async () => {}} />
      </ApiContext.Provider>,
    );
    expect(html).toContain("Cancel run");
    expect(html).toContain("Stop 2 hunters on harness? Findings so far are kept.");
  });

  it("the history shows Cancel run on an active run and who cancelled a cancelled one", () => {
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <HistoryBody
          org="acme"
          sessions={hunterSessions}
          runs={[live, run({ id: "rt_c", state: "cancelled", cancelled_by: "you", cancelled_at: "2026-10-06T12:00:00Z" })]}
          onClose={noop}
          onCancel={async () => {}}
          onOpenColony={noop}
          onNew={noop}
          initialSchedules={[]}
        />
      </ApiContext.Provider>,
    );
    expect(html).toContain("Cancel run");
    expect(html).toContain("cancelled by you");
    expect(html).toContain("findings so far kept");
  });

  it("the mock cancel lands the run cancelled, keeps its counts and is idempotent", async () => {
    const mock = createMockApi();
    const started = await mock.startRedTeamRun({ repo: "acme/untouched", arm: true });
    await expect(mock.startRedTeamRun({ repo: "acme/untouched", arm: true })).rejects.toThrow(started.id);
    const cancelled = await mock.cancelRedTeamRun(started.id);
    expect(cancelled.state).toBe("cancelled");
    expect(cancelled.cancelled_by).toBe("you");
    expect((await mock.cancelRedTeamRun(started.id)).state).toBe("cancelled");
    await expect(mock.startRedTeamRun({ repo: "acme/untouched", arm: true })).resolves.toBeTruthy();
  });
});
