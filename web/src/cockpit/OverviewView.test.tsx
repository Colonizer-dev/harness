// The overview dashboard (issue #398): six KPI tiles with honest empty states, the needs-you
// queue oldest first, the merged-per-day chart, the workspaces-compared table, workspace cards
// and the colonies table with its status + org filter chips. Rendering through
// react-dom/server, because this codebase keeps tests off jsdom; renderToStaticMarkup runs no
// effects, so the spend-history fetch never fires and history-backed figures read as unmeasured —
// exactly the older-mothership path. Static markup cannot click, so the filtered states render
// through the `initialFilter` prop.
//
// The overview page's relationship between the header total and the org cards (issue #209): when a
// mothership reports per-org spend, the header is the sum of the rows (never a session-derived
// figure that could disagree), and an org whose spend was never measured reads "—", never "$0.00".
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import type { OrgEntry } from "../orgs";
import type { Session } from "../types";
import { OverviewView } from "./OverviewView";
import { changeFailRate, dailyMerged, formatWait, mergedInWindow, orgColorFor, shortDayLabel } from "./dash";
import type { OverviewFilter } from "./feed";

const api = createMockApi();

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "s1",
    repo: "acme/webshop",
    org: "acme",
    issue: 42,
    issue_title: "Checkout fails for guest users",
    status: "running",
    branch: "colonizer/issue-42-s1",
    base: "main",
    parent: null,
    worktree: "/wt/s1",
    git_admin_dir: "/git/s1",
    sandbox: "colony-s1",
    mesh: null,
    agent: "claude-code",
    autopilot: false,
    pr_url: null,
    error: null,
    cost_usd: null,
    cleaned_up: false, keep_worktree: false,
    created_at: "2026-09-18T09:00:00Z",
    updated_at: "2026-09-18T09:10:00Z",
    attention: null,
    ...overrides,
  };
}

const noop = () => {};

const ACME: OrgEntry = { org: "acme", live: 1, queued: 0, total: 1, pending: 0, avatar: null };

const entry = (org: string, spend: OrgEntry["spend"]): OrgEntry => ({
  org,
  live: 0,
  queued: 0,
  total: 0,
  pending: 0,
  avatar: null,
  spend,
});

const renderOverview = (list: Session[], orgs: OrgEntry[], filter: OverviewFilter | null = null) =>
  renderToStaticMarkup(
    <ApiContext.Provider value={api}>
      <OverviewView
        sessions={list}
        orgs={orgs}
        cost={null}
        initialFilter={filter}
        onOpenColony={noop}
      />
    </ApiContext.Provider>,
  );

const measured = (costUsd: number, routedCostUsd: number): OrgEntry["spend"] => ({
  cost_usd: costUsd,
  routed_cost_usd: routedCostUsd,
  tokens: { input: 1, output: 1, cache_read: 0, cache_write: 0 },
  models: [],
});

const unmeasured: OrgEntry["spend"] = {
  cost_usd: null,
  routed_cost_usd: null,
  tokens: { input: 1, output: 1, cache_read: 0, cache_write: 0 },
  models: [],
};

describe("overview dash helpers", () => {
  const from = Date.parse("2026-09-01T00:00:00Z");
  const to = Date.parse("2026-10-01T00:00:00Z");
  const merged = (id: string, created_at: string, status: Session["status"] = "merged") =>
    session({ id, created_at, updated_at: created_at, status });

  it("mergedInWindow falls back to created_at when merged_at is absent: only merged sessions in the window count", () => {
    const list = [
      merged("in", "2026-09-10T09:00:00Z"),
      merged("too-old", "2026-08-10T09:00:00Z"),
      merged("pr-open", "2026-09-10T09:00:00Z", "pr_opened"),
      merged("failed", "2026-09-10T09:00:00Z", "failed"),
    ];
    expect(mergedInWindow(list, from, to).map((s) => s.id)).toEqual(["in"]);
  });

  it("dailyMerged counts merged sessions per day and per org", () => {
    const list = [
      merged("a1", "2026-09-10T09:00:00Z"),
      merged("a2", "2026-09-10T10:00:00Z"),
      merged("b1", "2026-09-11T09:00:00Z", "merged"),
    ];
    const other = session({ id: "o1", org: "beta", repo: "beta/api", created_at: "2026-09-10T09:00:00Z", updated_at: "2026-09-10T09:00:00Z", status: "merged" });
    expect(dailyMerged([...list, other], ["2026-09-10", "2026-09-11"])).toEqual([3, 1]);
    expect(dailyMerged([...list, other], ["2026-09-10", "2026-09-11"], "acme")).toEqual([2, 1]);
  });

  it("changeFailRate reads failed ÷ decided among sessions created in the window", () => {
    const list = [
      merged("m1", "2026-09-10T09:00:00Z"),
      merged("m2", "2026-09-10T09:00:00Z"),
      merged("f1", "2026-09-10T09:00:00Z", "failed"),
      merged("run", "2026-09-10T09:00:00Z", "running"),
      merged("old", "2026-08-10T09:00:00Z", "failed"),
    ];
    const rate = changeFailRate(list, from, to);
    expect(rate.failed).toBe(1);
    expect(rate.decided).toBe(3);
    expect(rate.rate).toBeCloseTo(1 / 3);
    expect(changeFailRate([merged("run", "2026-09-10T09:00:00Z", "running")], from, to).rate).toBeNull();
  });

  it("formatWait compacts a wait the way the queue shows it", () => {
    expect(formatWait(45_000)).toBe("45s");
    expect(formatWait(22 * 60_000)).toBe("22m");
    expect(formatWait(7 * 3_600_000)).toBe("7h");
    expect(formatWait(3 * 86_400_000)).toBe("3d");
    expect(formatWait(NaN)).toBe("—");
  });

  it("shortDayLabel shortens a history day for the x axis", () => {
    expect(shortDayLabel("2026-09-03")).toBe("Sep 3");
  });
});

describe("OverviewView KPIs", () => {
  const recent = new Date(Date.now() - 2 * 86_400_000).toISOString();
  const list = [
    session({ id: "m1", status: "merged", created_at: recent, updated_at: recent }),
    session({ id: "m2", status: "merged", created_at: recent, updated_at: recent }),
    session({ id: "f1", status: "failed", created_at: recent, updated_at: recent }),
    session({ id: "r1", status: "running" }),
  ];

  it("renders the measured tiles and names the unmeasured ones once", () => {
    const html = renderOverview(list, [ACME]);
    for (const label of ["Merged PRs", "Change failure rate", "Spend", "Live colonies"]) {
      expect(html).toContain(label);
    }
    // Lead time, PR cycle time and CI pass rate are measured tiles now, not a footnote.
    for (const label of ["Lead time", "PR cycle time", "CI pass rate"]) expect(html).toContain(label);
    expect(html).not.toContain("not measured yet");
  });

  it("counts merged PRs from sessions and says the bucket out loud", () => {
    const html = renderOverview(list, [ACME]);
    expect(html).toContain("by merge date");
    expect(html).toContain("Merged PRs per day");
  });

  it("labels the change-failure basis in the sub-line", () => {
    const html = renderOverview(list, [ACME]);
    expect(html).toContain("1 failed of 3 decided (merged+failed)");
  });

  it("reads empty when nothing was decided in range", () => {
    const html = renderOverview([session({ id: "r1", status: "running" })], [ACME]);
    expect(html).toContain("nothing decided in range");
  });
});

describe("OverviewView needs-you queue", () => {
  const waiting = (id: string, issue: number, since: string): Session =>
    session({
      id,
      issue,
      issue_title: `Question ${issue}`,
      status: "waiting_for_answer",
      attention: { reason: "waiting_for_answer", since, nudges: 0 },
      updated_at: since,
    });

  it("lists waiting colonies oldest first with an Answer affordance each", () => {
    const html = renderOverview(
      [waiting("new", 2, "2026-09-18T09:00:00Z"), waiting("old", 1, "2026-09-10T09:00:00Z")],
      [ACME],
    );
    expect(html).toContain(">Needs you</h2>");
    expect(html).toContain("2 waiting · oldest first");
    expect(html.indexOf("webshop#1")).toBeLessThan(html.indexOf("webshop#2"));
    expect(html.match(/>Answer</g)?.length).toBe(2);
  });

  it("stays hidden when nobody waits", () => {
    expect(renderOverview([session()], [ACME])).not.toContain(">Needs you</h2>");
  });
});

describe("OverviewView workspaces", () => {
  const withModels = (org: string): OrgEntry => ({
    ...entry(org, {
      cost_usd: 12.5,
      routed_cost_usd: 0.5,
      tokens: { input: 1, output: 1, cache_read: 0, cache_write: 0 },
      models: [{ model: "deepseek/deepseek-flash", tokens: 1_204_000, cost_usd: 2.91 }],
    }),
    live: 1,
    total: 2,
  });

  it("compares workspaces with merged, fail % and spend, and names the failure basis", () => {
    const recent = new Date(Date.now() - 2 * 86_400_000).toISOString();
    const html = renderOverview(
      [
        session({ id: "m1", status: "merged", created_at: recent, updated_at: recent }),
        session({ id: "f1", status: "failed", created_at: recent, updated_at: recent }),
      ],
      [withModels("acme")],
    );
    expect(html).toContain("Share by workspace");
    expect(html).toContain(">Fail</span>");
    expect(html).toContain("1 failed of 2 decided (merged+failed)");
    expect(html).toContain("$13.00");
  });

  it("tables each workspace with colonies, need, merged, fail, spend and a dashboard way in", () => {
    const html = renderOverview([session({ id: "r1" }), session({ id: "w1", status: "waiting_for_answer" })], [withModels("acme")]);
    expect(html).toContain(">Workspaces</h2>");
    for (const col of [">Colonies<", ">Need<", ">Merged<", ">Fail<", ">Spend<", ">Trend<"]) expect(html).toContain(col);
    expect(html).toContain("open the acme dashboard");
    expect(html).toContain("1 need you");
  });
});

describe("OverviewView colonies table", () => {
  it("shows colony titles with status, org, updated and spent", () => {
    const html = renderOverview(
      [session({ id: "s1", cost_usd: 0.87 }), session({ id: "s2", repo: "acme/design-system", issue: 7, issue_title: "Bad contrast on the nav" })],
      [ACME],
    );
    expect(html).toMatch(/>Colonies<\/h2><span[^>]*>2</);
    expect(html).toContain("Checkout fails for guest users");
    expect(html).toContain("Bad contrast on the nav");
    expect(html).toContain("Working");
    expect(html).toContain("$0.87");
    expect(html).toContain("—");
  });

  it("sorts needs-you longest-wait first, ahead of working colonies", () => {
    const html = renderOverview(
      [
        session({ id: "work", status: "running", updated_at: "2026-09-18T09:10:00Z" }),
        session({ id: "need", status: "waiting_for_answer", attention: { reason: "waiting_for_answer", since: "2026-09-10T09:00:00Z", nudges: 0 }, updated_at: "2026-09-10T09:00:00Z" }),
      ],
      [ACME],
    );
    expect(html.indexOf("Needs your answer")).toBeLessThan(html.indexOf("Working"));
  });

  it("header filters narrow the table and name the filter", () => {
    const beta: OrgEntry = { org: "beta", live: 1, queued: 0, total: 1, pending: 0, avatar: null };
    const html = renderOverview(
      [session({ id: "a1" }), session({ id: "b1", org: "beta", repo: "beta/api" })],
      [ACME, beta],
    );
    // The column titles are the filter controls: a search box and menus, no pill row above.
    expect(html).toContain('placeholder="Colony"');
    for (const label of [">Org<", ">Status<", ">Updated<", ">Spent<"]) expect(html).toContain(label);
    expect(html).not.toContain("All orgs");
    expect(html).toContain("beta");
    // A pinned bucket narrows the table and names the filter.
    const filtered = renderOverview([session({ id: "a1" }), session({ id: "b1", org: "beta", repo: "beta/api", status: "queued" })], [ACME, beta], "live");
    expect(filtered).toMatch(/>Colonies<\/h2><span[^>]*>1</);
    expect(filtered).toContain("showing 1 of 2");
  });

  it("caps a long table at ten with a way to see the rest", () => {
    const many = Array.from({ length: 11 }, (_, i) => session({ id: `s${i}`, issue: i, issue_title: `Work ${i}` }));
    const html = renderOverview(many, [ACME]);
    expect(html).toContain("Show all 11 colonies");
    const few = renderOverview(many.slice(0, 3), [ACME]);
    expect(few).not.toContain("Show all");
  });
});

describe("OverviewView spend", () => {
  const render = (orgs: OrgEntry[], cost: number | null) =>
    renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <OverviewView sessions={[]} orgs={orgs} cost={cost} onOpenColony={() => {}} />
      </ApiContext.Provider>,
    );

  it("derives the header from the org rows, matching their sum and ignoring the session-based cost prop", () => {
    const html = render([entry("acme", measured(12.5, 0.5)), entry("globex", measured(2, 0))], 999);
    // 12.5 + 0.5 + 2 = 15. The `cost` prop of 999 must not win.
    expect(html).toContain("$15.00 spent");
    expect(html).not.toContain("$999.00");
  });

  it("shows — on each org, and no $0.00, when every org is a never-measured subscription", () => {
    const html = render([entry("acme", unmeasured)], null);
    expect(html).toContain("—");
    expect(html).not.toContain("$0.00");
  });

  it("falls back to the sessions-derived cost when no org carries server spend", () => {
    // total: 1 so the card itself renders under the default hide-empty toggle; the pin is the header.
    const html = render([{ ...entry("acme", undefined), total: 1 }], 4.5);
    expect(html).toContain("$4.50 spent");
    expect(html).toContain("acme");
  });

  it("falls back to the sessions-derived cost unless every org carries server spend", () => {
    const html = render([entry("acme", measured(12.5, 0.5)), entry("globex", undefined)], 7.25);
    // 12.5 + 0.5 = 13 from the row with spend, but a sibling without spend means the server is not
    // reporting a complete rollup, so the sessions-derived `cost` of 7.25 must win.
    expect(html).toContain("$7.25 spent");
    expect(html).not.toContain("$13.00 spent");
  });
});

// Counters vs list (issue #246): the chips must never count colonies the cards cannot show. A
// switched-off org has no card, so its colonies are out of the counters and named in the scope
// line instead; a bucket filter that hides everything explains where the rest went and offers a
// one-click way back. renderToStaticMarkup cannot click, so the filtered states render through
// the `initialFilter` prop.
describe("OverviewView counters vs list", () => {
  // 14 live in the visible workspaces (9 in acme, 5 in beta, plus 2 queued in acme) and 11
  // waiting in gamma, which is switched off and therefore has no card.
  const live = (id: string, org: string, repo: string): Session =>
    session({ id, org, repo, status: "running", issue: 1, issue_title: `Work ${id}` });
  const queued = (id: string): Session => session({ id, status: "queued", issue: 2, issue_title: `Queued ${id}` });
  const waiting = (id: string): Session =>
    session({ id, org: "gamma", repo: "gamma/secret", status: "waiting_for_answer", issue: 3, issue_title: `Help ${id}` });

  const sessions = (): Session[] => [
    ...Array.from({ length: 9 }, (_, i) => live(`a${i}`, "acme", "acme/webshop")),
    ...Array.from({ length: 5 }, (_, i) => live(`b${i}`, "beta", "beta/api")),
    queued("q0"),
    queued("q1"),
    ...Array.from({ length: 11 }, (_, i) => waiting(`g${i}`)),
  ];
  const workspaces: OrgEntry[] = [
    { org: "acme", live: 9, queued: 2, total: 11, pending: 0, avatar: null },
    { org: "beta", live: 5, queued: 0, total: 5, pending: 0, avatar: null },
  ];

  it("scopes the counter chips to the visible workspaces and names the hidden org", () => {
    const html = renderOverview(sessions(), workspaces);
    // 14 live and 2 queued in acme/beta; gamma's 11 waiting must not reach any chip.
    expect(html).toContain("0 colonies need you · 14 live · 2 queued across 2 workspaces");
    // ... but they are explained, not silently dropped: the scope line names gamma.
    expect(html).toContain("hidden org (gamma)");
    expect(html).toContain("11 need you");
  });

  it("labels an active bucket filter with a one-click clear", () => {
    const html = renderOverview(sessions(), workspaces, "live");
    expect(html).toContain("showing 14 of 16");
    expect(html).toContain("clear ×");
    expect(html).toContain("clear filters");
  });

  it("an empty filtered page explains where the hidden colonies are and links back", () => {
    // "returned" matches nothing anywhere, so the page is empty: the 16 visible colonies sit in
    // other buckets and gamma's 11 wait in a hidden org. Neither may read as bare numbers.
    const html = renderOverview(sessions(), workspaces, "returned");
    expect(html).toContain("nothing matches these filters");
    expect(html).toContain("16 in other buckets");
    expect(html).toContain("hidden org (gamma)");
    expect(html).toContain("clear filters ×");
  });
});

// Held slots and the stalled queue (issue #217): idle colonies whose PR autopilot holds occupy
// parallel slots without doing work. When every live colony is held and something queues, the
// queued chip must read as stalled — warn-bordered with the word itself — never as a healthy
// busy queue; and the header names how many slots are held.
describe("OverviewView held slots and stalled queue", () => {
  const idleHeld = (id: string): Session =>
    session({ id, status: "idle", attention: { reason: "autopilot_held", since: "2026-09-18T09:00:00Z", nudges: 0 } });

  it("marks the queued counter stalled when every live colony is held", () => {
    const html = renderOverview([idleHeld("h1"), idleHeld("h2"), session({ id: "q1", status: "queued" })], [ACME]);
    expect(html).toContain("Status · queue stalled");
    expect(html).toContain("2 held");
  });

  it("renders the queued counter normally while a colony is still working", () => {
    const html = renderOverview([idleHeld("h1"), session({ id: "r1", status: "running" }), session({ id: "q1", status: "queued" })], [ACME]);
    expect(html).not.toContain("stalled");
    expect(html).toContain("1 held");
  });
});

// Org avatars (issue #445): every tile that names an org shows its image when /api/orgs knows
// one — workspace cards, the needs-you rows, the compared table, the legend and the filter
// chips — and falls back to the coloured lettermark when there is none.
describe("OverviewView org avatars", () => {
  const AVATAR = "https://example.com/avatars/acme.png";
  const withAvatar: OrgEntry = { ...ACME, avatar: AVATAR };
  const waiting = (): Session =>
    session({
      id: "w1",
      status: "waiting_for_answer",
      attention: { reason: "waiting_for_answer", since: "2026-09-18T09:00:00Z", nudges: 0 },
      updated_at: "2026-09-18T09:00:00Z",
    });

  it("renders the avatar image in the legend and the workspaces table", () => {
    const html = renderOverview([waiting()], [withAvatar]);
    // Legend icon + workspaces row (the org chips only show with more than one workspace).
    expect(html.match(new RegExp(`src="${AVATAR}"`, "g"))?.length).toBeGreaterThanOrEqual(2);
  });

  it("falls back to the coloured lettermark when no avatar is known", () => {
    const html = renderOverview([waiting()], [ACME]);
    expect(html).not.toContain("<img");
    expect(html).toContain(orgColorFor("acme"));
    expect(html).toContain(">A<");
  });

  it("paints org series with the ramp, not org hues", () => {
    // With an avatar known, no lettermark fallback renders — any oklch hue left would be a chart series.
    const html = renderOverview([session()], [{ ...ACME, avatar: "https://example.com/avatars/acme.png" }]);
    expect(html).toContain("var(--chart-1)");
    expect(html).not.toContain("oklch(0.72");
  });

  it("stands main charts 200px tall, as the v3 render does", () => {
    const recent = new Date(Date.now() - 2 * 86_400_000).toISOString();
    const html = renderOverview([session({ id: "m1", status: "merged", created_at: recent, updated_at: recent })], [ACME]);
    expect(html).toContain("h-[200px]");
  });
});
