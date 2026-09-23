// The cockpit's top bar: nothing but the workspaces that have colonies running right now, as avatars
// at the right, each a filter. Navigation and the rest of the state live in the sidebar and the views;
// the bar only speaks up otherwise when something is wrong (the mothership unreachable, the live feed
// down).
import type { ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import { sameOrg } from "../components/ui";
import { toggledOrg, type OrgEntry } from "../orgs";
import type { LiveConnection } from "../liveStream";
import { needFor } from "./feed";

export type { CockpitView } from "./NavRail";

/** The workspaces with colonies running, busiest first — the avatars the bar shows. The chosen
 *  workspace stays in the row even when it goes quiet, so its filter can always be cleared. */
export function runningOrgs(orgs: readonly OrgEntry[], selectedOrg: string | null): OrgEntry[] {
  return orgs
    .filter((o) => o.live > 0 || sameOrg(o.org, selectedOrg))
    .sort((a, b) => b.live - a.live || a.org.localeCompare(b.org));
}

export function Header(props: {
  /** Workspaces only: orgEntries() has already dropped the undecided and the switched-off. */
  orgs: OrgEntry[];
  selectedOrg: string | null;
  /** null is every workspace; a second click on the chosen avatar clears the filter. */
  onSelectOrg: (org: string | null) => void;
  /** Keyed lowercase (needCountByOrg). */
  needByOrg: Record<string, number>;
  /** The status poll is failing: said aloud, since nothing else on the bar would show it. */
  statusError: boolean;
  /** The realtime feed's connection; only a dropped feed is shown. */
  connection?: LiveConnection;
}): ReactElement {
  const { orgs, selectedOrg, onSelectOrg, needByOrg, statusError, connection } = props;
  const running = runningOrgs(orgs, selectedOrg);

  return (
    <header className="v3-glass sticky top-0 z-10 flex h-12 min-w-0 shrink-0 items-center gap-3 px-6 shadow-[inset_0_-1px_0_var(--border)]">
      {statusError && (
        <span role="status" className="inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap text-[13px] text-err">
          <span aria-hidden="true" className="h-1.5 w-1.5 rounded-full bg-err" />
          Mothership unreachable
        </span>
      )}
      {!statusError && connection !== undefined && connection !== "open" && (
        <span role="status" title="the live feed dropped — polls cover until it reconnects" className="inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap text-[13px] text-faint">
          <span aria-hidden="true" className="h-1.5 w-1.5 rounded-full bg-faint" />
          reconnecting…
        </span>
      )}

      <div className="min-w-0 flex-1" />

      <div role="group" aria-label="running workspaces" className="flex min-w-0 items-center gap-1.5 overflow-x-auto py-1 [scrollbar-width:none]">
        {running.map((o) => {
          const active = sameOrg(o.org, selectedOrg);
          const need = needFor(needByOrg, o.org);
          const label = `${o.org} · ${o.live} running${need > 0 ? ` · ${need} need you` : ""}${active ? " · filtered, click to show all" : ""}`;
          return (
            <button
              key={o.org}
              type="button"
              title={label}
              aria-label={label}
              aria-pressed={active}
              onClick={() => onSelectOrg(toggledOrg(selectedOrg, o.org))}
              className={`relative grid h-8 w-8 shrink-0 cursor-pointer place-items-center rounded-full border-0 bg-transparent transition-[box-shadow,opacity] duration-150 ${
                active ? "shadow-[0_0_0_2px_var(--bg),0_0_0_3.5px_var(--accent)]" : selectedOrg ? "opacity-45 hover:opacity-100" : "hover:shadow-[0_0_0_2px_var(--bg),0_0_0_3.5px_var(--border-strong)]"
              }`}
            >
              <Avatar name={o.org} src={o.avatar ?? undefined} size={28} rounded="full" />
              {o.live > 0 && (
                <span
                  aria-hidden="true"
                  className="absolute -bottom-0.5 -right-0.5 grid h-4 min-w-4 place-items-center rounded-full border-2 border-bg bg-ok px-0.5 font-mono text-[9px] font-semibold leading-none text-bg tabular-nums"
                >
                  {o.live}
                </span>
              )}
              {need > 0 && <span aria-hidden="true" className="absolute -right-0.5 -top-0.5 h-2.5 w-2.5 rounded-full border-2 border-bg bg-warn" />}
            </button>
          );
        })}
      </div>
    </header>
  );
}
