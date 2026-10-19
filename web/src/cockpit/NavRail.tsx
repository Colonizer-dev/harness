// The cockpit's sidebar: a slim, quiet column of icons that opens into labels. Collapsed it is 64px
// of clear glyphs, each naming itself on hover or focus; expanded (the choice is remembered) every
// item carries its label and count. One accent does the talking: the Colonize button, and the bar
// beside wherever you are. The workspace switcher sits at the top and is the cockpit's scope: every
// view reads it, and Overview shows the chosen org's dashboard, or every workspace when none is.
import { useEffect, useRef, useState, type ReactElement, type ReactNode } from "react";

import { Avatar } from "../components/Avatar";
import { sameOrg, store, stored } from "../components/ui";
import type { OrgEntry } from "../orgs";
import type { UpdateStatus } from "../types";
import { AntGlyph } from "./chat/PersonaAnt";
import { useColonize } from "./Colonize";
import { needFor } from "./feed";
import { AllMark, WorkspacePanel } from "./WorkspaceMenu";

export type CockpitView = "overview" | "home" | "colony" | "launch" | "inbox" | "history" | "queues" | "loops" | "settings" | "memory" | "host" | "secrets" | "code" | "chat";

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
/** The inbox is not here: it lives behind the bell at the top right (NotificationsBell). */
export function navTabs({ liveCount, pendingMemory }: { needCount: number; liveCount: number; pendingMemory: number }): NavTab[] {
  return [
    { view: "overview", label: "Overview", count: "" },
    { view: "home", label: "Nest", count: liveCount || "" },
    { view: "chat", label: "Chat", count: "" },
    { view: "code", label: "Code", count: "" },
    { view: "history", label: "History", count: "" },
    { view: "queues", label: "Queues", count: "" },
    { view: "loops", label: "Loops", count: "" },
    { view: "memory", label: "Memory", count: pendingMemory || "", urgent: pendingMemory > 0 },
    { view: "host", label: "Host", count: "" },
    { view: "secrets", label: "Secrets", count: "" },
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
  host: (
    <>
      <rect x="3.5" y="4" width="17" height="6.5" rx="2" />
      <rect x="3.5" y="13.5" width="17" height="6.5" rx="2" />
      <path d="M7 7.25h.01M7 16.75h.01M11 7.25h6M11 16.75h6" />
    </>
  ),
  code: (
    <>
      <path d="m8.5 8-4 4 4 4M15.5 8l4 4-4 4" />
      <path d="m13.5 5-3 14" />
    </>
  ),
  loops: (
    <>
      <path d="M4 12a8 8 0 0 1 13.7-5.6L20 8.5" />
      <path d="M20 4v4.5h-4.5" />
      <path d="M20 12a8 8 0 0 1-13.7 5.6L4 15.5" />
      <path d="M4 20v-4.5h4.5" />
    </>
  ),
  queues: (
    <>
      <path d="M4 6.5h.01M4 12h.01M4 17.5h.01" />
      <path d="M8.5 6.5H20M8.5 12H20M8.5 17.5H20" />
    </>
  ),
  chat: (
    <>
      <path d="M4.5 6.5A2 2 0 0 1 6.5 4.5h11a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2H11l-4 3.5v-3.5H6.5a2 2 0 0 1-2-2z" />
      <path d="M8.5 9.5h7M8.5 12.5h4.5" />
    </>
  ),
  secrets: (
    <>
      <circle cx="8" cy="15" r="4" />
      <path d="m10.8 12.2 8.2-8.2M16 7l2.5 2.5M13.5 9.5l2 2" />
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

/** ⌘B on a Mac, Ctrl+B elsewhere — what the toggle's tooltip names. */
const SHORTCUT = typeof navigator !== "undefined" && /Mac|iP(hone|ad)/.test(navigator.platform) ? "⌘B" : "Ctrl+B";

/** The outpost, drawn exactly as colonizer.dev draws it: a hexagon outline with a beacon, no tile. */
function BrandMark(): ReactElement {
  return (
    <span className="grid h-8 w-8 shrink-0 place-items-center text-accent [filter:drop-shadow(0_0_6px_color-mix(in_oklab,var(--accent)_45%,transparent))]">
      <svg width="24" height="24" viewBox="0 0 26 26" aria-hidden="true">
        <polygon points="13,2 23,7.5 23,18.5 13,24 3,18.5 3,7.5" fill="none" stroke="currentColor" strokeWidth="2" strokeLinejoin="round" />
        <circle cx="13" cy="13" r="3" fill="currentColor" />
      </svg>
    </span>
  );
}

/** One of the rail's own glyphs by name. Shared with the mobile tab bar, which hides this rail. */
export function Glyph({ name, size = 20 }: { name: string; size?: number }): ReactElement {
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
          className={`v3-pop pointer-events-none fixed z-50 -translate-y-1/2 whitespace-nowrap rounded-md border border-border-strong px-2 py-1 text-small-lg text-text shadow-[0_8px_24px_rgb(0_0_0/0.3)] transition-opacity duration-100 ${at ? "opacity-100" : "left-0 top-0 opacity-0"}`}
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
    return <span className={`ml-auto text-small tabular-nums ${urgent ? "text-warn" : "text-faint"}`}>{value}</span>;
  }
  return (
    <span
      aria-hidden="true"
      className={`absolute -right-0.5 -top-0.5 grid h-4 min-w-4 place-items-center rounded-full px-1 font-mono text-micro font-semibold tabular-nums ${urgent ? "bg-warn text-bg" : "bg-panel-3 text-muted"}`}
    >
      {value}
    </span>
  );
}

const ITEM =
  "group relative flex h-10 w-full cursor-pointer items-center gap-3 rounded-[10px] border-0 bg-transparent text-body transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent";

export function NavRail(props: {
  /** Workspaces only: orgEntries() has already dropped the undecided and the switched-off. */
  orgs: OrgEntry[];
  /** Switched-off orgs: not a choice, but listed so their settings (and the switch back on) stay reachable. */
  hiddenOrgs: OrgEntry[];
  onOpenOrgSettings: (org: string) => void;
  /** "Manage orgs…": Settings → Workspaces → Show or hide orgs, where hidden orgs are listed (issue #1213). */
  onManageOrgs?: () => void;
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
  /** The installed version and whether a newer one is out; an available update shows in the foot. */
  update?: UpdateStatus | null;
  onOpenUpdates?: () => void;
  /** Opens the Colonize pane; absent, the surrounding ColonizeProvider's, and outside one (static
   *  tests) the button goes to the Launch view instead. */
  onColonize?: () => void;
  /** Start expanded regardless of the remembered choice; the tests pin both states through it. */
  initialExpanded?: boolean;
}): ReactElement {
  const { orgs, hiddenOrgs, onOpenOrgSettings, selectedOrg, onSelectOrg, needByOrg, view, onNavigate, inboxCount, liveCount, pendingMemory, theme, onToggleTheme, update = null, onOpenUpdates } = props;
  const colonizer = useColonize();
  const onColonize = props.onColonize ?? colonizer?.open;
  const [expanded, setExpanded] = useState<boolean>(() => props.initialExpanded ?? stored(EXPANDED_KEY) !== "0");
  const toggle = () =>
    setExpanded((open) => {
      store(EXPANDED_KEY, open ? "0" : "1");
      return !open;
    });

  // ⌘B / Ctrl+B minimises and restores the sidebar from anywhere but a text field.
  useEffect(() => {
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey || event.key.toLowerCase() !== "b") return;
      const target = event.target as HTMLElement | null;
      if (target && (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName))) return;
      event.preventDefault();
      setExpanded((open) => {
        store(EXPANDED_KEY, open ? "0" : "1");
        return !open;
      });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const pad = expanded ? "px-3" : "justify-center px-0";
  const tabs = navTabs({ needCount: inboxCount, liveCount, pendingMemory });
  const tip = !expanded;

  return (
    <nav
      aria-label="cockpit"
      data-expanded={expanded}
      // Below Tailwind's `sm` breakpoint the rail is hidden and MobileTabBar takes over (issue #516);
      // from `sm` up this rail is exactly what it has always been.
      className={`v3-rail relative z-20 hidden h-full min-h-0 shrink-0 flex-col gap-1 border-r border-border px-3 py-4 transition-[width] duration-200 ease-out sm:flex ${expanded ? "w-[232px]" : "w-16"}`}
    >
      {/* Brand and the minimise toggle. Expanded: the mark and name open the overview, and the
          panel button beside them folds the sidebar away. Collapsed: the mark turns into the
          expand button on hover, so the way back out is exactly where the way in was. */}
      <div className={`mb-3 flex h-10 items-center ${expanded ? "justify-between" : "justify-center"}`}>
        {expanded ? (
          <>
            <button
              type="button"
              aria-label="overview · every colony"
              onClick={() => onNavigate("overview")}
              className="flex h-10 min-w-0 cursor-pointer items-center gap-2.5 rounded-[10px] border-0 bg-transparent px-1.5"
            >
              <BrandMark />
              <span className="text-title-lg font-semibold lowercase leading-none tracking-[-0.035em] text-text">colonizer</span>
            </button>
            <Tip label={`Minimise sidebar · ${SHORTCUT}`} show>
              <button
                type="button"
                aria-label="collapse sidebar"
                aria-expanded
                aria-keyshortcuts="Meta+B Control+B"
                onClick={toggle}
                className="grid h-8 w-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-faint transition-colors hover:bg-panel-2 hover:text-text"
              >
                <Glyph name="panel" size={18} />
              </button>
            </Tip>
          </>
        ) : (
          <Tip label={`Expand sidebar · ${SHORTCUT}`} show>
            <button
              type="button"
              aria-label="expand sidebar"
              aria-expanded={false}
              aria-keyshortcuts="Meta+B Control+B"
              onClick={toggle}
              className="group/brand grid h-10 w-10 cursor-pointer place-items-center rounded-[10px] border-0 bg-transparent"
            >
              <span className="group-hover/brand:hidden group-focus-visible/brand:hidden">
                <BrandMark />
              </span>
              <span className="hidden h-8 w-8 place-items-center rounded-[10px] bg-panel-2 text-text group-hover/brand:grid group-focus-visible/brand:grid">
                <Glyph name="panel" size={18} />
              </span>
            </button>
          </Tip>
        )}
      </div>

      <ScopeSwitcher
        orgs={orgs}
        hiddenOrgs={hiddenOrgs}
        selectedOrg={selectedOrg}
        onSelectOrg={onSelectOrg}
        onOpenOrgSettings={onOpenOrgSettings}
        onManageOrgs={props.onManageOrgs}
        needByOrg={needByOrg}
        expanded={expanded}
      />

      <Tip label="Colonize · ⌘K" show={tip}>
        <button
          type="button"
          aria-label="colonize"
          aria-haspopup={onColonize ? "dialog" : undefined}
          aria-keyshortcuts="Meta+K Control+K"
          aria-current={!onColonize && view === "launch" ? "page" : undefined}
          onClick={() => (onColonize ? onColonize() : onNavigate("launch"))}
          className={`v3-launch ant-glyph-host mb-3 flex h-10 w-full cursor-pointer items-center gap-2.5 rounded-[10px] border-0 font-semibold text-body transition-[filter,transform] duration-150 hover:brightness-110 active:scale-[0.98] ${pad}`}
        >
          <AntGlyph size={24} className="-m-[3px]" />
          {expanded && "Colonize"}
          {expanded && <kbd className="ml-auto font-sans text-meta font-medium opacity-70">⌘K</kbd>}
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

      {update?.available && onOpenUpdates && (
        <Tip label={`Update available${update.latest ? ` · ${update.latest.version}` : ""}`} show={tip}>
          <button
            type="button"
            aria-label={`update available${update.latest ? ` · ${update.installed.version} → ${update.latest.version}` : ""}`}
            onClick={onOpenUpdates}
            className={`${ITEM} ${pad} text-accent hover:bg-panel-2`}
          >
            <span className="relative grid place-items-center">
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M12 4v11M7 10l5 5 5-5M5 20h14" />
              </svg>
              <span aria-hidden="true" className="absolute -right-0.5 -top-0.5 h-2 w-2 animate-pulse rounded-full bg-accent" />
            </span>
            {expanded && <span className="truncate">Update{update.latest ? ` · ${update.latest.version}` : ""}</span>}
          </button>
        </Tip>
      )}
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
    </nav>
  );
}

/** The switcher's glyph for "every workspace". */

/**
 * The scope: every workspace, or one org. Expanded it is a full-width row (mark, name, what waits,
 * chevron); collapsed, the mark alone. Either way it opens the workspace menu, a Spotlight panel
 * (issue #1228) with a search box and Pinned, Recent and All workspaces.
 */
function ScopeSwitcher({
  orgs,
  hiddenOrgs,
  selectedOrg,
  onSelectOrg,
  onOpenOrgSettings,
  onManageOrgs,
  needByOrg,
  expanded,
}: {
  orgs: OrgEntry[];
  hiddenOrgs: OrgEntry[];
  selectedOrg: string | null;
  onSelectOrg: (org: string | null) => void;
  onOpenOrgSettings: (org: string) => void;
  onManageOrgs?: () => void;
  needByOrg: Record<string, number>;
  expanded: boolean;
}): ReactElement {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const current = orgs.find((o) => sameOrg(o.org, selectedOrg)) ?? null;
  const name = current ? current.org : "All workspaces";
  const needTotal = Object.values(needByOrg).reduce((a, b) => a + b, 0);
  const needHere = current ? needFor(needByOrg, current.org) : needTotal;
  const mark = current ? <Avatar name={current.org} src={current.avatar ?? undefined} size={24} rounded="full" /> : <AllMark />;

  return (
    <div className="mb-2">
      <Tip label={needHere > 0 ? `${name} · ${needHere} need you` : name} show={!expanded && !open}>
        <button
          ref={trigger}
          type="button"
          title={expanded ? undefined : "switch workspace"}
          aria-label="switch workspace"
          aria-haspopup="dialog"
          aria-expanded={open}
          onClick={() => setOpen((o) => !o)}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown" && !open) {
              event.preventDefault();
              setOpen(true);
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
                <span className="block truncate text-body font-medium text-text">{name}</span>
                <span className={`block text-meta-lg tabular-nums ${needHere > 0 ? "text-warn" : "text-faint"}`}>
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
        <WorkspacePanel
          anchor={trigger}
          orgs={orgs}
          hiddenOrgs={hiddenOrgs}
          selectedOrg={selectedOrg}
          needByOrg={needByOrg}
          onSelect={onSelectOrg}
          onOpenOrgSettings={onOpenOrgSettings}
          onManageOrgs={onManageOrgs}
          onClose={() => setOpen(false)}
        />
      )}
    </div>
  );
}
