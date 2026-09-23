import { useState, type ReactElement, type ReactNode } from "react";

import { Avatar } from "../components/Avatar";
import { IconOrg } from "../components/icons";
import { LogoMark } from "../components/Sidebar";
import { sameOrg } from "../components/ui";
import { toggledOrg, type OrgEntry } from "../orgs";
import { needFor } from "./feed";

export type CockpitView = "overview" | "home" | "colony" | "launch" | "inbox" | "history" | "settings" | "memory";

// Shared geometry for the 36px squares so every rail button reads as one family.
const SQUARE = "grid h-9 w-9 shrink-0 place-items-center rounded-[10px] transition-colors";

// A view is quiet until it is the one you are on; accent is the only "you are here" signal.
const viewTone = (active: boolean) => (active ? "text-accent" : "text-muted");

/**
 * A rail button and the name that appears beside it on hover or keyboard focus. The rail is 56px of
 * unlabelled icons and avatars, so without this the only way to read one is the native `title`, which
 * takes about a second to appear and never shows for a keyboard user at all.
 *
 * The label is placed with `position: fixed` from the item's own rectangle rather than hung off it
 * with `absolute`, because the org list scrolls: a scroll container clips on both axes, and an
 * absolutely placed label beside a scrolled avatar would be cut off at the rail's edge.
 */
function RailItem({ label, children }: { label: string; children: ReactNode }): ReactElement {
  const [at, setAt] = useState<{ left: number; top: number } | null>(null);
  const show = (event: { currentTarget: HTMLElement }) => {
    const box = event.currentTarget.getBoundingClientRect();
    setAt({ left: box.right + 8, top: box.top + box.height / 2 });
  };
  const hide = () => setAt(null);
  return (
    <div className="relative" onMouseEnter={show} onMouseLeave={hide} onFocus={show} onBlur={hide}>
      {children}
      <span
        role="tooltip"
        style={at ? { left: at.left, top: at.top } : undefined}
        className={`pointer-events-none fixed z-50 -translate-y-1/2 whitespace-nowrap rounded-md border border-border bg-panel px-2 py-1 text-[12px] text-text shadow-[var(--shadow)] transition-opacity duration-100 ${at ? "opacity-100" : "left-0 top-0 opacity-0"}`}
      >
        {label}
      </span>
    </div>
  );
}

export function Rail(props: {
  /** Workspaces only: orgEntries() has already dropped the undecided and the switched-off. */
  orgs: OrgEntry[];
  selectedOrg: string | null;
  /** null is "every workspace": the All workspaces item, or a second click on the org already chosen. */
  onSelectOrg: (org: string | null) => void;
  view: CockpitView;
  onNavigate: (view: CockpitView) => void;
  needCount: number;
  /** Keyed by lowercased org (needCountByOrg); read through needFor. */
  needByOrg: Record<string, number>;
  /** Memory proposals waiting for review, for the memory item's badge (memoryBadge). */
  pendingMemory: number;
  theme: "light" | "dark" | null;
  onToggleTheme: () => void;
}): ReactElement {
  const { orgs, selectedOrg, onSelectOrg, view, onNavigate, needCount, needByOrg, pendingMemory, theme, onToggleTheme } = props;

  return (
    // Only the org list scrolls; the overview, launch and the foot stay put however many orgs
    // there are. The hover labels are fixed-positioned (RailItem), so no overflow rule eats them.
    <nav
      aria-label="rail"
      className="relative z-20 flex w-14 shrink-0 flex-col items-center gap-1.5 border-r border-border bg-panel py-3"
    >
      <RailItem label="overview · every colony">
        <button
          type="button"
          aria-label="overview · every colony"
          aria-pressed={view === "overview"}
          onClick={() => onNavigate("overview")}
          className={`${SQUARE} hover:bg-panel-2 ${view === "overview" ? "bg-accent-soft" : ""}`}
        >
          <LogoMark size={26} />
        </button>
      </RailItem>

      <div aria-hidden="true" className="my-1.5 h-px w-5 bg-border" />

      <RailItem label="all workspaces">
        <button
          type="button"
          aria-label="all workspaces"
          aria-pressed={selectedOrg === null}
          onClick={() => onSelectOrg(null)}
          className={`${SQUARE} hover:bg-panel-2 ${selectedOrg === null ? "text-accent ring-2 ring-accent" : "text-muted"}`}
        >
          <IconOrg size={17} />
        </button>
      </RailItem>

      {/* Shrinks before anything else does, and scrolls once it has; py/px leave room for the ring. */}
      <div aria-label="workspaces" role="group" className="scroll-thin flex min-h-0 shrink flex-col items-center gap-1.5 overflow-y-auto px-1 py-0.5">
        {orgs.map((o) => {
          // Missing from the map means nothing of this org's is waiting.
          const need = needFor(needByOrg, o.org);
          const active = sameOrg(o.org, selectedOrg);
          // A second click on the chosen org is the quick way back to every workspace, so say so.
          const name = active ? `${o.org} · selected, click for all workspaces` : o.org;
          const label = need > 0 ? `${name} · ${need} need you` : name;
          return (
            <RailItem key={o.org} label={label}>
              <button
                type="button"
                aria-label={label}
                aria-pressed={active}
                onClick={() => onSelectOrg(toggledOrg(selectedOrg, o.org))}
                className={`${SQUARE} relative hover:bg-panel-2 ${active ? "ring-2 ring-accent" : ""}`}
              >
                <Avatar name={o.org} src={o.avatar} size={26} rounded="md" />
                {need > 0 && (
                  // Panel-coloured ring keeps the dot legible over any avatar image.
                  <span
                    aria-hidden="true"
                    className="absolute right-1 top-1 h-[7px] w-[7px] rounded-full border-2 border-panel bg-warn"
                  />
                )}
              </button>
            </RailItem>
          );
        })}
      </div>

      <RailItem label="launch a colony">
        <button
          type="button"
          aria-label="launch a colony"
          onClick={() => onNavigate("launch")}
          className={`${SQUARE} border border-dashed border-border-strong text-faint hover:border-accent hover:text-accent`}
        >
          <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M8 2v12M2 8h12" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
          </svg>
        </button>
      </RailItem>

      {/* The spacer pushes the steady-state buttons to the foot of the rail. */}
      <div className="min-h-0 flex-1" />

      <RailItem label={pendingMemory > 0 ? `memory · ${pendingMemory} to review` : "memory"}>
        <button
          type="button"
          aria-label={pendingMemory > 0 ? `memory · ${pendingMemory} to review` : "memory"}
          aria-pressed={view === "memory"}
          onClick={() => onNavigate("memory")}
          className={`${SQUARE} relative hover:bg-panel-2 ${viewTone(view === "memory")}`}
        >
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M5 4h11l3 3v13H5z" />
            <path d="M9 10h6M9 14h6M9 18h3" />
          </svg>
          {pendingMemory > 0 && (
            <span className="absolute right-0.5 top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-accent px-1 text-center font-mono text-[10px] font-semibold text-on-accent tabular-nums">
              {pendingMemory}
            </span>
          )}
        </button>
      </RailItem>

      <RailItem label="history">
        <button
          type="button"
          aria-label="history"
          aria-pressed={view === "history"}
          onClick={() => onNavigate("history")}
          className={`${SQUARE} hover:bg-panel-2 ${viewTone(view === "history")}`}
        >
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M3 12a9 9 0 1 0 3-6.7" />
            <path d="M3 4v5h5" />
            <path d="M12 8v4l3 2" />
          </svg>
        </button>
      </RailItem>

      <RailItem label={needCount > 0 ? `inbox · ${needCount} need you` : "inbox · needs you and notifications"}>
        <button
          type="button"
          aria-label={needCount > 0 ? `inbox · ${needCount} need you` : "inbox"}
          aria-pressed={view === "inbox"}
          onClick={() => onNavigate("inbox")}
          className={`${SQUARE} relative hover:bg-panel-2 ${viewTone(view === "inbox")}`}
        >
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M12 3a5 5 0 0 0-5 5v3.5L5 15h14l-2-3.5V8a5 5 0 0 0-5-5z" />
            <path d="M10 19a2 2 0 0 0 4 0" />
          </svg>
          {needCount > 0 && (
            // The count is the whole point of the badge, so it goes to the screen reader too.
            <span className="absolute right-0.5 top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-warn px-1 text-center font-mono text-[10px] font-semibold text-on-accent tabular-nums">
              {needCount}
            </span>
          )}
        </button>
      </RailItem>

      <RailItem label={theme === "dark" ? "switch to light" : "switch to dark"}>
        <button
          type="button"
          aria-label="toggle theme"
          aria-pressed={theme === "dark"}
          onClick={onToggleTheme}
          className={`${SQUARE} text-faint hover:bg-panel-2 hover:text-text`}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
            <circle cx="12" cy="12" r="4.5" />
            <path d="M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.3 5.3l1.4 1.4M17.3 17.3l1.4 1.4M5.3 18.7l1.4-1.4M17.3 6.7l1.4-1.4" strokeLinecap="round" />
          </svg>
        </button>
      </RailItem>

      <RailItem label="settings · modules">
        <button
          type="button"
          aria-label="settings"
          aria-pressed={view === "settings"}
          onClick={() => onNavigate("settings")}
          className={`${SQUARE} hover:bg-panel-2 ${viewTone(view === "settings")}`}
        >
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true">
            <path d="M4 7h10M18 7h2M4 17h4M12 17h8" />
            <circle cx="16" cy="7" r="2" />
            <circle cx="10" cy="17" r="2" />
          </svg>
        </button>
      </RailItem>
    </nav>
  );
}
