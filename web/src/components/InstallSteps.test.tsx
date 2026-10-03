// The install steps per platform, rendered to static markup: each browser gets the story it can
// actually act on, and the in-app/webview case gets the way across to Safari.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { InstallPlatform } from "../cockpitAddress";
import { InstallSteps } from "./InstallSteps";

const steps = (platform: InstallPlatform, extra: Record<string, unknown> = {}) =>
  renderToStaticMarkup(<InstallSteps platform={platform} address="https://h4xk.my.colonizer.dev" {...extra} />);

describe("InstallSteps", () => {
  it("walks iOS Safari through Share → Add to Home Screen", () => {
    const html = steps("ios-safari");
    expect(html).toContain("Add Colonizer to your Home Screen");
    expect(html).toContain("Share");
    expect(html).toContain("Add to Home Screen");
    expect((html.match(/<li/g) ?? []).length).toBe(3);
  });

  it("sends another iOS browser across to Safari, with a copy fallback", () => {
    const html = steps("ios-other");
    expect(html).toContain("Open in Safari");
    expect(html).toContain("x-safari-https://h4xk.my.colonizer.dev");
    expect(html).toContain("Copy address");
  });

  it("offers no Safari link for a plain-http address, only Copy and a paste hint", () => {
    const html = steps("ios-other", { address: "http://192.168.1.20:7878" });
    expect(html).not.toContain("Open in Safari");
    expect(html).not.toContain("x-safari-");
    expect(html).toContain("Copy address");
    expect(html).toContain("paste it into Safari");
  });

  it("offers Android Chrome the native prompt, else the menu path", () => {
    expect(steps("android-chrome", { installAvailable: true, onInstall: () => {} })).toContain("Install app");
    const menu = steps("android-chrome");
    expect(menu).toContain("⋮");
    expect(menu).toContain("Install app");
  });

  it("tells an installed app it is already there, and says nothing on the desktop", () => {
    expect(steps("installed")).toContain("Installed");
    expect(steps("desktop")).toBe("");
  });
});
