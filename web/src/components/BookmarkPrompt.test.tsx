// The bookmark prompt's gating, rendered to static markup with a stubbed window/navigator: it offers
// once, gets the browser's own shortcut on the desktop, shows the Home-Screen steps on an iPhone,
// and never comes back once dismissed, installed or already running as the app.
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import { BookmarkPrompt } from "./BookmarkPrompt";

const desktopUA = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const iPhoneUA = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";

function stub({ stored = null, standalone = false, userAgent = desktopUA }: { stored?: string | null; standalone?: boolean; userAgent?: string } = {}) {
  vi.stubGlobal("navigator", { userAgent, maxTouchPoints: 5, standalone });
  vi.stubGlobal("window", {
    localStorage: { getItem: () => stored, setItem: () => {}, removeItem: () => {} },
    matchMedia: () => ({ matches: standalone, addEventListener() {}, removeEventListener() {} }),
    addEventListener() {},
    removeEventListener() {},
  });
}

afterEach(() => vi.unstubAllGlobals());

const html = () => renderToStaticMarkup(<BookmarkPrompt address="http://127.0.0.1:7878" />);

describe("BookmarkPrompt", () => {
  it("offers the desktop bookmark with the browser's shortcut", () => {
    stub();
    const out = html();
    expect(out).toContain("Bookmark this cockpit");
    expect(out).toContain("Ctrl+D");
    expect(out).toContain('aria-label="Dismiss"');
  });

  it("shows the Home-Screen steps on an iPhone", () => {
    stub({ userAgent: iPhoneUA });
    expect(html()).toContain("Add Colonizer to your Home Screen");
  });

  it("never comes back once dismissed or installed", () => {
    stub({ stored: "dismissed" });
    expect(html()).toBe("");
    stub({ stored: "installed" });
    expect(html()).toBe("");
  });

  it("stays away in the installed app", () => {
    stub({ standalone: true });
    expect(html()).toBe("");
  });
});
