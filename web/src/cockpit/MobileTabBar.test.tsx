// The mobile tab bar (issue #516), rendered to static markup: the test environment has no DOM, so
// only the closed state — five tabs, the active one named, the inbox count beside its tab.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { MobileTabBar } from "./MobileTabBar";

const bar = (view: Parameters<typeof MobileTabBar>[0]["view"], inboxCount = 0) =>
  renderToStaticMarkup(<MobileTabBar view={view} onNavigate={() => undefined} inboxCount={inboxCount} />);

describe("MobileTabBar", () => {
  it("renders exactly the five tabs, and only below the sm breakpoint", () => {
    const out = bar("home");
    for (const label of ["Nest", "Inbox", "Chat", "Code", "More"]) expect(out).toContain(label);
    expect(out).toContain("sm:hidden");
    expect(out).toContain("safe-area-inset-bottom");
    expect(out).not.toContain("Overview"); // the More sheet is closed
  });

  it("names where you are, colony included", () => {
    expect(bar("home")).toContain('aria-current="page"');
    expect(bar("colony")).toContain('aria-current="page"');
    expect(bar("home", 2)).toContain("Inbox · 2");
  });

  it("the More sheet's views stay out of the tab row itself", () => {
    const out = bar("settings");
    expect(out).toContain('aria-current="page"');
    expect(out).toContain("More");
    expect(out).not.toContain("History");
  });
});
