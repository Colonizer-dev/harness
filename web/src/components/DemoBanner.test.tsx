// The demo build's banner (issue #682): says the colonies are simulated and points at the install
// docs. Rendered to static markup: the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { DemoBanner, INSTALL_DOCS } from "./DemoBanner";

describe("DemoBanner", () => {
  it("names the demo and links the install docs", () => {
    const out = renderToStaticMarkup(<DemoBanner />);
    expect(out).toContain("Demo: simulated colonies, nothing runs.");
    expect(out).toContain(`href="${INSTALL_DOCS}"`);
    expect(out).toContain("Install Colonizer");
    expect(out).toContain(`role="note"`);
  });
});
