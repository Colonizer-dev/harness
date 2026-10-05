// The install steps for a phone (issue #867), generalising the Home-Screen sheet: what to do to get
// Colonizer on the home screen, told for whichever browser is reading it. Presentational, plus the
// two actions a step needs — the browser's own install prompt, and copying the address for a browser
// that cannot add the app at all. A code comment below explains the x-safari- link.
import { useState, type ReactElement } from "react";

import type { InstallPlatform } from "../cockpitAddress";
import { Button } from "./ui";

/** Safari's share glyph, drawn inline for the "tap Share" step. */
function ShareIcon(): ReactElement {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" width={15} height={15} className="mr-0.5 inline-block align-[-2px]">
      <path d="M12 3v12M8 7l4-4 4 4M6 11v8h12v-8" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

/**
 * The address as an `x-safari-https://…` link — iOS hands that scheme straight to Safari. Null for
 * anything else: only the https scheme is known to work, and an http address cannot be installed as
 * an iOS web app anyway, so the caller offers Copy instead of a link that would just reload here.
 */
function safariLink(address: string): string | null {
  return address.startsWith("https://") ? address.replace(/^https:\/\//, "x-safari-https://") : null;
}

export function InstallSteps({
  platform,
  address = "",
  onInstall,
  installAvailable = false,
}: {
  platform: InstallPlatform;
  /** The address to open in the right browser; "" hides the offers that need it. */
  address?: string;
  /** The browser's install prompt, when one is waiting (useInstallPrompt().install). */
  onInstall?: () => Promise<boolean> | void;
  installAvailable?: boolean;
}): ReactElement | null {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(address);
      setCopied(true);
    } catch {
      /* nothing to copy into; the address is on screen anyway */
    }
  };
  const note = "rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px] text-muted";

  if (platform === "installed") {
    return (
      <p role="status" className={note}>
        Installed — open Colonizer from your home screen.
      </p>
    );
  }

  if (platform === "ios-safari") {
    return (
      <div role="note" aria-label="Add Colonizer to your Home Screen" className={note}>
        <p className="m-0 text-text">Add Colonizer to your Home Screen, and open it from there:</p>
        <ol className="mb-0 mt-1.5 list-decimal space-y-0.5 pl-5">
          <li>
            Tap the Share button <ShareIcon /> in the toolbar.
          </li>
          <li>Scroll down and choose &ldquo;Add to Home Screen&rdquo;.</li>
          <li>Open Colonizer from the Home Screen.</li>
        </ol>
      </div>
    );
  }

  // The other browsers on iOS — Brave, Chrome, Firefox, Edge, Opera, in-app webviews — cannot add a
  // web app to the Home Screen (issue #1083), and only a Home-Screen web app gets web push there. iOS
  // hands an `x-safari-https://…` link straight to Safari, so that is the way across when the address
  // is https. A plain-http address gets no link (it would just reload this webview; see safariLink) —
  // Copy and "paste it into Safari" carry it across instead. Android offers Copy too.
  if (platform === "ios-other") {
    const safari = address ? safariLink(address) : null;
    return (
      <div role="note" aria-label="Open Colonizer in Safari to install it" className={note}>
        <p className="m-0 text-text">Open this page in Safari to add Colonizer to your Home Screen.</p>
        <p className="m-0 mt-1">This browser has no &ldquo;Add to Home Screen&rdquo;, and on iPhone and iPad web push only reaches the app added from Safari.</p>
        {address && (
          <div className="mt-2 flex flex-wrap items-center gap-2">
            {safari && (
              <a
                href={safari}
                className="inline-flex min-h-11 items-center rounded-lg border border-border bg-panel px-3.5 text-[12.5px] no-underline text-text"
              >
                Open in Safari
              </a>
            )}
            <Button size="sm" className="min-h-11" onClick={() => void copy()}>
              {copied ? "Copied" : "Copy link"}
            </Button>
            {!safari && <span className="text-[12.5px] text-muted">then paste it into Safari</span>}
          </div>
        )}
      </div>
    );
  }

  if (platform === "android-chrome" && installAvailable && onInstall) {
    return (
      <div className="flex items-center gap-2">
        <Button size="sm" variant="primary" className="min-h-11" onClick={() => void onInstall()}>
          Install app
        </Button>
        <span className="min-w-0 flex-1 text-[12.5px] text-muted">Its own icon; same cockpit, same sign-in.</span>
      </div>
    );
  }

  if (platform === "android-chrome") {
    return (
      <div role="note" aria-label="Install Colonizer from Chrome's menu" className={note}>
        <p className="m-0">
          Open Chrome&rsquo;s menu (<span aria-hidden="true">⋮</span> at the top right) and choose <strong>Install app</strong> (or <strong>Add to Home screen</strong>).
        </p>
      </div>
    );
  }

  // android-other, desktop: nothing this component can do here.
  if (platform === "android-other") {
    return (
      <div role="note" aria-label="Open Colonizer in Chrome to install it" className={note}>
        <p className="m-0">This browser can&rsquo;t add Colonizer to your home screen. Open it in Chrome to install it.</p>
        {address && (
          <div className="mt-2">
            <Button size="sm" className="min-h-11" onClick={() => void copy()}>
              {copied ? "Copied" : "Copy address"}
            </Button>
          </div>
        )}
      </div>
    );
  }

  return null;
}
