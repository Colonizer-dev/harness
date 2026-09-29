// The cockpit as an installable app: registers the service worker (production builds only) and
// keeps the browser's install prompt so Settings → Desktop can offer an "Install app" button.
import { useEffect, useState } from "react";

import { DEMO } from "./demo";

interface InstallPromptEvent extends Event {
  prompt: () => Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed" }>;
}

let deferred: InstallPromptEvent | null = null;
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((l) => l());

// The waiting worker of an update that has installed but not taken over. Module state rather than
// component state, so the first render already sees an update that was waiting before the page
// loaded; the update prompt (UpdatePrompt) reads it through useAppUpdate.
let waiting: ServiceWorker | null = null;
const updateListeners = new Set<() => void>();
// Bumped every time the waiting worker changes identity — it is only called on a change — so hooks
// keyed on it re-arm: a prompt the visitor put off is asked again for the next build (UpdatePrompt).
let waitingTick = 0;
const notifyUpdate = () => {
  waitingTick += 1;
  updateListeners.forEach((l) => l());
};
// Set only by the tab that asked the new worker to take over. controllerchange fires in every tab
// when activate claims them, and only the asking one follows it — the others keep running the old
// build (its chunks stay cached) until they reload themselves.
let askedToSkip = false;

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
  // The service worker only in a real build served by the mothership: the dev server, the
  // in-browser mock (?mock=1) and the hosted demo have no stable /assets to cache.
  const mock = new URLSearchParams(window.location.search).has("mock");
  if (import.meta.env.PROD && !mock && !DEMO && "serviceWorker" in navigator) {
    navigator.serviceWorker.addEventListener("controllerchange", () => {
      // The waiting worker this page may have been prompted about has just become the controller —
      // however it got there, there is nothing left to wait for, so retire the prompt.
      if (waiting !== null) {
        waiting = null;
        notifyUpdate();
      }
      if (!askedToSkip) return;
      askedToSkip = false;
      window.location.reload();
    });
    window.addEventListener("load", () => {
      navigator.serviceWorker
        .register("/sw.js", { scope: "/" })
        .then((registration) => {
          // An update deployed while this tab was shut is already waiting here; one deployed while
          // the tab runs arrives through updatefound and reaches "installed". Without a controller —
          // the very first install — the worker activates at once and there is nothing to prompt.
          const seen = () => {
            const next = registration.waiting && navigator.serviceWorker.controller ? registration.waiting : null;
            if (next !== waiting) {
              waiting = next;
              notifyUpdate();
            }
          };
          seen();
          registration.addEventListener("updatefound", () => {
            const installing = registration.installing;
            installing?.addEventListener("statechange", () => {
              if (installing.state === "installed") seen();
            });
          });
        })
        .catch(() => {
          /* installable-app extras only; the cockpit works without them */
        });
    });
    // A long-lived tab notices a mothership deploy the next time it is seen; the browser checks on
    // its own on navigations, so this stays cheap and runs at most hourly.
    let lastCheck = Date.now();
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState !== "visible" || Date.now() - lastCheck < 3_600_000) return;
      lastCheck = Date.now();
      navigator.serviceWorker.getRegistration().then((r) => r?.update().catch(() => undefined));
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

/** Whether an updated build is waiting to take over, a token that changes whenever the waiting
 *  worker does, and the reload that brings it in: the waiting worker is told to skip waiting, and
 *  this tab reloads itself once the new worker has claimed it. */
export function useAppUpdate(): { ready: boolean; token: number; reload: () => void } {
  const [update, setUpdate] = useState(() => ({ ready: waiting !== null, token: waitingTick }));
  useEffect(() => {
    const l = () => setUpdate({ ready: waiting !== null, token: waitingTick });
    updateListeners.add(l);
    l();
    return () => {
      updateListeners.delete(l);
    };
  }, []);
  return {
    ready: update.ready,
    token: update.token,
    reload: () => {
      if (!waiting) return;
      askedToSkip = true;
      // A worker that vanished between the check and the send throws; then the flag goes back, so
      // only a skip-waiting this tab actually asked for reloads it on controllerchange.
      try {
        waiting.postMessage({ type: "colonizer:skip-waiting" });
      } catch {
        askedToSkip = false;
      }
    },
  };
}
