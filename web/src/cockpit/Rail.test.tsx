// The rail's foot (issue #203): settings is the bottom-most item, the bottom-left corner of the
// screen, with the theme toggle directly above it. Rendered to static markup, as the cockpit's tests do.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { Rail, type CockpitView } from "./Rail";

const render = (view: CockpitView = "home") =>
  renderToStaticMarkup(
    <Rail
      orgs={[]}
      selectedOrg={null}
      onSelectOrg={() => {}}
      view={view}
      onNavigate={() => {}}
      needCount={0}
      needByOrg={{}}
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
