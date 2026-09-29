// The Home-Screen sheet, rendered to static markup: the test environment has no DOM and no
// navigator, so the gating is exercised through `showIosInstallHint`'s arguments — the sheet
// itself must name the four steps and nothing platform-specific beyond them.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { IosHomeScreenSheet, showIosInstallHint } from "./IosHomeScreenSheet";

const iPhoneUA = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
const macSafariUA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15";

describe("showIosInstallHint", () => {
  it("shows on an iOS browser that is not the installed app", () => {
    expect(showIosInstallHint(iPhoneUA, 5, false)).toBe(true);
  });

  it("stays hidden in the installed app and on anything not iOS", () => {
    expect(showIosInstallHint(iPhoneUA, 5, true)).toBe(false);
    expect(showIosInstallHint(macSafariUA, 0, false)).toBe(false);
    expect(showIosInstallHint("", 0, false)).toBe(false);
  });
});

describe("IosHomeScreenSheet", () => {
  const html = renderToStaticMarkup(<IosHomeScreenSheet />);

  it("names the whole Add to Home Screen route, step by step", () => {
    expect(html).toContain("Home Screen");
    expect(html).toContain("iOS 16.4+");
    expect(html).toContain("Share");
    expect(html).toContain("Add to Home Screen");
    expect(html).toContain("Settings");
    expect(html).toContain("<ol");
    expect((html.match(/<li/g) ?? []).length).toBe(4);
  });

  it("carries an accessible name so it reads as one note", () => {
    expect(html).toContain('role="note"');
    expect(html).toContain("aria-label");
  });
});
