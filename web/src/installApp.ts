// The cockpit as an installable app: registers the service worker (production builds only) and
// keeps the browser's install prompt so Settings → Desktop can offer an "Install app" button.
import { useEffect, useState } from "react";

interface InstallPromptEvent extends Event {
  prompt: () => Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed" }>;
}

let deferred: InstallPromptEvent | null = null;
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((l) => l());

/** Whether this page is already running as the installed app. */
export function runningStandalone(): boolean {
  try {
    return (
      window.matchMedia("(display-mode: standalone)").matches ||
      (navigator as Navigator & { standalone?: boolean }).standalone === true
    );
  } catch {
    return false;
  }
}

/** Safari installs through its own menu (File → Add to Dock), with no prompt event. */
export function isSafari(): boolean {
  const ua = navigator.userAgent;
  return /Safari\//.test(ua) && !/Chrome\/|Chromium\/|Edg\//.test(ua);
}

export function setupInstallApp(): void {
  window.addEventListener("beforeinstallprompt", (event) => {
    event.preventDefault();
    deferred = event as InstallPromptEvent;
    notify();
  });
  window.addEventListener("appinstalled", () => {
    deferred = null;
    notify();
  });
  // The service worker only in a real build served by the mothership: the dev server and the
  // in-browser mock (?mock=1) have no stable /assets to cache.
  const mock = new URLSearchParams(window.location.search).has("mock");
  if (import.meta.env.PROD && !mock && "serviceWorker" in navigator) {
    window.addEventListener("load", () => {
      navigator.serviceWorker.register("/sw.js", { scope: "/" }).catch(() => {
        /* installable-app extras only; the cockpit works without them */
      });
    });
  }
}

/** Whether the browser offered to install, and the function that shows its prompt. */
export function useInstallPrompt(): { available: boolean; install: () => Promise<boolean> } {
  const [, setTick] = useState(0);
  useEffect(() => {
    const l = () => setTick((n) => n + 1);
    listeners.add(l);
    return () => {
      listeners.delete(l);
    };
  }, []);
  return {
    available: deferred !== null,
    install: async () => {
      if (!deferred) return false;
      const event = deferred;
      await event.prompt();
      const { outcome } = await event.userChoice;
      deferred = null;
      notify();
      return outcome === "accepted";
    },
  };
}
