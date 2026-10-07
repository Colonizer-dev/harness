// The Home-Screen sheet, rendered to static markup: the test environment has no DOM and no
// navigator, so the gating is exercised through `showIosInstallHint`'s arguments — the sheet
// itself must name the four steps and nothing platform-specific beyond them.
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import { IosHomeScreenSheet, showIosInstallHint } from "./IosHomeScreenSheet";

const iPhoneUA = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
const braveUA = iPhoneUA; // Brave on iOS sends Safari's agent unchanged
const iosUA = (token: string) =>
  `Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) ${token} Mobile/15E148 Safari/604.1`;
const iPadAsMacUA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15";
const macSafariUA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15";

describe("showIosInstallHint", () => {
  it("shows on an iOS browser that is not the installed app", () => {
    expect(showIosInstallHint(iPhoneUA, 5, false)).toBe(true);
  });

  it("applies in every iOS browser — the sheet then picks steps or the Safari hand-off", () => {
    for (const ua of [braveUA, iosUA("CriOS/124.0"), iosUA("FxiOS/127.0"), iosUA("EdgiOS/125.0"), iosUA("OPiOS/16.0")]) {
      expect(showIosInstallHint(ua, 5, false)).toBe(true);
    }
    expect(showIosInstallHint(iPadAsMacUA, 5, false)).toBe(true);
  });

  it("stays hidden in standalone mode, whatever the browser", () => {
    expect(showIosInstallHint(iosUA("CriOS/124.0"), 5, true)).toBe(false);
    expect(showIosInstallHint(iPadAsMacUA, 5, true)).toBe(false);
  });

  it("stays hidden in the installed app and on anything not iOS", () => {
    expect(showIosInstallHint(iPhoneUA, 5, true)).toBe(false);
    expect(showIosInstallHint(macSafariUA, 0, false)).toBe(false);
    expect(showIosInstallHint("", 0, false)).toBe(false);
  });
});

describe("IosHomeScreenSheet", () => {
  const html = renderToStaticMarkup(<IosHomeScreenSheet safari address="https://h4xk.my.colonizer.dev" />);

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

describe("IosHomeScreenSheet outside Safari (#1083)", () => {
  const html = renderToStaticMarkup(<IosHomeScreenSheet safari={false} address="https://h4xk.my.colonizer.dev" />);

  it("hands over to Safari instead of naming steps this browser cannot follow", () => {
    expect(html).toContain("Open this page in Safari to add Colonizer to your Home Screen");
    expect(html).not.toContain("<ol");
    expect(html).not.toContain("Choose");
    expect(html).toContain("web push only reaches the app added from Safari");
  });

  it("offers Copy link with the bare cockpit address, never a token", () => {
    expect(html).toContain("Copy link");
    expect(html).toContain("x-safari-https://h4xk.my.colonizer.dev");
    expect(html).not.toContain("token");
  });
});

describe("IosHomeScreenSheet detecting the browser itself", () => {
  afterEach(() => vi.unstubAllGlobals());
  const render = (nav: Record<string, unknown>) => {
    vi.stubGlobal("navigator", { maxTouchPoints: 5, ...nav });
    return renderToStaticMarkup(<IosHomeScreenSheet address="https://h4xk.my.colonizer.dev" />);
  };

  it("shows Brave on iOS (Safari's agent + navigator.brave) the hand-off, not the steps", () => {
    const html = render({ userAgent: braveUA, brave: { isBrave: () => Promise.resolve(true) } });
    expect(html).toContain("Open this page in Safari");
    expect(html).not.toContain("<ol");
  });

  it("shows Chrome, Firefox and Edge on iOS the hand-off", () => {
    for (const token of ["CriOS/124.0", "FxiOS/127.0", "EdgiOS/125.0"]) {
      expect(render({ userAgent: iosUA(token) })).toContain("Open this page in Safari");
    }
  });

  it("shows Safari itself the steps", () => {
    const html = render({ userAgent: iPhoneUA });
    expect(html).toContain("<ol");
    expect(html).not.toContain("Open this page in Safari");
  });
});
