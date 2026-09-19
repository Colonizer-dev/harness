import type { ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import { LogoMark } from "../components/Sidebar";
import type { OrgEntry } from "../orgs";

export type CockpitView = "home" | "colony" | "launch" | "inbox" | "history" | "settings" | "memory";

// Shared geometry for the 36px squares so every rail button reads as one family.
const SQUARE = "grid h-9 w-9 shrink-0 place-items-center rounded-[10px] transition-colors";

// A view is quiet until it is the one you are on; accent is the only "you are here" signal.
const viewTone = (active: boolean) => (active ? "text-accent" : "text-muted");

export function Rail(props: {
  /** Workspaces only: orgEntries() has already dropped the undecided and the switched-off. */
  orgs: OrgEntry[];
  selectedOrg: string | null;
  onSelectOrg: (org: string) => void;
  view: CockpitView;
  onNavigate: (view: CockpitView) => void;
  needCount: number;
  needByOrg: Record<string, number>;
  theme: "light" | "dark" | null;
  onToggleTheme: () => void;
}): ReactElement {
  const { orgs, selectedOrg, onSelectOrg, view, onNavigate, needCount, needByOrg, theme, onToggleTheme } = props;

  return (
    <nav
      aria-label="rail"
      className="flex min-h-0 w-14 shrink-0 flex-col items-center gap-1.5 overflow-hidden border-r border-border bg-panel py-3"
    >
      <button
        type="button"
        title="colonizer · base"
        aria-label="colonizer · base"
        onClick={() => onNavigate("home")}
        className={`${SQUARE} hover:bg-panel-2`}
      >
        <LogoMark size={26} />
      </button>

      <div aria-hidden="true" className="my-1.5 h-px w-5 bg-border" />

      {orgs.map((o) => {
        // Missing from the map means the mothership has not reported anything for this org yet.
        const need = needByOrg[o.org] ?? 0;
        const active = o.org === selectedOrg;
        return (
          <button
            key={o.org}
            type="button"
            title={o.org}
            aria-label={o.org}
            aria-pressed={active}
            onClick={() => onSelectOrg(o.org)}
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
        );
      })}

      <button
        type="button"
        title="launch a colony"
        aria-label="launch a colony"
        onClick={() => onNavigate("launch")}
        className={`${SQUARE} border border-dashed border-border-strong text-faint hover:border-accent hover:text-accent`}
      >
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
          <path d="M8 2v12M2 8h12" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
        </svg>
      </button>

      {/* The spacer pushes the steady-state buttons to the foot of the rail. */}
      <div className="min-h-0 flex-1" />

      <button
        type="button"
        title="history"
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

      <button
        type="button"
        title="inbox · needs you and notifications"
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

      <button
        type="button"
        title="settings · modules"
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

      <button
        type="button"
        title="light / dark"
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
    </nav>
  );
}
