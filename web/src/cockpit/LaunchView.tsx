// Launch: the frontier, and what you send out to settle it.
//
// The picker itself is the sidebar's launcher, unchanged. It already knows how to list repositories
// and issues, remember the last repository, select a batch, report a partial failure and queue past
// the parallel limit; a second copy laid out to the design would be a second set of those bugs.
import type { ReactElement } from "react";

import { NewSession } from "../components/Sidebar";
import type { Session } from "../types";

export function LaunchView({
  org,
  githubConnected,
  statusKnown,
  autopilotDefault,
  maxParallel,
  sessions,
  onOpenColony,
  onCreated,
  onOpenSettings,
}: {
  org: string | null;
  githubConnected: boolean;
  statusKnown: boolean;
  autopilotDefault: boolean;
  maxParallel: number | null;
  /** The mothership's colony list, for the launch form's pre-submit duplicate check. */
  sessions: Session[];
  /** Opens a colony holding an issue, from that issue's inline warning. */
  onOpenColony: (session: Session) => void;
  onCreated: (session: Session) => void;
  onOpenSettings: () => void;
}): ReactElement {
  return (
    <main className="cockpit min-h-0 overflow-y-auto px-6 pb-20 pt-10">
      <div className="mx-auto w-full max-w-[1080px]">
        <h1 className="m-0 text-[30px] font-semibold leading-[1.15] tracking-[-0.035em]">Launch</h1>
        <p className="mt-2 text-[14px] text-pretty text-muted">
          Send a settler out{org ? ` into ${org}` : ""}:
          one colony each, in its own microvm on a fresh worktree
          {maxParallel != null ? ` · queued past ${maxParallel} in parallel` : ""}.
        </p>
        <p className="mt-1 text-[12px] text-faint">
          Duplicate check is per-host; other motherships in the fleet are not consulted.
        </p>
        <div className="mt-8 border-y border-border">
          <NewSession
            org={org}
            githubConnected={githubConnected}
            statusKnown={statusKnown}
            autopilotDefault={autopilotDefault}
            sessions={sessions}
            onOpenColony={onOpenColony}
            onCreated={onCreated}
            onOpenSettings={onOpenSettings}
          />
        </div>
      </div>
    </main>
  );
}
