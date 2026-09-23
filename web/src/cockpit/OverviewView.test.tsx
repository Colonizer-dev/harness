// The overview's colony rows are disclosures: collapsed they must not leak the issue title (that is
// the whole point of the issue), a click reveals the title and a working link to the issue itself,
// and the toggle is pure enough to pin in the plain node environment — a Set keyed by session id, so
// one row's state cannot touch its siblings'. Rendering through react-dom/server, because this
// codebase keeps tests off jsdom: the collapsed and expanded markup are both fully visible there.
//
// The overview page's relationship between the header total and the org cards (issue #209): when a
// mothership reports per-org spend, the header is the sum of the rows (never a session-derived
// figure that could disagree), and an org whose spend was never measured reads "—", never "$0.00".
// renderToStaticMarkup runs no effects, so the sparkline fetch never fires and the org cards simply
// have no history — which is exactly the older-mothership path.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import type { OrgEntry } from "../orgs";
import type { HarnessStatus, Session } from "../types";
import { ColonyRow, OverviewView, flipExpanded } from "./OverviewView";
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

const render = (orgs: OrgEntry[], cost: number | null) =>
  renderToStaticMarkup(
    <ApiContext.Provider value={api}>
      <OverviewView sessions={[]} orgs={orgs} cost={cost} onOpenOrg={() => {}} onOpenColony={() => {}} onSelect={noop} />
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

describe("flipExpanded", () => {
  it("expands a collapsed row on the first click", () => {
    expect(flipExpanded(new Set(), "s1")).toEqual(new Set(["s1"]));
  });

  it("collapses an expanded row on the second click", () => {
    expect(flipExpanded(new Set(["s1", "s2"]), "s1")).toEqual(new Set(["s2"]));
  });

  it("leaves the other rows' state alone, so rows expand independently", () => {
    expect(flipExpanded(new Set(["s2"]), "s1")).toEqual(new Set(["s1", "s2"]));
  });
});

describe("ColonyRow", () => {
  it("collapsed: one compact line with no issue title, and aria-expanded false", () => {
    const markup = renderToStaticMarkup(<ColonyRow session={session()} open={false} onToggle={noop} onOpenColony={noop} onSelect={noop} />);
    expect(markup).not.toContain("Checkout fails for guest users");
    expect(markup).toContain("webshop#42");
    expect(markup).not.toContain("open colony →");
    expect(markup).toMatch(/aria-expanded="false"/);
    expect(markup).toMatch(/aria-controls="/);
  });

  it("expanded: reveals the issue title and a real link to the issue, and keeps the way in", () => {
    const markup = renderToStaticMarkup(<ColonyRow session={session()} open onToggle={noop} onOpenColony={noop} onSelect={noop} />);
    expect(markup).toContain("Checkout fails for guest users");
    expect(markup).toContain('href="https://github.com/acme/webshop/issues/42"');
    expect(markup).toContain("acme/webshop #42");
    expect(markup).toContain("open colony →");
    expect(markup).toMatch(/aria-expanded="true"/);
  });

  it("offers the open-colony affordance as a real enabled button, whatever the host Setup calls the machine", () => {
    // The overview never reads which platform the mothership is on — the affordance is a plain,
    // ungated <button>. The fixture only names the shape the ?runtime=other mock produces
    // (platform "other", kvm null — issue #214's unsupported-host scenario); with a colony to
    // inspect the way in must be present and live, not replaced by Settings.
    const unsupportedHost: HarnessStatus = {
      github: { connected: true, login: "octocat", name: "The Octocat", source: "gh CLI login" },
      claude: { configured: true, source: "Claude subscription", kind: "CLAUDE_CODE_OAUTH_TOKEN" },
      sandbox: { msb_version: "msb 0.6.18", image: "node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0", claude_bin: "/opt/claude/bin/claude", claude_bin_error: null },
      mesh: { enabled: true, provider: "headscale", state: "running", harness_ip: "100.64.0.1", nodes: 1, error: null },
      runtime: {
        platform: "other",
        kvm: null,
        git: { ok: true, version: "2.45.0" },
        gh: { ok: true, version: "2.60.0" },
        host_claude_bin: "/usr/local/bin/claude",
        host_claude_bin_error: null,
        os: { vendor: "unknown", name: "Other", version: null, id: null },
      },
    };
    expect(unsupportedHost.runtime?.platform).toBe("other");
    expect(unsupportedHost.runtime?.kvm).toBeNull();

    const markup = renderToStaticMarkup(<ColonyRow session={session()} open onToggle={noop} onOpenColony={noop} onSelect={noop} />);
    // A real focusable button — never a clickable div — and never disabled.
    expect(markup).toMatch(/<button type="button"[^>]*>open colony →<\/button>/);
    expect(markup).not.toContain("disabled");
  });

  it("never invents an issue link for a colony with no issue", () => {
    const markup = renderToStaticMarkup(<ColonyRow session={session({ issue: null, issue_title: "" })} open onToggle={noop} onOpenColony={noop} onSelect={noop} />);
    expect(markup).not.toContain("/issues/");
    expect(markup).toContain("acme/webshop");
  });

  it("says why a flagged colony needs attention", () => {
    const flagged = session({ attention: { reason: "stalled", since: "2026-09-18T09:05:00Z", nudges: 2 } });
    const markup = renderToStaticMarkup(<ColonyRow session={flagged} open onToggle={noop} onOpenColony={noop} onSelect={noop} />);
    expect(markup).toContain("No progress, nudged 2×");
  });

  it("a colony that needs you gets a shortcut that opens it in the inspector pane", () => {
    const markup = renderToStaticMarkup(
      <ColonyRow session={session({ status: "waiting_for_answer" })} open onToggle={noop} onOpenColony={noop} onSelect={noop} />,
    );
    expect(markup).toContain("answer in the pane →");
  });

  it("a colony that does not need you has no pane shortcut", () => {
    const markup = renderToStaticMarkup(<ColonyRow session={session()} open onToggle={noop} onOpenColony={noop} onSelect={noop} />);
    expect(markup).not.toContain("answer in the pane →");
  });
});

describe("OverviewView", () => {
  it("shows no issue title anywhere by default", () => {
    const markup = renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <OverviewView
          sessions={[session({ id: "s1" }), session({ id: "s2", repo: "acme/design-system", issue: 7, issue_title: "Bad contrast on the nav" })]}
          orgs={[ACME]}
          cost={null}
          onOpenOrg={noop}
          onOpenColony={noop}
          onSelect={noop}
        />
      </ApiContext.Provider>,
    );
    expect(markup).not.toContain("Checkout fails for guest users");
    expect(markup).not.toContain("Bad contrast on the nav");
    // One disclosure header per colony, all collapsed; no "open colony →" affordance leaks into the roster.
    expect(markup.match(/aria-expanded="false"/g)?.length).toBe(2);
    expect(markup).not.toContain("open colony →");
  });
});

describe("OverviewView spend", () => {  it("derives the header from the org rows, matching their sum and ignoring the session-based cost prop", () => {
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

  const renderOverview = (list: Session[], filter: OverviewFilter | null = null) =>
    renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <OverviewView
          sessions={list}
          orgs={workspaces}
          cost={null}
          initialFilter={filter}
          onOpenOrg={() => {}}
          onOpenColony={() => {}}
          onSelect={noop}
        />
      </ApiContext.Provider>,
    );

  it("scopes the counter chips to the visible workspaces and names the hidden org", () => {
    const html = renderOverview(sessions());
    // 14 live and 2 queued in acme/beta; gamma's 11 waiting must not reach any chip.
    expect(html).toContain(">14</span> live");
    expect(html).toContain(">0</span> need you");
    expect(html).toContain(">2</span> queued");
    expect(html).not.toContain(">11</span>");
    // ... but they are explained, not silently dropped: the scope line names gamma.
    expect(html).toContain("hidden org (gamma)");
    expect(html).toContain("11 need you");
  });

  it("labels an active bucket filter with a one-click clear", () => {
    const html = renderOverview(sessions(), "live");
    expect(html).toContain("showing 14 of 16");
    expect(html).toContain("clear ×");
  });

  it("an empty filtered page explains where the hidden colonies are and links back", () => {
    // "returned" matches nothing anywhere, so the page is empty: the 16 visible colonies sit in
    // other buckets and gamma's 11 wait in a hidden org. Neither may read as bare numbers.
    const html = renderOverview(sessions(), "returned");
    expect(html).toContain("nothing under");
    expect(html).toContain("16 in other buckets");
    expect(html).toContain("hidden org (gamma)");
    expect(html).toContain("clear filter ×");
  });
});

// Held slots and the stalled queue (issue #217): idle colonies whose PR autopilot holds occupy
// parallel slots without doing work. When every live colony is held and something queues, the
// queued chip must read as stalled — warn-bordered with the word itself — never as a healthy
// busy queue; and the header names how many slots are held.
describe("OverviewView held slots and stalled queue", () => {
  const idleHeld = (id: string): Session =>
    session({ id, status: "idle", attention: { reason: "autopilot_held", since: "2026-09-18T09:00:00Z", nudges: 0 } });

  const renderOverview = (list: Session[]) =>
    renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <OverviewView sessions={list} orgs={[ACME]} cost={null} onOpenOrg={noop} onOpenColony={noop} onSelect={noop} />
      </ApiContext.Provider>,
    );

  it("marks the queued counter stalled when every live colony is held", () => {
    const html = renderOverview([idleHeld("h1"), idleHeld("h2"), session({ id: "q1", status: "queued" })]);
    expect(html).toContain("queued · stalled");
    expect(html).toContain("border-warn");
    expect(html).toContain("2 held");
  });

  it("renders the queued counter normally while a colony is still working", () => {
    const html = renderOverview([idleHeld("h1"), session({ id: "r1", status: "running" }), session({ id: "q1", status: "queued" })]);
    expect(html).not.toContain("stalled");
    expect(html).toContain("1 held");
  });
});
