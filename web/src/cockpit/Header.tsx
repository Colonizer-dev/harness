// The cockpit's shell (Cockpit Dashboards v3): one sticky glass header in two rows, replacing the
// old 56px icon rail. The top row is identity and state — the base mark, the org switcher, the
// workspace avatars, the realtime ticker, the Live indicator, spend, and the theme and workspace
// buttons. The second row is the view tabs, underlined where you are, each with the count it answers.
import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent, ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import { IconSettings } from "../components/icons";
import { sameOrg } from "../components/ui";
import { toggledOrg, type OrgEntry } from "../orgs";
import { formatCost } from "../spend";
import type { UpdateStatus } from "../types";
import type { LiveConnection } from "../liveStream";
import { needFor } from "./feed";
import type { LiveEvent } from "./liveEvents";
import { LiveIndicator } from "./Live";

export type CockpitView = "overview" | "home" | "colony" | "launch" | "inbox" | "history" | "settings" | "memory";

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

/** One tab in the nav row: the view it opens, its label, and the count beside it ("" says nothing). */
export interface NavTab {
  view: CockpitView;
  label: string;
  count: number | "";
  /** The count speaks up (warn) rather than sitting quiet (faint). */
  urgent?: boolean;
}

/** The nav row, left to right. The colony view has no tab: it is reached by opening a colony. */
export function navTabs({ needCount, liveCount, pendingMemory }: { needCount: number; liveCount: number; pendingMemory: number }): NavTab[] {
  return [
    { view: "overview", label: "Overview", count: "" },
    { view: "home", label: "Nest", count: liveCount || "" },
    { view: "inbox", label: "Inbox", count: needCount || "", urgent: needCount > 0 },
    { view: "history", label: "History", count: "" },
    { view: "launch", label: "Launch", count: "" },
    { view: "memory", label: "Memory", count: pendingMemory || "", urgent: pendingMemory > 0 },
    { view: "settings", label: "Settings", count: "" },
  ];
}

/** The round org mark every header surface uses: the org's avatar, else its initial. */
function OrgMark({ org, avatar, size = 20 }: { org: string; avatar?: string | null; size?: number }): ReactElement {
  return <Avatar name={org} src={avatar ?? undefined} size={size} rounded="full" />;
}

/** The "every workspace" mark: an asterisk on an inverted disc. */
function AllMark(): ReactElement {
  return (
    <span aria-hidden="true" className="grid h-5 w-5 shrink-0 place-items-center rounded-full bg-text font-mono text-[9px] font-medium text-bg">
      ∗
    </span>
  );
}

const ICON_BUTTON =
  "grid h-8 w-8 shrink-0 cursor-pointer place-items-center rounded-full border border-border bg-transparent text-muted transition-colors hover:border-border-strong hover:text-text";

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
  view: CockpitView;
  onNavigate: (view: CockpitView) => void;
  /** Cross-workspace: the inbox tab's count. */
  inboxCount: number;
  /** Memory proposals waiting for review, narrowed to the chosen org (memoryBadge). */
  pendingMemory: number;
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
  theme?: "light" | "dark" | null;
  onToggleTheme?: () => void;
}): ReactElement {
  const {
    orgs,
    hiddenOrgs,
    selectedOrg,
    onSelectOrg,
    onOpenOrgSettings,
    needByOrg,
    view,
    onNavigate,
    inboxCount,
    pendingMemory,
    liveCount,
    needCount,
    cost,
    update,
    onOpenUpdates,
    statusError,
    connection,
    latest = null,
    theme = null,
    onToggleTheme,
  } = props;
  const [menuOpen, setMenuOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);

  const current = orgs.find((o) => sameOrg(o.org, selectedOrg)) ?? null;
  const orgName = current ? current.org : "All workspaces";
  // The workspaces button opens the chosen org's settings, else the first workspace's: that
  // dialog carries the switch-off toggle and "hide orgs with no colonies".
  const settingsOrg = current?.org ?? orgs[0]?.org ?? hiddenOrgs[0]?.org ?? null;

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

  const tabs = navTabs({ needCount: inboxCount, liveCount, pendingMemory });

  return (
    <header className="v3-glass sticky top-0 z-20 shrink-0">
      <div className="flex h-14 items-center gap-3 px-6">
        <button
          type="button"
          aria-label="overview · every colony"
          title="overview"
          onClick={() => onNavigate("overview")}
          className="h-[22px] w-[22px] shrink-0 cursor-pointer rounded-full border-0 bg-text p-0"
        />
        <span aria-hidden="true" className="text-[20px] font-light text-border-strong">/</span>

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
            className="flex cursor-pointer items-center gap-2 rounded-md border-0 bg-transparent px-2 py-[5px] text-sm font-medium text-text transition-colors hover:bg-panel-2"
          >
            {current ? <OrgMark org={current.org} avatar={current.avatar} /> : <AllMark />}
            <span className="whitespace-nowrap">{orgName}</span>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true" className="text-faint">
              <path d="m8 9 4-4 4 4M8 15l4 4 4-4" />
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
                className="v3-pop scroll-thin absolute left-0 top-full z-40 mt-1.5 max-h-[min(480px,calc(100vh-80px))] w-[280px] animate-[ck-in_180ms_ease-out_both] overflow-y-auto rounded-xl border border-border-strong p-1.5 shadow-[0_12px_40px_rgb(0_0_0/0.35)]"
              >
                <OrgRow label="All workspaces" note={String(orgs.length)} urgent={false} active={selectedOrg === null} icon={<AllMark />} onClick={() => pickOrg(null)} />
                {orgs.map((o) => {
                  const need = needFor(needByOrg, o.org);
                  return (
                    <OrgRow
                      key={o.org}
                      label={o.org}
                      note={need > 0 ? `${need} need you` : ""}
                      urgent={need > 0}
                      active={sameOrg(o.org, selectedOrg)}
                      icon={<OrgMark org={o.org} avatar={o.avatar} />}
                      // A second click on the chosen org clears the filter.
                      onClick={() => pickOrg(toggledOrg(selectedOrg, o.org))}
                    />
                  );
                })}
                {hiddenOrgs.length > 0 && (
                  // Switched off in their settings, so not a choice; this is the way back to switching one on.
                  <div role="group" aria-label="switched off" className="mt-1 border-t border-border pt-1">
                    <div aria-hidden="true" className="px-2 pb-0.5 pt-1 text-[11.5px] text-faint">Switched off · open settings to turn on</div>
                    {hiddenOrgs.map((o) => (
                      <button
                        key={o.org}
                        type="button"
                        role="menuitem"
                        tabIndex={-1}
                        aria-label={`settings for ${o.org} (switched off)`}
                        onClick={() => openSettingsFor(o.org)}
                        className="flex w-full cursor-pointer items-center gap-2.5 rounded-md border-0 bg-transparent px-2 py-1.5 text-left transition-colors hover:bg-panel-2 focus-visible:bg-panel-2 focus-visible:outline-none"
                      >
                        <OrgMark org={o.org} avatar={o.avatar} />
                        <span className="min-w-0 flex-1 truncate text-[13px] text-muted">{o.org}</span>
                        <IconSettings size={14} className="shrink-0 text-faint" />
                      </button>
                    ))}
                  </div>
                )}
                {settingsOrg && (
                  <>
                    <div role="separator" className="my-1.5 h-px bg-border" />
                    <button
                      type="button"
                      role="menuitem"
                      tabIndex={-1}
                      onClick={() => openSettingsFor(settingsOrg)}
                      className="w-full cursor-pointer rounded-md border-0 bg-transparent p-2 text-left text-[13px] text-muted transition-colors hover:bg-panel-2 hover:text-text focus-visible:bg-panel-2 focus-visible:outline-none"
                    >
                      {current ? `${current.org} settings…` : hiddenOrgs.length > 0 ? `${hiddenOrgs.length} hidden · manage workspaces` : "manage workspaces"}
                    </button>
                  </>
                )}
              </div>
            </>
          )}
        </div>

        {orgs.length > 1 && (
          // The workspaces the rail used to list, as a row of avatars beside the switcher.
          <div aria-label="workspaces" role="group" className="hidden items-center gap-1 pl-1 md:flex">
            {orgs.map((o) => {
              const need = needFor(needByOrg, o.org);
              const active = sameOrg(o.org, selectedOrg);
              // A second click on the chosen org is the quick way back to every workspace, so say so.
              const name = active ? `${o.org} · selected, click for all workspaces` : o.org;
              const label = need > 0 ? `${name} · ${need} need you` : name;
              return (
                <button
                  key={o.org}
                  type="button"
                  title={label}
                  aria-label={label}
                  aria-pressed={active}
                  onClick={() => onSelectOrg(toggledOrg(selectedOrg, o.org))}
                  className={`relative grid h-7 w-7 cursor-pointer place-items-center rounded-full border-0 bg-transparent transition-colors hover:bg-panel-2 ${active ? "shadow-[0_0_0_1.5px_var(--text)]" : ""}`}
                >
                  <OrgMark org={o.org} avatar={o.avatar} />
                  {need > 0 && <span aria-hidden="true" className="absolute right-0.5 top-0.5 h-[7px] w-[7px] rounded-full border-2 border-bg bg-warn" />}
                </button>
              );
            })}
          </div>
        )}

        <div className="flex-1" />

        {latest && (
          <span aria-live="polite" className="hidden min-w-0 max-w-[300px] overflow-hidden text-ellipsis whitespace-nowrap text-[13px] text-faint lg:inline">
            {/* Re-keyed on the event so every new one eases in. */}
            <span key={`${latest.id}-${latest.at}`} className="v3-evin">
              › {latest.text}
            </span>
          </span>
        )}
        {statusError && (
          <span role="status" title="Mothership unreachable" className="inline-flex items-center gap-1.5 whitespace-nowrap text-[13px] text-err">
            <span aria-hidden="true" className="h-1.5 w-1.5 rounded-full bg-err" />
            Mothership unreachable
          </span>
        )}
        <LiveIndicator connection={connection} />
        <span className="hidden whitespace-nowrap text-[13px] tabular-nums text-muted sm:inline">
          {liveCount} live · <span className={needCount > 0 ? "text-warn" : "text-faint"}>{needCount} need you</span>
        </span>
        {cost !== null && (
          <span className="whitespace-nowrap font-mono text-[12.5px] tabular-nums text-muted" title="what this workspace's colonies have spent in total">
            {formatCost(cost)}
          </span>
        )}
        {update !== null && (
          <button
            type="button"
            title={updateTitle(update)}
            aria-label={`updates · ${updateTitle(update)}`}
            onClick={onOpenUpdates}
            className={`inline-flex cursor-pointer items-center gap-1.5 rounded-full border bg-transparent px-2.5 py-1 font-mono text-[11.5px] tabular-nums transition-colors hover:border-accent ${update.available ? "border-accent text-accent" : "border-border text-muted"}`}
          >
            {update.available && <span aria-hidden="true" className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />}
            {versionChip(update)}
          </button>
        )}
        {onToggleTheme && (
          <button
            type="button"
            aria-label="toggle theme"
            title={theme === "dark" ? "switch to light" : "switch to dark"}
            aria-pressed={theme === "dark"}
            onClick={onToggleTheme}
            className={ICON_BUTTON}
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" aria-hidden="true">
              <circle cx="12" cy="12" r="4" />
              <path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4 7 17M17 7l1.4-1.4" />
            </svg>
          </button>
        )}
        {settingsOrg && (
          <button type="button" aria-label="workspaces" title="workspaces" onClick={() => onOpenOrgSettings(settingsOrg)} className={ICON_BUTTON}>
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" aria-hidden="true">
              <path d="M4 7h9M17 7h3M4 17h3M11 17h9M15 5v4M9 15v4" />
            </svg>
          </button>
        )}
      </div>

      <nav aria-label="cockpit views" className="flex gap-1 overflow-x-auto overflow-y-hidden px-4 shadow-[inset_0_-1px_0_var(--border)] [scrollbar-width:none]">
        {tabs.map((tab) => {
          const on = tab.view === view;
          const label = tab.count !== "" ? `${tab.label} · ${tab.count}` : tab.label;
          return (
            <button
              key={tab.view}
              type="button"
              aria-label={label}
              aria-current={on ? "page" : undefined}
              onClick={() => onNavigate(tab.view)}
              className={`flex cursor-pointer items-center gap-2 whitespace-nowrap border-0 border-b-2 border-solid bg-transparent px-2.5 pb-[11px] pt-3 text-[13.5px] transition-colors hover:text-text ${on ? "border-text text-text" : "border-transparent text-muted"}`}
            >
              {tab.label}
              {tab.count !== "" && <span className={`text-[12px] tabular-nums ${tab.urgent ? "text-warn" : "text-faint"}`}>{tab.count}</span>}
            </button>
          );
        })}
      </nav>
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
  note,
  urgent,
  active,
  icon,
  onClick,
}: {
  label: string;
  note: string;
  urgent: boolean;
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
      className={`grid w-full cursor-pointer grid-cols-[20px_minmax(0,1fr)_auto] items-center gap-2.5 rounded-md border-0 p-2 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${active ? "bg-panel-2" : "bg-transparent hover:bg-panel-2 focus-visible:bg-panel-2"}`}
    >
      {icon}
      <span className="truncate text-[13.5px] text-text">{label}</span>
      <span className={`text-[12px] tabular-nums ${urgent ? "text-warn" : "text-faint"}`}>{note}</span>
    </button>
  );
}
