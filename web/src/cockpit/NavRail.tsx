// The cockpit's sidebar: a slim, quiet column of icons that opens into labels. Collapsed it is 64px
// of clear glyphs, each naming itself on hover or focus; expanded (the choice is remembered) every
// item carries its label and count. One accent does the talking: the launch button, and the bar
// beside wherever you are. The workspace switcher sits at the top and is the cockpit's scope: every
// view reads it, and Overview shows the chosen org's dashboard, or every workspace when none is.
import { useEffect, useRef, useState, type KeyboardEvent, type ReactElement, type ReactNode } from "react";

import { Avatar } from "../components/Avatar";
import { sameOrg, store, stored } from "../components/ui";
import type { OrgEntry } from "../orgs";
import { needFor } from "./feed";

export type CockpitView = "overview" | "home" | "colony" | "launch" | "inbox" | "history" | "settings" | "memory";

const EXPANDED_KEY = "colonizer.sidebarExpanded";

/** One view in the sidebar: the view it opens, its label, and the count beside it ("" says nothing). */
export interface NavTab {
  view: CockpitView;
  label: string;
  count: number | "";
  /** The count speaks up (warn) rather than sitting quiet. */
  urgent?: boolean;
}

/** The views, top to bottom. Launch has its own button and settings sits in the foot; the colony
 *  view has no item — it is reached by opening a colony. */
export function navTabs({ needCount, liveCount, pendingMemory }: { needCount: number; liveCount: number; pendingMemory: number }): NavTab[] {
  return [
    { view: "overview", label: "Overview", count: "" },
    { view: "home", label: "Nest", count: liveCount || "" },
    { view: "inbox", label: "Inbox", count: needCount || "", urgent: needCount > 0 },
    { view: "history", label: "History", count: "" },
    { view: "memory", label: "Memory", count: pendingMemory || "", urgent: pendingMemory > 0 },
  ];
}

// 24px, 1.6 stroke, round joins: one family, legible at 20px.
const GLYPH: Record<string, ReactNode> = {
  overview: (
    <>
      <rect x="3.5" y="3.5" width="7" height="7" rx="2" />
      <rect x="13.5" y="3.5" width="7" height="7" rx="2" />
      <rect x="3.5" y="13.5" width="7" height="7" rx="2" />
      <rect x="13.5" y="13.5" width="7" height="7" rx="2" />
    </>
  ),
  home: (
    <>
      <path d="M12 3 19.8 7.5v9L12 21l-7.8-4.5v-9z" />
      <circle cx="12" cy="12" r="2.4" />
    </>
  ),
  inbox: (
    <>
      <path d="M4 13.5 6.4 5.8A1.5 1.5 0 0 1 7.8 4.8h8.4a1.5 1.5 0 0 1 1.4 1l2.4 7.7" />
      <path d="M4 13.5V18a1.5 1.5 0 0 0 1.5 1.5h13A1.5 1.5 0 0 0 20 18v-4.5h-4.5l-1 2.5h-5l-1-2.5z" />
    </>
  ),
  history: (
    <>
      <path d="M3.5 12a8.5 8.5 0 1 0 2.6-6.1" />
      <path d="M3.5 4.5v4h4" />
      <path d="M12 7.5V12l3 2" />
    </>
  ),
  memory: (
    <>
      <path d="m12 3.5 8.5 4.5-8.5 4.5L3.5 8z" />
      <path d="m3.5 12 8.5 4.5 8.5-4.5" />
      <path d="m3.5 16 8.5 4.5 8.5-4.5" />
    </>
  ),
  settings: (
    <>
      <path d="M4 7h9M17 7h3M4 17h3M11 17h9" />
      <circle cx="15" cy="7" r="2" />
      <circle cx="9" cy="17" r="2" />
    </>
  ),
  sun: (
    <>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.3 5.3l1.4 1.4M17.3 17.3l1.4 1.4M5.3 18.7l1.4-1.4M17.3 6.7l1.4-1.4" />
    </>
  ),
  moon: <path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z" />,
  panel: (
    <>
      <rect x="3.5" y="4.5" width="17" height="15" rx="3" />
      <path d="M9.5 4.5v15" />
    </>
  ),
  plus: <path d="M12 5v14M5 12h14" />,
  all: (
    <>
      <circle cx="8" cy="8" r="3" />
      <circle cx="16" cy="8" r="3" />
      <circle cx="8" cy="16" r="3" />
      <circle cx="16" cy="16" r="3" />
    </>
  ),
};

function Glyph({ name, size = 20 }: { name: string; size?: number }): ReactElement {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" className="shrink-0">
      {GLYPH[name]}
    </svg>
  );
}

/**
 * A name beside a collapsed item, on hover or keyboard focus. Placed with `position: fixed` from
 * the item's own rectangle so the scrolling workspace list cannot clip it.
 */
function Tip({ label, show, children }: { label: string; show: boolean; children: ReactNode }): ReactElement {
  const [at, setAt] = useState<{ left: number; top: number } | null>(null);
  const open = (event: { currentTarget: HTMLElement }) => {
    if (!show) return;
    const box = event.currentTarget.getBoundingClientRect();
    setAt({ left: box.right + 10, top: box.top + box.height / 2 });
  };
  const close = () => setAt(null);
  return (
    <div className="relative" onMouseEnter={open} onMouseLeave={close} onFocus={open} onBlur={close}>
      {children}
      {show && (
        <span
          role="tooltip"
          style={at ? { left: at.left, top: at.top } : undefined}
          className={`v3-pop pointer-events-none fixed z-50 -translate-y-1/2 whitespace-nowrap rounded-md border border-border-strong px-2 py-1 text-[12.5px] text-text shadow-[0_8px_24px_rgb(0_0_0/0.3)] transition-opacity duration-100 ${at ? "opacity-100" : "left-0 top-0 opacity-0"}`}
        >
          {label}
        </span>
      )}
    </div>
  );
}

/** A count: a bubble on the icon when collapsed, a right-aligned figure when expanded. */
function Count({ value, urgent, expanded }: { value: number | ""; urgent?: boolean; expanded: boolean }): ReactElement | null {
  if (value === "") return null;
  if (expanded) {
    return <span className={`ml-auto text-[12px] tabular-nums ${urgent ? "text-warn" : "text-faint"}`}>{value}</span>;
  }
  return (
    <span
      aria-hidden="true"
      className={`absolute -right-0.5 -top-0.5 grid h-4 min-w-4 place-items-center rounded-full px-1 font-mono text-[9.5px] font-semibold tabular-nums ${urgent ? "bg-warn text-bg" : "bg-panel-3 text-muted"}`}
    >
      {value}
    </span>
  );
}

const ITEM =
  "group relative flex h-10 w-full cursor-pointer items-center gap-3 rounded-[10px] border-0 bg-transparent text-[13.5px] transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent";

export function NavRail(props: {
  /** Workspaces only: orgEntries() has already dropped the undecided and the switched-off. */
  orgs: OrgEntry[];
  /** Switched-off orgs: not a choice, but listed so their settings (and the switch back on) stay reachable. */
  hiddenOrgs: OrgEntry[];
  onOpenOrgSettings: (org: string) => void;
  selectedOrg: string | null;
  /** null is "every workspace". */
  onSelectOrg: (org: string | null) => void;
  /** Keyed lowercase (needCountByOrg); read through needFor. */
  needByOrg: Record<string, number>;
  view: CockpitView;
  onNavigate: (view: CockpitView) => void;
  /** Cross-workspace: the inbox's count. */
  inboxCount: number;
  liveCount: number;
  /** Memory proposals waiting for review (memoryBadge). */
  pendingMemory: number;
  theme: "light" | "dark" | null;
  onToggleTheme: () => void;
  /** Start expanded regardless of the remembered choice; the tests pin both states through it. */
  initialExpanded?: boolean;
}): ReactElement {
  const { orgs, hiddenOrgs, onOpenOrgSettings, selectedOrg, onSelectOrg, needByOrg, view, onNavigate, inboxCount, liveCount, pendingMemory, theme, onToggleTheme } = props;
  const [expanded, setExpanded] = useState<boolean>(() => props.initialExpanded ?? stored(EXPANDED_KEY) !== "0");
  const toggle = () =>
    setExpanded((open) => {
      store(EXPANDED_KEY, open ? "0" : "1");
      return !open;
    });

  const pad = expanded ? "px-3" : "justify-center px-0";
  const tabs = navTabs({ needCount: inboxCount, liveCount, pendingMemory });
  const tip = !expanded;

  return (
    <nav
      aria-label="cockpit"
      data-expanded={expanded}
      className={`v3-rail relative z-20 flex h-full min-h-0 shrink-0 flex-col gap-1 border-r border-border px-3 py-4 transition-[width] duration-200 ease-out ${expanded ? "w-[232px]" : "w-16"}`}
    >
      {/* Brand: the outpost mark, and the name once there is room for it. */}
      <button
        type="button"
        aria-label="overview · every colony"
        onClick={() => onNavigate("overview")}
        className={`mb-3 flex h-10 cursor-pointer items-center gap-2.5 rounded-[10px] border-0 bg-transparent ${expanded ? "px-1.5" : "justify-center"}`}
      >
        <span className="v3-brand grid h-8 w-8 shrink-0 place-items-center rounded-[10px] text-accent">
          <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M12 2.8 20 7.4v9.2L12 21.2 4 16.6V7.4z" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round" />
            <circle cx="12" cy="12" r="2.6" fill="currentColor" />
          </svg>
        </span>
        {expanded && <span className="text-[15px] font-semibold tracking-[-0.02em] text-text">Colonizer</span>}
      </button>

      <ScopeSwitcher
        orgs={orgs}
        hiddenOrgs={hiddenOrgs}
        selectedOrg={selectedOrg}
        onSelectOrg={onSelectOrg}
        onOpenOrgSettings={onOpenOrgSettings}
        needByOrg={needByOrg}
        expanded={expanded}
      />

      <Tip label="Launch a colony" show={tip}>
        <button
          type="button"
          aria-label="launch a colony"
          aria-current={view === "launch" ? "page" : undefined}
          onClick={() => onNavigate("launch")}
          className={`v3-launch mb-3 flex h-10 w-full cursor-pointer items-center gap-2.5 rounded-[10px] border-0 font-medium text-[13.5px] transition-[filter,transform] duration-150 hover:brightness-110 active:scale-[0.98] ${pad}`}
        >
          <Glyph name="plus" size={18} />
          {expanded && "New colony"}
        </button>
      </Tip>

      {tabs.map((tab) => {
        const on = tab.view === view;
        const label = tab.count !== "" ? `${tab.label} · ${tab.count}` : tab.label;
        return (
          <Tip key={tab.view} label={label} show={tip}>
            <button
              type="button"
              aria-label={label}
              aria-current={on ? "page" : undefined}
              onClick={() => onNavigate(tab.view)}
              className={`${ITEM} ${pad} ${on ? "bg-panel-2 text-text" : "text-muted hover:bg-panel-2 hover:text-text"}`}
            >
              {/* Where you are: a short accent bar on the rail's inner edge. */}
              <span aria-hidden="true" className={`absolute -left-3 top-1/2 h-5 w-[3px] -translate-y-1/2 rounded-r-full bg-accent transition-opacity ${on ? "opacity-100" : "opacity-0"}`} />
              <span className="relative grid place-items-center">
                <Glyph name={tab.view} />
                {!expanded && <Count value={tab.count} urgent={tab.urgent} expanded={false} />}
              </span>
              {expanded && <span className="truncate">{tab.label}</span>}
              {expanded && <Count value={tab.count} urgent={tab.urgent} expanded />}
            </button>
          </Tip>
        );
      })}

      <div className="min-h-2 flex-1" />

      <Tip label="Settings" show={tip}>
        <button
          type="button"
          aria-label="settings"
          aria-current={view === "settings" ? "page" : undefined}
          onClick={() => onNavigate("settings")}
          className={`${ITEM} ${pad} ${view === "settings" ? "bg-panel-2 text-text" : "text-muted hover:bg-panel-2 hover:text-text"}`}
        >
          <span aria-hidden="true" className={`absolute -left-3 top-1/2 h-5 w-[3px] -translate-y-1/2 rounded-r-full bg-accent transition-opacity ${view === "settings" ? "opacity-100" : "opacity-0"}`} />
          <Glyph name="settings" />
          {expanded && "Settings"}
        </button>
      </Tip>
      <Tip label={theme === "dark" ? "Light theme" : "Dark theme"} show={tip}>
        <button
          type="button"
          aria-label="toggle theme"
          aria-pressed={theme === "dark"}
          onClick={onToggleTheme}
          className={`${ITEM} ${pad} text-muted hover:bg-panel-2 hover:text-text`}
        >
          <Glyph name={theme === "dark" ? "sun" : "moon"} />
          {expanded && (theme === "dark" ? "Light theme" : "Dark theme")}
        </button>
      </Tip>
      <Tip label="Expand sidebar" show={tip}>
        <button
          type="button"
          aria-label={expanded ? "collapse sidebar" : "expand sidebar"}
          aria-expanded={expanded}
          onClick={toggle}
          className={`${ITEM} ${pad} text-faint hover:bg-panel-2 hover:text-text`}
        >
          <Glyph name="panel" />
          {expanded && "Collapse"}
        </button>
      </Tip>
    </nav>
  );
}

/** The switcher's glyph for "every workspace". */
function AllMark({ size = 24 }: { size?: number }): ReactElement {
  return (
    <span aria-hidden="true" className="grid shrink-0 place-items-center rounded-full bg-text text-bg" style={{ width: size, height: size }}>
      <Glyph name="all" size={Math.round(size * 0.55)} />
    </span>
  );
}

/** The menu's focusable rows, top to bottom. */
function menuItems(menu: HTMLElement | null): HTMLElement[] {
  return menu ? [...menu.querySelectorAll<HTMLElement>('[role="menuitem"], [role="menuitemradio"]')] : [];
}

/**
 * The scope: every workspace, or one org. Expanded it is a full-width row (mark, name, what waits,
 * chevron); collapsed, the mark alone. Either way it opens one menu, placed fixed beside the trigger
 * so the sidebar's own overflow never clips it.
 */
function ScopeSwitcher({
  orgs,
  hiddenOrgs,
  selectedOrg,
  onSelectOrg,
  onOpenOrgSettings,
  needByOrg,
  expanded,
}: {
  orgs: OrgEntry[];
  hiddenOrgs: OrgEntry[];
  selectedOrg: string | null;
  onSelectOrg: (org: string | null) => void;
  onOpenOrgSettings: (org: string) => void;
  needByOrg: Record<string, number>;
  expanded: boolean;
}): ReactElement {
  const [at, setAt] = useState<{ left: number; top: number } | null>(null);
  const [query, setQuery] = useState("");
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const open = at !== null;
  // A filter box once the list is long enough to hunt through; it takes focus when the menu opens.
  const searchable = orgs.length + hiddenOrgs.length > 6;
  const q = query.trim().toLowerCase();
  const shown = q ? orgs.filter((o) => o.org.toLowerCase().includes(q)) : orgs;
  const shownHidden = q ? hiddenOrgs.filter((o) => o.org.toLowerCase().includes(q)) : hiddenOrgs;

  const current = orgs.find((o) => sameOrg(o.org, selectedOrg)) ?? null;
  const name = current ? current.org : "All workspaces";
  const needTotal = Object.values(needByOrg).reduce((a, b) => a + b, 0);
  const needHere = current ? needFor(needByOrg, current.org) : needTotal;
  const settingsOrg = current?.org ?? orgs[0]?.org ?? hiddenOrgs[0]?.org ?? null;

  const show = () => {
    const box = trigger.current?.getBoundingClientRect();
    if (!box) return;
    setAt(expanded ? { left: box.left, top: box.bottom + 6 } : { left: box.right + 10, top: box.top });
  };
  const close = (refocus: boolean) => {
    setAt(null);
    setQuery("");
    if (refocus) trigger.current?.focus();
  };

  // Focus goes into the menu when it opens: the filter box when there is one, else the checked row.
  useEffect(() => {
    if (!open) return;
    if (search.current) {
      search.current.focus();
      return;
    }
    const items = menuItems(menu.current);
    (items.find((item) => item.getAttribute("aria-checked") === "true") ?? items[0])?.focus();
  }, [open]);

  const pick = (org: string | null) => {
    onSelectOrg(org);
    close(true);
  };
  const settings = (org: string) => {
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
    // In the filter box: Enter takes the first match, the arrows step into the list.
    if (document.activeElement === search.current) {
      if (event.key === "Enter") {
        event.preventDefault();
        (items.find((item) => item.dataset.match === "1") ?? items[0]).click();
        return;
      }
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        (event.key === "ArrowDown" ? items[0] : items[items.length - 1]).focus();
      }
      return;
    }
    const i = items.indexOf(document.activeElement as HTMLElement);
    const next =
      event.key === "ArrowDown" ? (i + 1) % items.length
      : event.key === "ArrowUp" ? (i <= 0 ? items.length - 1 : i - 1)
      : event.key === "Home" ? 0
      : event.key === "End" ? items.length - 1
      : null;
    if (next === null) return;
    event.preventDefault();
    items[next].focus();
  };

  const mark = current ? <Avatar name={current.org} src={current.avatar ?? undefined} size={24} rounded="full" /> : <AllMark />;

  return (
    <div className="mb-2">
      <Tip label={needHere > 0 ? `${name} · ${needHere} need you` : name} show={!expanded && !open}>
        <button
          ref={trigger}
          type="button"
          title={expanded ? undefined : "switch workspace"}
          aria-label="switch workspace"
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={() => (open ? close(false) : show())}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown" && !open) {
              event.preventDefault();
              show();
            }
          }}
          className={`relative flex h-11 w-full cursor-pointer items-center gap-2.5 rounded-[10px] border border-border bg-transparent text-left transition-colors hover:border-border-strong hover:bg-panel-2 ${expanded ? "px-2" : "justify-center border-transparent px-0"} ${open ? "bg-panel-2" : ""}`}
        >
          <span className="relative grid shrink-0 place-items-center">
            {mark}
            {needHere > 0 && <span aria-hidden="true" className="absolute -right-0.5 -top-0.5 h-2 w-2 rounded-full border-2 border-bg bg-warn" />}
          </span>
          {expanded && (
            <>
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[13.5px] font-medium text-text">{name}</span>
                <span className={`block text-[11.5px] tabular-nums ${needHere > 0 ? "text-warn" : "text-faint"}`}>
                  {needHere > 0 ? `${needHere} need you` : current ? `${current.live} live · ${current.total} colonies` : `${orgs.length} workspaces`}
                </span>
              </span>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true" className="shrink-0 text-faint">
                <path d="m8 9 4-4 4 4M8 15l4 4 4-4" />
              </svg>
            </>
          )}
        </button>
      </Tip>

      {open && (
        <>
          {/* Click-catcher beneath the menu: any click outside the options lands here and closes it. */}
          <div aria-hidden="true" className="fixed inset-0 z-40" onClick={() => close(false)} />
          <div
            ref={menu}
            role="menu"
            aria-label="workspaces"
            onKeyDown={onMenuKey}
            style={{ left: at.left, top: at.top }}
            className="v3-pop scroll-thin fixed z-50 max-h-[min(520px,calc(100vh-40px))] w-[280px] animate-[ck-in_160ms_ease-out_both] overflow-y-auto rounded-xl border border-border-strong p-1.5 shadow-[0_16px_48px_rgb(0_0_0/0.4)]"
          >
            {searchable && (
              <div className="mb-1 flex items-center gap-2 rounded-md border border-border px-2 py-1.5 focus-within:border-border-strong">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true" className="shrink-0 text-faint">
                  <circle cx="11" cy="11" r="6.5" />
                  <path d="m16 16 4 4" />
                </svg>
                <input
                  ref={search}
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  placeholder="Find a workspace…"
                  aria-label="find a workspace"
                  className="min-w-0 flex-1 border-0 bg-transparent p-0 text-[13px] text-text outline-none placeholder:text-faint focus-visible:outline-none"
                />
              </div>
            )}
            {!q && <ScopeRow label="All workspaces" note={needTotal > 0 ? `${needTotal} need you` : String(orgs.length)} urgent={needTotal > 0} active={selectedOrg === null} icon={<AllMark size={22} />} onClick={() => pick(null)} />}
            {!q && orgs.length > 0 && <div role="separator" className="my-1 h-px bg-border" />}
            {q && shown.length === 0 && shownHidden.length === 0 && <div className="px-2 py-3 text-[13px] text-faint">No workspace matches “{query.trim()}”.</div>}
            {shown.map((o) => {
              const need = needFor(needByOrg, o.org);
              return (
                <ScopeRow
                  key={o.org}
                  label={o.org}
                  note={need > 0 ? `${need} need you` : o.live > 0 ? `${o.live} live` : ""}
                  urgent={need > 0}
                  active={sameOrg(o.org, selectedOrg)}
                  icon={<Avatar name={o.org} src={o.avatar ?? undefined} size={22} rounded="full" />}
                  match
                  onClick={() => pick(o.org)}
                />
              );
            })}
            {shownHidden.length > 0 && (
              // Switched off in their settings, so not a choice; this is the way back to switching one on.
              <div role="group" aria-label="switched off" className="mt-1 border-t border-border pt-1">
                <div aria-hidden="true" className="px-2 pb-0.5 pt-1 text-[11.5px] text-faint">Switched off · open settings to turn on</div>
                {shownHidden.map((o) => (
                  <button
                    key={o.org}
                    type="button"
                    role="menuitem"
                    tabIndex={-1}
                    aria-label={`settings for ${o.org} (switched off)`}
                    onClick={() => settings(o.org)}
                    className="flex w-full cursor-pointer items-center gap-2.5 rounded-md border-0 bg-transparent px-2 py-1.5 text-left opacity-70 transition-colors hover:bg-panel-2 hover:opacity-100 focus-visible:bg-panel-2 focus-visible:outline-none"
                  >
                    <Avatar name={o.org} src={o.avatar ?? undefined} size={22} rounded="full" />
                    <span className="min-w-0 flex-1 truncate text-[13px] text-muted">{o.org}</span>
                    <Glyph name="settings" size={14} />
                  </button>
                ))}
              </div>
            )}
            {settingsOrg && (
              <>
                <div role="separator" className="my-1 h-px bg-border" />
                <button
                  type="button"
                  role="menuitem"
                  tabIndex={-1}
                  onClick={() => settings(settingsOrg)}
                  className="flex w-full cursor-pointer items-center gap-2.5 rounded-md border-0 bg-transparent p-2 text-left text-[13px] text-muted transition-colors hover:bg-panel-2 hover:text-text focus-visible:bg-panel-2 focus-visible:outline-none"
                >
                  <Glyph name="settings" size={15} />
                  {current ? `${current.org} settings` : "Manage workspaces"}
                </button>
              </>
            )}
          </div>
        </>
      )}
    </div>
  );
}

/** One choice in the switcher. */
function ScopeRow({ label, note, urgent, active, icon, match = false, onClick }: { label: string; note: string; urgent: boolean; active: boolean; icon: ReactElement; match?: boolean; onClick: () => void }): ReactElement {
  return (
    <button
      type="button"
      data-match={match ? "1" : undefined}
      role="menuitemradio"
      aria-checked={active}
      // Roving focus: the arrows move between rows, so Tab leaves the menu instead of walking it.
      tabIndex={-1}
      onClick={onClick}
      className={`grid w-full cursor-pointer grid-cols-[22px_minmax(0,1fr)_auto_14px] items-center gap-2.5 rounded-md border-0 p-2 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${active ? "bg-panel-2" : "bg-transparent hover:bg-panel-2 focus-visible:bg-panel-2"}`}
    >
      {icon}
      <span className="truncate text-[13.5px] text-text">{label}</span>
      <span className={`text-[12px] tabular-nums ${urgent ? "text-warn" : "text-faint"}`}>{note}</span>
      <span aria-hidden="true" className="text-accent">
        {active && (
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="m5 12.5 4.5 4.5L19 7.5" />
          </svg>
        )}
      </span>
    </button>
  );
}
