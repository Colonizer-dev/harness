// The narrow-screen tab mapping (issue #516): every CockpitView lands on exactly one of the five
// bottom tabs, and the "More" sheet covers precisely what the four primary tabs do not.
import { describe, expect, it } from "vitest";

import type { CockpitView } from "./NavRail";
import { MOBILE_TABS, mobileMoreViews, mobileTabFor } from "./mobile";

describe("mobile tab mapping", () => {
  it("is exactly Nest, Inbox, Chat, Code, More, each a real view except More", () => {
    expect(MOBILE_TABS.map((tab) => tab.id)).toEqual(["nest", "inbox", "chat", "code", "more"]);
    expect(MOBILE_TABS.map((tab) => tab.label)).toEqual(["Nest", "Inbox", "Chat", "Code", "More"]);
    expect(MOBILE_TABS.map((tab) => tab.view)).toEqual(["home", "inbox", "chat", "code", null]);
  });

  it("lights the tab of the view itself, and Nest for an open colony", () => {
    expect(mobileTabFor("home")).toBe("nest");
    expect(mobileTabFor("colony")).toBe("nest");
    expect(mobileTabFor("inbox")).toBe("inbox");
    expect(mobileTabFor("chat")).toBe("chat");
    expect(mobileTabFor("code")).toBe("code");
    expect(mobileTabFor("settings")).toBe("more");
  });

  it("sends everything that is not a primary view to the More sheet", () => {
    const all: CockpitView[] = ["overview", "home", "colony", "launch", "inbox", "history", "loops", "settings", "memory", "host", "secrets", "code", "chat"];
    for (const view of all) expect(["nest", "inbox", "chat", "code", "more"]).toContain(mobileTabFor(view));
    expect(all.filter((v) => mobileTabFor(v) === "more")).toEqual(["overview", "launch", "history", "loops", "settings", "memory", "host", "secrets"]);
  });

  it("the More sheet lists precisely the non-primary views plus Settings, with no duplicates", () => {
    const sheet = mobileMoreViews();
    expect(sheet.map((item) => item.view)).toEqual(["overview", "launch", "history", "loops", "memory", "host", "secrets", "settings"]);
    const primary = MOBILE_TABS.map((tab) => tab.view).filter((v): v is CockpitView => v !== null);
    for (const item of sheet) expect(primary).not.toContain(item.view);
    expect(mobileTabFor("colony") === "nest" && sheet.some((item) => item.view === "colony")).toBe(false);
  });
});
