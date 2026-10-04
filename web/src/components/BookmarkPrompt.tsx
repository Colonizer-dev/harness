// The bookmark prompt (issue #867): shown once per device in the signed-in cockpit, it asks for a
// bookmark (or the app install) and remembers the answer — a dismissed or installed device never
// sees it again unless the Your cockpit card re-offers it. A desktop gets the shortcut and, when the
// browser has one waiting, an Install button; a phone gets its platform's install steps.
import { useEffect, useState, type ReactElement } from "react";

import { bookmarkShortcut, currentAddress, dismissBookmark, installPlatform, markBookmarkInstalled, shouldOfferBookmark } from "../cockpitAddress";
import { useInstallPrompt, useStandalone } from "../installApp";
import { InstallSteps } from "./InstallSteps";
import { Button } from "./ui";

export function BookmarkPrompt({ address = currentAddress() }: { address?: string }): ReactElement | null {
  const [hidden, setHidden] = useState(false);
  const standalone = useStandalone();
  const { available, install } = useInstallPrompt();
  const platform = installPlatform(undefined, standalone);

  // Running as the installed app is the same as having installed it: remember and stand down.
  useEffect(() => {
    if (!standalone) return;
    markBookmarkInstalled();
    setHidden(true);
  }, [standalone]);

  // The browser finishing an install (from anywhere in the cockpit) counts too.
  useEffect(() => {
    const onInstalled = () => {
      markBookmarkInstalled();
      setHidden(true);
    };
    window.addEventListener("appinstalled", onInstalled);
    return () => window.removeEventListener("appinstalled", onInstalled);
  }, []);

  if (hidden || platform === "installed" || !shouldOfferBookmark({ standalone })) return null;

  const dismiss = () => {
    dismissBookmark();
    setHidden(true);
  };
  const close = (
    <button
      type="button"
      onClick={dismiss}
      aria-label="Dismiss"
      className="-m-1 grid size-11 shrink-0 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
    >
      ×
    </button>
  );
  const phone = platform !== "desktop";

  return (
    <div
      role="region"
      aria-label="Bookmark this cockpit"
      className="fixed inset-x-3 bottom-[calc(0.75rem+env(safe-area-inset-bottom))] z-40 mx-auto max-w-md rounded-2xl border border-border bg-panel p-4 shadow-[var(--shadow)] motion-safe:transition max-sm:mx-auto sm:inset-x-auto sm:right-4 sm:w-96"
    >
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <h2 className="text-[14px] font-semibold">Bookmark this cockpit</h2>
          <p className="mt-0.5 text-[12.5px] text-muted">
            {phone
              ? "Keep Colonizer a tap away: add it to your home screen, or bookmark this address."
              : `Press ${bookmarkShortcut()} to keep this address. Unlike the one-time link in your terminal, it doesn't change.`}
          </p>
        </div>
        {close}
      </div>
      <div className="mt-3 space-y-2.5">
        {phone ? (
          <InstallSteps platform={platform} address={address} onInstall={install} installAvailable={available} />
        ) : (
          <div className="flex flex-wrap items-center gap-2">
            {available && (
              <Button variant="primary" className="min-h-11" onClick={() => void install()}>
                Install app
              </Button>
            )}
            <Button className="min-h-11" onClick={dismiss}>
              Got it
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}
