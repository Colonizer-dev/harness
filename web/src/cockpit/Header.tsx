import { useState } from "react";
import type { ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import type { OrgInfo, UpdateStatus } from "../types";

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
  orgs: OrgInfo[];
  selectedOrg: string | null;
  onSelectOrg: (org: string) => void;
  /** Colonies of that org waiting on an answer — not the same thing as its pending memory notes. */
  needByOrg: Record<string, number>;
  crumb: string;
  liveCount: number;
  needCount: number;
  costToday: number | null;
  update: UpdateStatus | null;
  onOpenUpdates: () => void;
}): ReactElement {
  const { orgs, selectedOrg, onSelectOrg, needByOrg, crumb, liveCount, needCount, costToday, update, onOpenUpdates } =
    props;
  const [menuOpen, setMenuOpen] = useState(false);

  const current = orgs.find((o) => o.org === selectedOrg) ?? null;
  // With no org chosen the header speaks for the base itself.
  const orgName = current ? current.org : "colonizer";

  const pickOrg = (org: string) => {
    onSelectOrg(org);
    setMenuOpen(false);
  };

  return (
    <header className="flex h-12 shrink-0 items-center gap-3.5 border-b border-border px-5">
      <div className="relative">
        <button
          type="button"
          title="switch organisation"
          aria-label="switch organisation"
          aria-haspopup="listbox"
          aria-expanded={menuOpen}
          onClick={() => setMenuOpen((open) => !open)}
          className="-ml-2 flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm font-semibold text-text transition-colors hover:bg-panel-2"
        >
          <Avatar name={orgName} src={current?.avatar_url} size={18} rounded="md" />
          {orgName}
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" className="text-faint">
            <path d="m6 9 6 6 6-6" />
          </svg>
        </button>

        {menuOpen && (
          <>
            {/* Click-catcher beneath the menu: any click outside the options lands here and closes it. */}
            <div aria-hidden="true" className="fixed inset-0 z-30" onClick={() => setMenuOpen(false)} />
            <div
              role="listbox"
              aria-label="organisations"
              className="absolute left-0 top-full z-40 mt-2 w-[280px] animate-[ck-in_180ms_ease-out_both] rounded-[14px] border border-border bg-panel p-1.5 shadow-xl"
            >
              {orgs.map((o) => {
                const active = o.org === selectedOrg;
                const need = needByOrg[o.org] ?? 0;
                return (
                  <button
                    key={o.org}
                    type="button"
                    role="option"
                    aria-selected={active}
                    onClick={() => pickOrg(o.org)}
                    className={`grid w-full grid-cols-[28px_minmax(0,1fr)_auto] items-center gap-2.5 rounded-[10px] px-2.5 py-2 text-left transition-colors ${active ? "bg-accent-soft" : "hover:bg-panel-2"}`}
                  >
                    <Avatar name={o.org} src={o.avatar_url} size={28} rounded="md" />
                    <span className="min-w-0">
                      <span className="block truncate text-[13.5px] font-semibold text-text">{o.org}</span>
                      <span className="block font-mono text-[11px] text-faint">{o.colonies.total} colonies · {o.colonies.live} live</span>
                    </span>
                    {/* Silent when nothing waits: the row should only speak up when it has something to ask for. */}
                    <span className="font-mono text-[11px] tabular-nums text-warn">{need > 0 ? `${need} need you` : ""}</span>
                  </button>
                );
              })}
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
        {costToday !== null && <span className="tabular-nums">${costToday.toFixed(2)} today</span>}
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
