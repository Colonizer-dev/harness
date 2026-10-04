// The install prompt's module state, exercised through its exported attach/install helpers against a
// plain EventTarget — no window, no real browser. attachInstallPrompt is what setupInstallApp wires
// to the window; a fake beforeinstallprompt stands in for Chrome's.
import { describe, expect, it, vi } from "vitest";

import { attachInstallPrompt, installPromptAvailable, showInstallPrompt } from "./installApp";

const promptEvent = (outcome: "accepted" | "dismissed") =>
  Object.assign(new Event("beforeinstallprompt", { cancelable: true }), {
    prompt: vi.fn().mockResolvedValue(undefined),
    userChoice: Promise.resolve({ outcome }),
  });

describe("install prompt", () => {
  it("keeps a beforeinstallprompt and answers with its outcome", async () => {
    const target = new EventTarget();
    attachInstallPrompt(target);
    expect(installPromptAvailable()).toBe(false);

    const event = promptEvent("accepted");
    target.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    expect(installPromptAvailable()).toBe(true);

    await expect(showInstallPrompt()).resolves.toBe(true);
    expect(installPromptAvailable()).toBe(false);
  });

  it("retires the prompt once the app is installed", () => {
    const target = new EventTarget();
    attachInstallPrompt(target);
    target.dispatchEvent(promptEvent("dismissed"));
    expect(installPromptAvailable()).toBe(true);
    target.dispatchEvent(new Event("appinstalled"));
    expect(installPromptAvailable()).toBe(false);
  });

  it("answers false when nothing is waiting", async () => {
    await expect(showInstallPrompt()).resolves.toBe(false);
  });
});
