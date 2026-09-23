// The cockpit's status bar (the unreachable indicator, issue #411, and the ticker) and its sidebar
// (the views, the workspace switcher that is the cockpit's scope, the theme and collapse controls).
// Rendered to static markup, as the cockpit's tests do.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { OrgEntry } from "../orgs";
import { actionError } from "./Cockpit";
import { Header } from "./Header";
import { NavRail, navTabs, type CockpitView } from "./NavRail";

const entry = (org: string): OrgEntry => ({ org, live: 0, queued: 0, total: 1, pending: 0, avatar: null });

const header = ({ statusError = false, scope = null as string | null, latest = null as { id: string; kind: "asked"; text: string; at: number } | null } = {}) =>
  renderToStaticMarkup(
    <Header scope={scope} crumb="Nest" liveCount={0} needCount={0} cost={null} update={null} onOpenUpdates={() => {}} statusError={statusError} latest={latest} />,
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

describe("Header", () => {
  it("names the scope and the view", () => {
    expect(header()).toContain("All workspaces");
    expect(header({ scope: "acme" })).toContain("acme");
    expect(header()).toContain("Nest");
  });

  it("shows the latest change in the ticker", () => {
    expect(header({ latest: { id: "a", kind: "asked", text: "web #4 asked a question", at: 1 } })).toContain("web #4 asked a question");
  });
});

describe("NavRail views", () => {
  it("lists the views in order; launch and settings have their own buttons", () => {
    expect(navTabs({ needCount: 0, liveCount: 0, pendingMemory: 0 }).map((t) => t.view)).toEqual(["overview", "home", "inbox", "history", "memory"]);
    const html = rail();
    expect(html).toContain('aria-label="launch a colony"');
    expect(html).toContain('aria-label="settings"');
  });

  it("marks the current view and counts the inbox and memory", () => {
    const html = rail({ view: "memory", pendingMemory: 3, inboxCount: 2 });
    expect(html).toMatch(/aria-label="Memory · 3" aria-current="page"/);
    expect(html).toContain('aria-label="Inbox · 2"');
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
