// The cockpit's top bar: the workspaces that have colonies running right now, as avatars at the
// right, each a filter, then the notifications bell (the inbox) at the far right. (Issues are handed
// off from Colonize: the sidebar's button, the dashboard's, or ⌘K.) Navigation and the rest of the state live in the sidebar and the views;
// the bar only speaks up otherwise when something is wrong (the mothership unreachable, the live feed
// down).
import { useEffect, useRef, useState, type ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import { sameOrg } from "../components/ui";
import type { OrgEntry } from "../orgs";
import type { LiveConnection } from "../liveStream";
import { NotificationsBell, type InboxActions } from "./NotificationsBell";

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
  /** The inbox behind the bell; absent, the bar has no bell (static tests). */
  inbox?: InboxActions;
  /** The signed-in GitHub user, shown as the one avatar at the right with its menu. */
  user?: UserMenuProps;
}): ReactElement {
  const { statusError, connection, inbox, user } = props;

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

      {inbox && <NotificationsBell {...inbox} />}
      {user && <UserMenu {...user} />}
    </header>
  );
}

export interface UserMenuProps {
  /** GitHub login; null when GitHub is not connected. */
  login: string | null;
  name?: string | null;
  avatarUrl?: string | null;
  onOpenSettings: () => void;
  onOpenSecrets: () => void;
}

/** The one avatar at the right of the bar: the signed-in GitHub user, with a small menu. */
export function UserMenu({ login, name, avatarUrl, onOpenSettings, onOpenSecrets }: UserMenuProps): ReactElement {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent | KeyboardEvent) => {
      if (e instanceof KeyboardEvent ? e.key === "Escape" : !root.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", close);
    };
  }, [open]);
  const label = login ? `${name || login} (@${login})` : "GitHub not connected";
  const item = "flex w-full cursor-pointer items-center gap-2 rounded-md border-0 bg-transparent px-2.5 py-1.5 text-left text-[13px] text-text hover:bg-panel-2";
  return (
    <div ref={root} className="relative shrink-0">
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={`account · ${label}`}
        title={label}
        onClick={() => setOpen((o) => !o)}
        className="grid h-8 w-8 cursor-pointer place-items-center rounded-full border-0 bg-transparent p-0 transition-shadow hover:shadow-[0_0_0_2px_var(--bg),0_0_0_3.5px_var(--border-strong)]"
      >
        <Avatar name={login ?? "?"} src={avatarUrl ?? (login ? `https://github.com/${login}.png?size=64` : undefined)} size={28} rounded="full" />
      </button>
      {open && (
        <div role="menu" className="absolute right-0 top-10 z-50 w-56 rounded-xl border border-border-strong bg-panel p-1.5 shadow-[0_16px_48px_rgb(0_0_0/0.35)]">
          <div className="border-b border-border px-2.5 pb-2 pt-1">
            <div className="truncate text-[13px] font-medium text-text">{name || login || "Not signed in"}</div>
            {login && <div className="truncate text-[12px] text-faint">@{login} · GitHub</div>}
          </div>
          <div className="pt-1">
            <button type="button" role="menuitem" className={item} onClick={() => (setOpen(false), onOpenSettings())}>
              Settings
            </button>
            <button type="button" role="menuitem" className={item} onClick={() => (setOpen(false), onOpenSecrets())}>
              Secrets
            </button>
            {login && (
              <a role="menuitem" className={item + " no-underline"} href={`https://github.com/${login}`} target="_blank" rel="noreferrer">
                GitHub profile ↗
              </a>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
