// The header's unreachable indicator (issue #411): the wide layout has no StatusRow, so the
// header says it itself when the status poll is failing. Rendered to static markup, as the
// cockpit's tests do.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { actionError } from "./Cockpit";
import { Header } from "./Header";

const render = (statusError: boolean) =>
  renderToStaticMarkup(
    <Header
      orgs={[]}
      hiddenOrgs={[]}
      selectedOrg={null}
      onSelectOrg={() => {}}
      onOpenOrgSettings={() => {}}
      needByOrg={{}}
      crumb="nest"
      liveCount={0}
      needCount={0}
      cost={null}
      update={null}
      onOpenUpdates={() => {}}
      statusError={statusError}
    />,
  );

describe("Header status error", () => {
  it("stays quiet while the status poll succeeds", () => {
    expect(render(false)).not.toContain("Mothership unreachable");
  });

  it("names the outage, as a live region, while the poll fails", () => {
    const html = render(true);
    expect(html).toContain("Mothership unreachable");
    expect(html).toContain('role="status"');
  });
});

describe("actionError", () => {
  it("names the action, the colony and the reason", () => {
    expect(actionError("stop", "acme/webshop#42", new Error("boom"))).toBe("Couldn't stop acme/webshop#42: boom");
    expect(actionError("resume", "acme/webshop#42", "gone")).toBe("Couldn't resume acme/webshop#42: gone");
  });
});
