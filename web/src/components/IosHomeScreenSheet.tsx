// The Home-Screen sheet (issue #745): the step-by-step answer to "why can't I turn web push on?" on
// an iPhone or iPad. iOS only offers web push to a web app on the Home Screen (iOS 16.4+), and only
// Safari can put one there (issue #1083) — so Safari gets the install, in four steps, and every other
// iOS browser (Brave, Chrome, Firefox, Edge, Opera, an in-app webview) gets the hand-off to Safari
// instead: steps that name a menu item the browser does not have would lead nowhere. Presentational
// only: whether it applies here is `showIosInstallHint`, which the panes call.
import type { ReactElement } from "react";

import { currentAddress } from "../cockpitAddress";
import { isBraveBrowser, isIosDevice, isIosSafari } from "../launchUrl";
import { runningStandalone } from "../installApp";
import { InstallSteps } from "./InstallSteps";

/** Whether the sheet applies here: an iOS device, and not already the installed Home-Screen app. */
export function showIosInstallHint(
  userAgent: string = navigator.userAgent,
  maxTouchPoints: number = navigator.maxTouchPoints ?? 0,
  standalone: boolean = runningStandalone(),
): boolean {
  return isIosDevice(userAgent, maxTouchPoints) && !standalone;
}

/** Whether this iOS browser is Safari, the one that can follow the steps; anything else gets the hand-off. */
function thisIsIosSafari(): boolean {
  if (typeof navigator === "undefined") return true;
  return isIosSafari(navigator.userAgent, navigator.maxTouchPoints ?? 0, isBraveBrowser(navigator));
}

export function IosHomeScreenSheet({
  safari = thisIsIosSafari(),
  address = currentAddress(),
}: {
  /** Safari itself (the steps) or another iOS browser (the hand-off); detected when left out. */
  safari?: boolean;
  /** The cockpit address the hand-off copies: a bare bookmarkable url, never a token. */
  address?: string;
} = {}): ReactElement {
  if (!safari) return <InstallSteps platform="ios-other" address={address} />;
  return (
    <div role="note" aria-label="Add Colonizer to your Home Screen for web push" className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-muted">
      <p className="m-0 text-text">
        Web push on iPhone and iPad needs Colonizer on your Home Screen (iOS 16.4+):
      </p>
      <ol className="mb-0 mt-1.5 list-decimal space-y-0.5 pl-5">
        <li>Tap the Share button in Safari&rsquo;s toolbar.</li>
        <li>Choose &ldquo;Add to Home Screen&rdquo;.</li>
        <li>Open Colonizer from the Home Screen.</li>
        <li>Turn on notifications in Settings &rarr; Notifications.</li>
      </ol>
    </div>
  );
}
