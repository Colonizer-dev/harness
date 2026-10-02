// The sheet a freshly paired phone lands on (issue #746): it offers what is worth doing once —
// installing and notifications — in whichever form this browser supports, and nothing it cannot do.
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import { PhoneWelcomeSheet } from "./PhoneWelcomeSheet";

const sheet = () =>
  renderToStaticMarkup(
    <ApiContext.Provider value={{} as Api}>
      <PhoneWelcomeSheet onClose={() => {}} />
    </ApiContext.Provider>,
  );

afterEach(() => vi.unstubAllGlobals());

describe("PhoneWelcomeSheet", () => {
  it("says the phone is signed in and can be dismissed", () => {
    const html = sheet();
    expect(html).toContain("You&#x27;re signed in");
    expect(html).toContain('aria-label="Dismiss"');
    expect(html).toContain('role="dialog"');
  });

  it("without an install prompt or notifications, points to later rather than offering buttons that cannot work", () => {
    vi.stubGlobal("Notification", undefined);
    const html = sheet();
    expect(html).toContain("Install later from Settings");
    expect(html).not.toContain("Turn on notifications");
  });

  it("offers notifications while the permission is still undecided", () => {
    vi.stubGlobal("window", globalThis);
    vi.stubGlobal("Notification", { permission: "default" });
    expect(sheet()).toContain("Turn on notifications");
    vi.stubGlobal("Notification", { permission: "granted" });
    expect(sheet()).not.toContain("Turn on notifications");
  });

  it("on iPhone Safari, shows the Home Screen steps instead of an install button", () => {
    vi.stubGlobal("navigator", {
      userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1",
      maxTouchPoints: 5,
    });
    const html = sheet();
    expect(html).not.toContain("Install later from Settings");
    expect(html).toContain("Home Screen");
  });
});
