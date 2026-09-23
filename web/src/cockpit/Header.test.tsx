// The cockpit header (v3 shell): the unreachable indicator (issue #411), the view tabs that
// replaced the rail, and the workspace avatars that carry the rail's org list. Rendered to static
// markup, as the cockpit's tests do.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { OrgEntry } from "../orgs";
import { actionError } from "./Cockpit";
import { Header, navTabs, type CockpitView } from "./Header";

const entry = (org: string): OrgEntry => ({ org, live: 0, queued: 0, total: 1, pending: 0, avatar: null });

const render = ({
  statusError = false,
  view = "home",
  orgs = [],
  selectedOrg = null,
  needByOrg = {},
  pendingMemory = 0,
  inboxCount = 0,
  latest = null,
}: {
  statusError?: boolean;
  view?: CockpitView;
  orgs?: OrgEntry[];
  selectedOrg?: string | null;
  needByOrg?: Record<string, number>;
  pendingMemory?: number;
  inboxCount?: number;
  latest?: { id: string; kind: "asked"; text: string; at: number } | null;
} = {}) =>
  renderToStaticMarkup(
    <Header
      orgs={orgs}
      hiddenOrgs={[]}
      selectedOrg={selectedOrg}
      onSelectOrg={() => {}}
      onOpenOrgSettings={() => {}}
      needByOrg={needByOrg}
      view={view}
      onNavigate={() => {}}
      inboxCount={inboxCount}
      pendingMemory={pendingMemory}
      liveCount={0}
      needCount={0}
      cost={null}
      update={null}
      onOpenUpdates={() => {}}
      statusError={statusError}
      latest={latest}
      theme="light"
      onToggleTheme={() => {}}
    />,
  );

describe("Header status error", () => {
  it("stays quiet while the status poll succeeds", () => {
    expect(render()).not.toContain("Mothership unreachable");
  });

  it("names the outage, as a live region, while the poll fails", () => {
    const html = render({ statusError: true });
    expect(html).toContain("Mothership unreachable");
    expect(html).toContain('role="status"');
  });
});

describe("Header view tabs", () => {
  it("reaches every view the rail did, in order", () => {
    expect(navTabs({ needCount: 0, liveCount: 0, pendingMemory: 0 }).map((t) => t.view)).toEqual([
      "overview",
      "home",
      "inbox",
      "history",
      "launch",
      "memory",
      "settings",
    ]);
  });

  it("marks the current view and counts the inbox and memory", () => {
    const html = render({ view: "memory", pendingMemory: 3, inboxCount: 2 });
    expect(html).toMatch(/aria-label="Memory · 3" aria-current="page"/);
    expect(html).toContain('aria-label="Inbox · 2"');
  });

  it("keeps the theme toggle", () => {
    expect(render()).toContain('aria-label="toggle theme"');
  });

  it("shows the latest change in the ticker", () => {
    expect(render({ latest: { id: "a", kind: "asked", text: "web #4 asked a question", at: 1 } })).toContain("web #4 asked a question");
  });
});

describe("Header workspaces", () => {
  const orgs = [entry("Acme"), entry("octo")];

  it("marks the chosen org whatever its case, and says a second click clears it", () => {
    const html = render({ orgs, selectedOrg: "acme" });
    expect(html).toMatch(/aria-label="Acme · selected, click for all workspaces" aria-pressed="true"/);
    expect(html).toMatch(/aria-label="octo" aria-pressed="false"/);
  });

  it("reads the need count keyed lowercase by needCountByOrg", () => {
    expect(render({ orgs, needByOrg: { acme: 2 } })).toContain('aria-label="Acme · 2 need you"');
  });

  it("names every workspace in the switcher when none is chosen", () => {
    expect(render({ orgs })).toContain("All workspaces");
  });
});

describe("actionError", () => {
  it("names the action, the colony and the reason", () => {
    expect(actionError("stop", "acme/webshop#42", new Error("boom"))).toBe("Couldn't stop acme/webshop#42: boom");
    expect(actionError("resume", "acme/webshop#42", "gone")).toBe("Couldn't resume acme/webshop#42: gone");
  });
});
