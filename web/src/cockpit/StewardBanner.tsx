// The merge steward's org banner (issue #1172): when every job of an org's pull requests fails in a
// few seconds with no steps, or GitHub says billing or a spending limit, GitHub Actions is blocked
// there and no colony can fix it. /api/status `merge_steward.ci_blocked` says so, one line per org,
// above every view. It clears itself once the org's pull requests are no longer blocked. Absent on
// an older mothership. Rendered to static markup in the tests.
import type { ReactElement } from "react";

import type { MergeStewardStatus } from "../features/orgs/types";

export function StewardBanner({ steward }: { steward: MergeStewardStatus | null | undefined }): ReactElement | null {
  const blocked = steward?.ci_blocked ?? [];
  if (blocked.length === 0) return null;
  return (
    <div className="px-6 pt-4">
      <div className="flex flex-col gap-1.5">
        {blocked.map((b) => (
          <div
            key={b.org}
            role="status"
            className="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-md border border-warn bg-warn-soft px-3 py-2 text-sm text-warn"
          >
            <span>
              {b.message} {b.prs} pull {b.prs === 1 ? "request waits" : "requests wait"}; fix the billing or spending limit, then re-run the checks.
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
