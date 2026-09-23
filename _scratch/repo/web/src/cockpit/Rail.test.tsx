// The rail's foot (issue #203): settings is the bottom-most item, the bottom-left corner of the
// screen, with the theme toggle directly above it. Rendered to static markup, as the cockpit's tests do.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { OrgEntry } from "../orgs";
import { Rail, type CockpitView } from "./Rail";

const entry = (org: string): OrgEntry => ({ org, live: 0, queued: 0, total: 1, pending: 0, avatar: null });

const render = (
  view: CockpitView = "home",
  { orgs = [], selectedOrg = null, needByOrg = {}, pendingMemory = 0 }: { orgs?: OrgEntry[]; selectedOrg?: string | null; needByOrg?: Record<string, number>; pendingMemory?: number } = {},
) =>
  renderToStaticMarkup(
    <Rail
      orgs={orgs}
      selectedOrg={selectedOrg}
      onSelectOrg={() => {}}
      view={view}
      onNavigate={() => {}}
      needCount={0}
      needByOrg={needByOrg}
      pendingMemory={pendingMemory}
      theme="light"
      onToggleTheme={() => {}}
    />,
  );

/** The rail's buttons' aria-labels, top to bottom. */
const buttons = (html: string) => [...html.matchAll(/<button[^>]*aria-label="([^"]*)"/g)].map((m) => m[1]);

describe("Rail", () => {
  it("ends with the theme toggle and then settings", () => {
    expect(buttons(render()).slice(-4)).toEqual(["history", "inbox", "toggle theme", "settings"]);
  });

  it("keeps settings' hover label and pressed state", () => {
    const html = render("settings");
    expect(html).toContain("settings · modules");
    expect(html).toMatch(/aria-label="settings" aria-pressed="true"/);
  });
});

// The org list (issue #411): a way back to every workspace, case-insensitive matching, and memory.
describe("Rail orgs", () => {
  const orgs = [entry("Acme"), entry("octo")];

  it("offers all workspaces first, pressed while no org is chosen", () => {
    const html = render("home", { orgs });
    expect(buttons(html).slice(1, 4)).toEqual(["all workspaces", "Acme", "octo"]);
    expect(html).toMatch(/aria-label="all workspaces" aria-pressed="true"/);
  });

  it("marks the chosen org whatever its case, and says a second click clears it", () => {
    const html = render("home", { orgs, selectedOrg: "acme" });
    expect(html).toMatch(/aria-label="all workspaces" aria-pressed="false"/);
    expect(html).toMatch(/aria-label="Acme · selected, click for all workspaces" aria-pressed="true"/);
    expect(html).toMatch(/aria-label="octo" aria-pressed="false"/);
  });

  it("reads the need count keyed lowercase by needCountByOrg", () => {
    expect(render("home", { orgs, needByOrg: { acme: 2 } })).toContain('aria-label="Acme · 2 need you"');
  });

  it("puts the org list in its own scroll region, apart from the foot", () => {
    const html = render("home", { orgs });
    const region = html.slice(html.indexOf('aria-label="workspaces"'), html.indexOf('aria-label="launch a colony"'));
    expect(region).toContain("overflow-y-auto");
    expect(region).toContain('aria-label="octo"');
    expect(region).not.toContain('aria-label="settings"');
  });

  it("has a memory item with the proposals badge", () => {
    expect(buttons(render("memory"))).toContain("memory");
    const html = render("memory", { pendingMemory: 3 });
    expect(html).toMatch(/aria-label="memory · 3 to review" aria-pressed="true"/);
  });
});
