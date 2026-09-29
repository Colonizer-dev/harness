// The hosted demo's banner (issue #682): the demo build runs the in-browser mock, so nothing on
// the page is real — the banner says so and points at the install docs for the actual cockpit.
// Rendered to static markup in the tests: the test environment has no DOM.
import type { ReactElement } from "react";

export const INSTALL_DOCS = "https://colonizer.dev/docs/install";

export function DemoBanner(): ReactElement {
  return (
    <div role="note" className="flex shrink-0 flex-wrap items-center gap-x-2 gap-y-0.5 border-b border-warn bg-warn-soft px-4 py-2 text-[12.5px] text-warn">
      Demo: simulated colonies, nothing runs.
      <a href={INSTALL_DOCS} target="_blank" rel="noreferrer" className="text-accent hover:underline">
        Install Colonizer
      </a>
    </div>
  );
}
