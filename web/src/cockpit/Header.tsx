import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent, ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import { IconOrg, IconSettings } from "../components/icons";
import { sameOrg } from "../components/ui";
import { toggledOrg, type OrgEntry } from "../orgs";
import { formatCost } from "../spend";
import type { UpdateStatus } from "../types";
import { needFor } from "./feed";

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
  /** Workspaces only: orgEntries() has already dropped the undecided and the switched-off. */
  orgs: OrgEntry[];
  /** Switched-off orgs: not a choice, but listed so their settings (and the switch back on) stay reachable. */
  hiddenOrgs: OrgEntry[];
  selectedOrg: string | null;
  /** null is "every workspace". */
  onSelectOrg: (org: string | null) => void;
  onOpenOrgSettings: (org: string) => void;
  /** Colonies of that org waiting on an answer — not the same thing as its pending memory notes. Keyed lowercase. */
  needByOrg: Record<string, number>;
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
}): ReactElement {
  const { orgs, hiddenOrgs, selectedOrg, onSelectOrg, onOpenOrgSettings, needByOrg, crumb, liveCount, needCount, cost, update, onOpenUpdates } =
    props;
  const [menuOpen, setMenuOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);

  const current = orgs.find((o) => sameOrg(o.org, selectedOrg)) ?? null;
  // With no org chosen the header speaks for the base itself.
  const orgName = current ? current.org : "colonizer";

  // Focus goes into the menu when it opens, onto the checked row, and back to the trigger when it
  // closes — but only if it was inside the menu, so a click elsewhere keeps the focus it moved.
  useEffect(() => {
    if (!menuOpen) return;
    const items = menuItems(menu.current);
    (items.find((item) => item.getAttribute("aria-checked") === "true") ?? items[0])?.focus();
  }, [menuOpen]);

  const close = (refocus: boolean) => {
    setMenuOpen(false);
    if (refocus) trigger.current?.focus();
  };

  const pickOrg = (org: string | null) => {
    onSelectOrg(org);
    close(true);
  };

  const openSettingsFor = (org: string) => {
    close(false);
    onOpenOrgSettings(org);
  };

  // Escape closes; the arrows, Home and End move between the rows, wrapping at the ends.
  const onMenuKey = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape" || event.key === "Tab") {
      if (event.key === "Escape") event.preventDefault();
      close(event.key === "Escape");
      return;
    }
    const items = menuItems(menu.current);
    if (items.length === 0) return;
    const at = items.indexOf(document.activeElement as HTMLElement);
    const next =
      event.key === "ArrowDown" ? (at + 1) % items.length
      : event.key === "ArrowUp" ? (at <= 0 ? items.length - 1 : at - 1)
      : event.key === "Home" ? 0
      : event.key === "End" ? items.length - 1
      : null;
    if (next === null) return;
    event.preventDefault();
    items[next].focus();
  };

  return (
    <header className="flex h-12 shrink-0 items-center gap-3.5 border-b border-border px-5">
      <div className="relative">
        <button
          ref={trigger}
          type="button"
          title="switch organisation"
          aria-label="switch organisation"
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          onClick={() => setMenuOpen((open) => !open)}
          onKeyDown={(event) => {
            // The menu-button pattern: the down arrow opens the menu as well as Enter and Space do.
            if (event.key === "ArrowDown" && !menuOpen) {
              event.preventDefault();
              setMenuOpen(true);
            }
          }}
          className="-ml-2 flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm font-semibold text-text transition-colors hover:bg-panel-2"
        >
          <Avatar name={orgName} src={current?.avatar} size={18} rounded="md" />
          {orgName}
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" className="text-faint">
            <path d="m6 9 6 6 6-6" />
          </svg>
        </button>

        {menuOpen && (
          <>
            {/* Click-catcher beneath the menu: any click outside the options lands here and closes it. */}
            <div aria-hidden="true" className="fixed inset-0 z-30" onClick={() => close(false)} />
            <div
              ref={menu}
              role="menu"
              aria-label="organisations"
              onKeyDown={onMenuKey}
              className="scroll-thin absolute left-0 top-full z-40 mt-2 max-h-[min(480px,calc(100vh-80px))] w-[280px] animate-[ck-in_180ms_ease-out_both] overflow-y-auto rounded-[14px] border border-border bg-panel p-1.5 shadow-xl"
            >
              <OrgRow
                label="All workspaces"
                meta={`${orgs.length} ${orgs.length === 1 ? "workspace" : "workspaces"}`}
                need={0}
                active={selectedOrg === null}
                icon={
                  <span className="grid h-7 w-7 place-items-center rounded-md bg-panel-3 text-muted">
                    <IconOrg size={15} />
                  </span>
                }
                onClick={() => pickOrg(null)}
              />
              {orgs.length > 0 && <div role="separator" className="my-1 border-t border-border" />}
              {orgs.map((o) => {
                const active = sameOrg(o.org, selectedOrg);
                return (
                  <OrgRow
                    key={o.org}
                    label={o.org}
                    meta={`${o.total} colonies · ${o.live} live`}
                    need={needFor(needByOrg, o.org)}
                    active={active}
                    icon={<Avatar name={o.org} src={o.avatar} size={28} rounded="md" />}
                    // A second click on the chosen org clears the filter, as on the rail.
                    onClick={() => pickOrg(toggledOrg(selectedOrg, o.org))}
                  />
                );
              })}
              {current && (
                <>
                  <div role="separator" className="my-1 border-t border-border" />
                  <button
                    type="button"
                    role="menuitem"
                    tabIndex={-1}
                    onClick={() => openSettingsFor(current.org)}
                    className="flex w-full items-center gap-2.5 rounded-[10px] px-2.5 py-2 text-left text-[13px] text-muted transition-colors hover:bg-panel-2 hover:text-text focus-visible:bg-panel-2 focus-visible:outline-none"
                  >
                    <IconSettings size={15} />
                    {current.org} settings…
                  </button>
                </>
              )}
              {hiddenOrgs.length > 0 && (
                // Switched off in their settings, so not a choice; this is the way back to switching one on.
                <div role="group" aria-label="switched off" className="mt-1 border-t border-border pt-1">
                  <div aria-hidden="true" className="px-2.5 pb-0.5 pt-1 text-[11px] text-faint">Switched off · open settings to turn on</div>
                  {hiddenOrgs.map((o) => (
                    <button
                      key={o.org}
                      type="button"
                      role="menuitem"
                      tabIndex={-1}
                      aria-label={`settings for ${o.org} (switched off)`}
                      onClick={() => openSettingsFor(o.org)}
                      className="flex w-full items-center gap-2.5 rounded-[10px] px-2.5 py-1.5 text-left transition-colors hover:bg-panel-2 focus-visible:bg-panel-2 focus-visible:outline-none"
                    >
                      <Avatar name={o.org} src={o.avatar} size={20} rounded="md" />
                      <span className="min-w-0 flex-1 truncate text-[13px] text-muted">{o.org}</span>
                      <IconSettings size={14} className="shrink-0 text-faint" />
                    </button>
                  ))}
                </div>
              )}
            </div>
          </>
        )}
      </div>

      <span aria-hidden="true" className="text-faint">/</span>
      <span className="text-[13px] text-muted">{crumb}</span>

      <div className="flex-1" />

      <div className="flex items-center gap-3.5 font-mono text-xs text-muted">
        <span className="inline-flex items-center gap-1.5 tabular-nums">
          <span aria-hidden="true" className="h-1.5 w-1.5 rounded-full bg-ok" />
          {liveCount} live
        </span>
        <span className={`inline-flex items-center gap-1.5 tabular-nums ${needCount > 0 ? "text-warn" : "text-faint"}`}>
          <span aria-hidden="true" className={`h-1.5 w-1.5 rounded-full ${needCount > 0 ? "bg-warn" : "bg-faint"}`} />
          {needCount} need you
        </span>
        {cost !== null && (
          <span className="tabular-nums" title="what this workspace's colonies have spent in total">
            {formatCost(cost)} spent
          </span>
        )}
        {update !== null && (
          <button
            type="button"
            title={updateTitle(update)}
            aria-label={`updates · ${updateTitle(update)}`}
            onClick={onOpenUpdates}
            className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[11.5px] tabular-nums transition-colors hover:border-accent ${update.available ? "border-accent text-accent" : "border-border text-muted"}`}
          >
            {update.available && (
              <span aria-hidden="true" className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
            )}
            {versionChip(update)}
          </button>
        )}
      </div>
    </header>
  );
}

/** The menu's focusable rows, top to bottom. */
function menuItems(menu: HTMLElement | null): HTMLElement[] {
  return menu ? [...menu.querySelectorAll<HTMLElement>('[role="menuitem"], [role="menuitemradio"]')] : [];
}

/** One choice in the switcher: every workspace, or one org. */
function OrgRow({
  label,
  meta,
  need,
  active,
  icon,
  onClick,
}: {
  label: string;
  meta: string;
  need: number;
  active: boolean;
  icon: ReactElement;
  onClick: () => void;
}): ReactElement {
  return (
    <button
      type="button"
      role="menuitemradio"
      aria-checked={active}
      // Roving focus: the arrows move between rows, so Tab leaves the menu instead of walking it.
      tabIndex={-1}
      onClick={onClick}
      className={`grid w-full grid-cols-[28px_minmax(0,1fr)_auto] items-center gap-2.5 rounded-[10px] px-2.5 py-2 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${active ? "bg-accent-soft" : "hover:bg-panel-2 focus-visible:bg-panel-2"}`}
    >
      {icon}
      <span className="min-w-0">
        <span className="block truncate text-[13.5px] font-semibold text-text">{label}</span>
        <span className="block font-mono text-[11px] text-faint">{meta}</span>
      </span>
      {/* Silent when nothing waits: the row should only speak up when it has something to ask for. */}
      <span className="font-mono text-[11px] tabular-nums text-warn">{need > 0 ? `${need} need you` : ""}</span>
    </button>
  );
}
