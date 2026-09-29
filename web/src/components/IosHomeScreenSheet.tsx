// The Home-Screen sheet (issue #745): the step-by-step answer to "why can't I turn web push on?" on
// an iPhone or iPad. Safari — and every browser on iOS, which shares its WebKit — only offers web
// push to a web app that has been added to the Home Screen (iOS 16.4+), so the guidance is the
// install, in four steps, shown where the push switch would otherwise look broken. Presentational
// only: whether it applies here is `showIosInstallHint`, which the panes call.
import type { ReactElement } from "react";

import { isIosSafari } from "../launchUrl";
import { runningStandalone } from "../installApp";

/** Whether the sheet applies here: an iOS device, and not already the installed Home-Screen app. */
export function showIosInstallHint(
  userAgent: string = navigator.userAgent,
  maxTouchPoints: number = navigator.maxTouchPoints ?? 0,
  standalone: boolean = runningStandalone(),
): boolean {
  return isIosSafari(userAgent, maxTouchPoints) && !standalone;
}

export function IosHomeScreenSheet(): ReactElement {
  return (
    <div role="note" aria-label="Add Colonizer to your Home Screen for web push" className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px] text-muted">
      <p className="m-0 text-text">
        Web push on iPhone and iPad needs Colonizer on your Home Screen (iOS 16.4+):
      </p>
      <ol className="mb-0 mt-1.5 list-decimal space-y-0.5 pl-5">
        <li>Tap the Share button in the browser&rsquo;s toolbar.</li>
        <li>Choose &ldquo;Add to Home Screen&rdquo;.</li>
        <li>Open Colonizer from the Home Screen.</li>
        <li>Turn on notifications in Settings &rarr; Notifications.</li>
      </ol>
    </div>
  );
}
