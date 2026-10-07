// The unified loop card (issue #1199): every loop is drawn by the one component, built-in and the
// user's own are told apart, and the 7-day strip draws. Static markup, like the rest of the cockpit's tests.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Api } from "../api";
import { ApiContext } from "../context";
import { buildHistory } from "../features/loops/mockHistory";
import type { Loop, LoopHistoryRun } from "../types";
import { DiskCleanupCard } from "./DiskCleanupLoop";
import { DocsLoopPanel } from "./DocsLoopCard";
import { LoopStrip, StackedBars } from "./LoopCard";
import { CustomLoopCard, LoopsView } from "./LoopsView";
import { MergeLoopPanel } from "./MergeLoopCard";
import { defaultMergeLoopSettings } from "./mergeLoop";
import { CLEANUP_LOOP } from "./testFixtures";

const api = {} as Api;
const wrap = (node: React.ReactElement) => renderToStaticMarkup(<ApiContext.Provider value={api}>{node}</ApiContext.Provider>);

const custom: Loop = {
  id: "loop_a",
  name: "Triage new issues",
  org: "acme",
  repo: "acme/web",
  prompt: "Triage the issues.",
  cadence: { every: "daily", hour: 9, minute: 0 },
  tz_offset_minutes: 0,
  model: null,
  subagent_model: null,
  autopilot: true,
  max_runs: null,
  end_at: null,
  enabled: true,
  next_run_at: null,
  runs: 3,
  last_run: null,
  last_note: null,
  ended_reason: null,
  created_at: "2026-09-20T00:00:00Z",
};

const cards = () => ({
  merge: wrap(
    <MergeLoopPanel
      view={{ settings: defaultMergeLoopSettings(), next_run_at: null, running: false, writes_blocked: false, repos: {}, last_report: null, history: [] }}
      draft={defaultMergeLoopSettings()}
      repoNames={[]}
      dirty={false}
      busy={false}
      onChange={() => {}}
      onSave={() => {}}
      onRun={() => {}}
    />,
  ),
  docs: wrap(
    <DocsLoopPanel
      view={{ name: "Docs & README", settings: { allow: [], interval_hours: 24, cooldown_hours: 24 }, enabled: false, next_run_at: null, last_report: null, history: [], limits: { min_interval_hours: 1, max_interval_hours: 168, max_cooldown_hours: 720 } }}
      dryRun={null}
      busy={false}
      onTarget={() => {}}
      onSave={() => {}}
      onRun={() => {}}
      onOpenColony={() => {}}
    />,
  ),
  disk: wrap(<DiskCleanupCard loop={CLEANUP_LOOP} onChanged={() => {}} />),
  mine: wrap(<CustomLoopCard loop={custom} onToggle={() => {}} onRun={() => {}} onEdit={() => {}} onDelete={() => {}} onOpenColony={() => {}} />),
});

describe("the loop card", () => {
  it("gives every loop the same shape: a name, a purpose, when it runs, what it covers, its last run and a 7-day strip", () => {
    for (const [name, html] of Object.entries(cards())) {
      expect(html, name).toContain("<article");
      expect(html, name).toMatch(/<h3[^>]*>/);
      for (const label of [">Runs<", ">Covers<", ">Last run<", "Last 7 days"]) expect(html, `${name} ${label}`).toContain(label);
    }
  });

  it("tells a built-in loop from one of your own, and says plainly when nothing is set up", () => {
    const { merge, docs, disk, mine } = cards();
    for (const html of [merge, docs, disk]) {
      expect(html).toContain("Built-in");
      expect(html).not.toContain(">Yours<");
    }
    expect(mine).toContain(">Yours<");
    expect(mine).not.toContain("Built-in");
    expect(merge).toContain("Not set up: add a repository");
    expect(docs).toContain("Not set up: add a repository or an org");
    expect(mine).toContain("Every day at");
    expect(mine).toContain("acme/web");
  });

  it("separates the built-in loops from your own on the page", () => {
    const html = wrap(<LoopsView org={null} orgs={[]} repos={[]} sessions={[]} onOpenColony={() => {}} />);
    const builtIn = html.indexOf('aria-label="Built-in loops"');
    const yours = html.indexOf('aria-label="Your loops"');
    expect(builtIn).toBeGreaterThan(-1);
    expect(yours).toBeGreaterThan(builtIn);
    expect(html).toContain("New loop");
  });
});

describe("the 7-day strip's markup", () => {
  const now = Date.parse("2026-10-07T12:00:00Z");
  const runs: LoopHistoryRun[] = [
    { at: "2026-10-07T09:00:00Z", trigger: "schedule", outcome: "ok", summary: "ok", counts: {}, colonies: ["c"], cost_usd: 2 },
    { at: "2026-10-06T09:00:00Z", trigger: "schedule", outcome: "failed", summary: "bad", counts: {}, colonies: [], cost_usd: 0 },
  ];

  it("renders seven bars coloured by outcome, each with its day in the tooltip", () => {
    const html = renderToStaticMarkup(<LoopStrip history={buildHistory("x", runs, 7, 0, now)} />);
    expect(html).toContain('data-strip="day"');
    expect(html).toContain('aria-label="Last 7 days: 2 runs · 1 failed · $2.00"');
    expect(html).toContain('title="Wed 7 Oct: 1 run (1 ok), $2.00"');
    expect(html).toContain("background:var(--ok)");
    expect(html).toContain("background:var(--err)");
    expect(html.match(/title="/g)).toHaveLength(7);
  });

  it("draws seven bars for an hourly loop too, so every card's strip reads the same", () => {
    const hourly: LoopHistoryRun[] = Array.from({ length: 48 }, (_, i) => ({ at: `2026-10-0${6 + Math.floor(i / 24)}T${String(i % 24).padStart(2, "0")}:00:00Z`, trigger: "schedule", outcome: i % 10 === 0 ? "failed" : "ok", summary: "x", counts: {}, colonies: [], cost_usd: 0 }));
    const html = renderToStaticMarkup(<LoopStrip history={buildHistory("x", hourly, 7, 0, now)} />);
    expect(html).toContain('data-strip="day"');
    expect(html.match(/title="/g)).toHaveLength(7);
  });

  it("draws the detail's runs per day as stacked bars, one column a day, split by outcome", () => {
    const html = renderToStaticMarkup(<StackedBars history={buildHistory("x", runs, 7, 0, now)} range={7} />);
    expect(html).toContain("data-stacked-bars");
    expect(html.match(/aria-label="[A-Z][a-z]{2} \d+ Oct:/g)).toHaveLength(7);
    expect(html).toContain("Wed 7 Oct: 1 run (1 ok), $2.00");
    expect(html).toContain("background:var(--ok)");
    expect(html).toContain("background:var(--err)");
    expect(html).not.toContain("<path");
    expect(renderToStaticMarkup(<StackedBars history={buildHistory("x", [], 7, 0, now)} range={7} />)).toContain("no runs in this range");
  });

  it("shows a placeholder while the history loads", () => {
    expect(renderToStaticMarkup(<LoopStrip history={null} />)).toContain("animate-pulse");
  });
});
