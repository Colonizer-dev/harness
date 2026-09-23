// The cockpit's status bar (Cockpit Dashboards v3). Navigation and the workspace scope live in the
// sidebar (NavRail); this slim glass bar only says where you are and what is happening: the scope and
// view, the realtime ticker, the Live indicator, the live and need counts, spend, and the version.
import type { ReactElement } from "react";

import { formatCost } from "../spend";
import type { UpdateStatus } from "../types";
import type { LiveConnection } from "../liveStream";
import type { LiveEvent } from "./liveEvents";
import { LiveIndicator } from "./Live";

export type { CockpitView } from "./NavRail";

// Chip text and its tooltip are derived together so they can never disagree.
function versionChip(update: UpdateStatus): string {
  return update.available && update.latest
    ? `${update.installed.version} → ${update.latest.version}`
    : update.installed.version;
}

function updateTitle(update: UpdateStatus): string {
  if (update.available && update.latest) {
    return `update available: ${update.installed.version} → ${update.latest.version}`;
  }
  if (update.available) return `update available: ${update.installed.version}`;
  return `up to date · ${update.installed.version}`;
}

export function Header(props: {
  /** The workspace in scope; null is every workspace. */
  scope: string | null;
  /** The view's name, after the scope. */
  crumb: string;
  liveCount: number;
  needCount: number;
  /**
   * What this workspace's colonies have spent in total. Not a daily figure: the API reports a
   * running total per colony and no history, so there is nothing to slice a day out of.
   */
  cost: number | null;
  update: UpdateStatus | null;
  onOpenUpdates: () => void;
  /** The status poll is failing: the counts beside it are stale, and the header says so. */
  statusError: boolean;
  /** The realtime feed's connection; absent reads as reconnecting. */
  connection?: LiveConnection;
  /** The newest thing that changed, for the ticker; null says nothing. */
  latest?: LiveEvent | null;
}): ReactElement {
  const { scope, crumb, liveCount, needCount, cost, update, onOpenUpdates, statusError, connection, latest = null } = props;

  return (
    <header className="v3-glass sticky top-0 z-10 flex h-12 min-w-0 shrink-0 items-center gap-4 px-6 shadow-[inset_0_-1px_0_var(--border)]">
      <div className="flex min-w-0 shrink-0 items-center gap-2 text-[13px]">
        <span className="max-w-[220px] truncate text-muted">{scope ?? "All workspaces"}</span>
        <span aria-hidden="true" className="text-border-strong">/</span>
        <span className="text-text">{crumb}</span>
      </div>

      <div className="min-w-0 flex-1 overflow-hidden">
        {latest && (
          <span aria-live="polite" className="hidden min-w-0 overflow-hidden text-ellipsis whitespace-nowrap text-[13px] text-faint md:block">
            {/* Re-keyed on the event so every new one eases in. */}
            <span key={`${latest.id}-${latest.at}`} className="v3-evin">
              › {latest.text}
            </span>
          </span>
        )}
      </div>

      {statusError && (
        <span role="status" title="Mothership unreachable" className="inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap text-[13px] text-err">
          <span aria-hidden="true" className="h-1.5 w-1.5 rounded-full bg-err" />
          Mothership unreachable
        </span>
      )}
      <span className="shrink-0">
        <LiveIndicator connection={connection} />
      </span>
      <span className="hidden shrink-0 whitespace-nowrap text-[13px] tabular-nums text-muted sm:inline">
        {liveCount} live · <span className={needCount > 0 ? "text-warn" : "text-faint"}>{needCount} need you</span>
      </span>
      {cost !== null && (
        <span className="shrink-0 whitespace-nowrap font-mono text-[12.5px] tabular-nums text-muted" title="what this workspace's colonies have spent in total">
          {formatCost(cost)}
        </span>
      )}
      {update !== null && (
        <button
          type="button"
          title={updateTitle(update)}
          aria-label={`updates · ${updateTitle(update)}`}
          onClick={onOpenUpdates}
          className={`inline-flex shrink-0 cursor-pointer items-center gap-1.5 rounded-full border bg-transparent px-2.5 py-1 font-mono text-[11.5px] tabular-nums transition-colors hover:border-accent ${update.available ? "border-accent text-accent" : "border-border text-muted"}`}
        >
          {update.available && <span aria-hidden="true" className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />}
          {versionChip(update)}
        </button>
      )}
    </header>
  );
}
