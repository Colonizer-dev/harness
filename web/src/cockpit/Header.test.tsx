// The cockpit's status bar (the unreachable indicator, issue #411, and the ticker) and its sidebar
// (the views, the workspace switcher that is the cockpit's scope, the theme and collapse controls).
// Rendered to static markup, as the cockpit's tests do.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { OrgEntry } from "../orgs";
import type { Session } from "../types";
import { actionError } from "./Cockpit";
import { Header, runningOrgs } from "./Header";
import { NavRail, navTabs, type CockpitView } from "./NavRail";

const entry = (org: string): OrgEntry => ({ org, live: 0, queued: 0, total: 1, pending: 0, avatar: null });

const header = ({
  statusError = false,
  orgs = [] as OrgEntry[],
  selectedOrg = null as string | null,
  needByOrg = {} as Record<string, number>,
  connection = "open" as "open" | "connecting" | "closed",
} = {}) =>
  renderToStaticMarkup(
    <Header orgs={orgs} selectedOrg={selectedOrg} onSelectOrg={() => {}} needByOrg={needByOrg} statusError={statusError} connection={connection as never} />,
  );

const rail = ({
  view = "home",
  orgs = [],
  selectedOrg = null,
  needByOrg = {},
  pendingMemory = 0,
  inboxCount = 0,
  expanded = true,
}: {
  view?: CockpitView;
  orgs?: OrgEntry[];
  selectedOrg?: string | null;
  needByOrg?: Record<string, number>;
  pendingMemory?: number;
  inboxCount?: number;
  expanded?: boolean;
} = {}) =>
  renderToStaticMarkup(
    <NavRail
      orgs={orgs}
      hiddenOrgs={[]}
      onOpenOrgSettings={() => {}}
      selectedOrg={selectedOrg}
      onSelectOrg={() => {}}
      needByOrg={needByOrg}
      view={view}
      onNavigate={() => {}}
      inboxCount={inboxCount}
      liveCount={0}
      pendingMemory={pendingMemory}
      theme="light"
      onToggleTheme={() => {}}
      initialExpanded={expanded}
    />,
  );

describe("Header status error", () => {
  it("stays quiet while the status poll succeeds", () => {
    expect(header()).not.toContain("Mothership unreachable");
  });

  it("names the outage, as a live region, while the poll fails", () => {
    const html = header({ statusError: true });
    expect(html).toContain("Mothership unreachable");
    expect(html).toContain('role="status"');
  });
});

describe("Header running workspaces", () => {
  const live = (org: string, n: number): OrgEntry => ({ ...entry(org), live: n });

  it("shows only workspaces with colonies running, busiest first", () => {
    expect(runningOrgs([live("a", 1), live("b", 0), live("c", 3)], null).map((o) => o.org)).toEqual(["c", "a"]);
  });

  it("keeps the filtered workspace in the row after it goes quiet, so the filter can be cleared", () => {
    expect(runningOrgs([live("a", 0), live("b", 2)], "A").map((o) => o.org)).toEqual(["b", "a"]);
  });

  it("shows no workspace avatars — the sidebar's switcher owns the scope — only the user's own", () => {
    const html = header({ orgs: [live("Acme", 2), live("octo", 1)], selectedOrg: "acme", needByOrg: { acme: 1 } });
    expect(html).not.toContain("running workspaces");
    expect(html).not.toContain("Acme · 2 running");
    const withUser = renderToStaticMarkup(
      <Header orgs={[]} selectedOrg={null} onSelectOrg={() => {}} needByOrg={{}} statusError={false}
        user={{ login: "octocat", name: "Mona", avatarUrl: null, onOpenSettings: () => {}, onOpenSecrets: () => {} }} />,
    );
    expect(withUser).toContain('aria-label="account · Mona (@octocat)"');
  });

  it("carries no crumb, ticker, counts or spend — only trouble speaks up", () => {
    const quiet = header({ orgs: [live("acme", 1)] });
    expect(quiet).not.toContain("All workspaces");
    expect(quiet).not.toContain("reconnecting");
    expect(header({ connection: "connecting" })).toContain("reconnecting…");
  });
});

describe("Header notifications bell", () => {
  const colony = (id: string, status: string): Session =>
    ({ id, repo: "acme/web", issue: 7, issue_title: "t", status, created_at: "2026-09-24T00:00:00Z", updated_at: "2026-09-24T00:00:00Z" }) as unknown as Session;
  const withBell = (sessions: Session[]) =>
    renderToStaticMarkup(
      <Header
        orgs={[]}
        selectedOrg={null}
        onSelectOrg={() => {}}
        needByOrg={{}}
        statusError={false}
        inbox={{ sessions, onOpenColony: () => {}, onOpenInbox: () => {}, onOpenNotificationSettings: () => {} }}
      />,
    );

  it("sits at the right of the bar, closed, counting the colonies that need you", () => {
    const html = withBell([colony("a", "waiting_for_answer"), colony("b", "waiting_for_answer"), colony("c", "running")]);
    expect(html).toMatch(/aria-label="notifications · 2 need you[^"]*" aria-haspopup="dialog" aria-expanded="false"/);
    expect(html).toContain(">2</span>");
    expect(html).not.toContain('role="dialog"');
  });

  it("is absent without an inbox, and quiet when nothing waits", () => {
    expect(header()).not.toContain("notifications");
    expect(withBell([colony("c", "running")])).not.toContain("need you");
  });
});

describe("NavRail views", () => {
  it("lists the views in order; launch and settings have their own buttons", () => {
    expect(navTabs({ needCount: 0, liveCount: 0, pendingMemory: 0 }).map((t) => t.view)).toEqual(["overview", "home", "chat", "code", "history", "loops", "memory", "host", "secrets"]);
    const html = rail();
    expect(html).toContain('aria-label="launch a colony"');
    expect(html).toContain('aria-label="settings"');
  });

  it("marks the current view and counts memory; the inbox is the header's bell, not a rail item", () => {
    const html = rail({ view: "memory", pendingMemory: 3, inboxCount: 2 });
    expect(html).toMatch(/aria-label="Memory · 3" aria-current="page"/);
    expect(html).not.toContain("Inbox");
  });

  it("keeps the theme toggle and the collapse control", () => {
    expect(rail()).toContain('aria-label="toggle theme"');
    expect(rail()).toContain('aria-label="collapse sidebar"');
    expect(rail({ expanded: false })).toContain('aria-label="expand sidebar"');
  });

  it("labels items when expanded, and names them in tooltips when collapsed", () => {
    expect(rail()).toContain(">Overview</span>");
    expect(rail({ expanded: false })).toContain('role="tooltip"');
  });
});

describe("NavRail workspace switcher", () => {
  const orgs = [entry("Acme"), entry("octo")];

  it("shows every workspace as the scope when none is chosen", () => {
    expect(rail({ orgs })).toContain("All workspaces");
  });

  it("shows the chosen org whatever its case", () => {
    expect(rail({ orgs, selectedOrg: "acme" })).toContain(">Acme</span>");
  });

  it("says what waits in the scope, keyed lowercase by needCountByOrg", () => {
    expect(rail({ orgs, selectedOrg: "Acme", needByOrg: { acme: 2 } })).toContain("2 need you");
    expect(rail({ orgs, needByOrg: { acme: 2, octo: 1 } })).toContain("3 need you");
  });
});

describe("actionError", () => {
  it("names the action, the colony and the reason", () => {
    expect(actionError("stop", "acme/webshop#42", new Error("boom"))).toBe("Couldn't stop acme/webshop#42: boom");
    expect(actionError("resume", "acme/webshop#42", "gone")).toBe("Couldn't resume acme/webshop#42: gone");
  });
});
