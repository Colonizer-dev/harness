// The narrow-screen nav (issue #516): below `sm` the sidebar rail hides and this five-tab bar sits
// fixed to the foot of the screen — Nest, Inbox, Chat, Code, and More, which opens a small sheet of
// the remaining views plus Settings. At ≥ 640px the whole thing is `display: none` and the desktop
// rail is exactly as it was. Tabs are 56px tall; the bar stands on the safe-area inset so a
// home-indicator phone does not draw the tabs under it.
import { useState, type ReactElement } from "react";
import { createPortal } from "react-dom";

import { Glyph, type CockpitView } from "./NavRail";
import { MOBILE_TABS, mobileMoreViews, mobileTabFor } from "./mobile";

export function MobileTabBar({
  view,
  onNavigate,
  inboxCount,
}: {
  view: CockpitView;
  onNavigate: (view: CockpitView) => void;
  /** The inbox's cross-workspace "need you" count, shown as a bubble on its tab like the rail's. */
  inboxCount: number;
}): ReactElement {
  const [moreOpen, setMoreOpen] = useState(false);
  const active = mobileTabFor(view);
  const go = (next: CockpitView) => {
    setMoreOpen(false);
    onNavigate(next);
  };

  // The sheet portals out of the cockpit: the cockpit root is an `isolate` stacking context, so an
  // unportaled sheet could never rise above App's own fixed cards (the live-map prompt, the storage
  // alert) however high its z-index went. At the body the plain z order applies — the sheet above
  // the cards, the settings dialog above the sheet. (The static-markup tests run without a document.)
  const sheet = moreOpen ? (
    <>
      <div aria-hidden="true" className="fixed inset-0 z-40 bg-bg/60 sm:hidden" onClick={() => setMoreOpen(false)} />
      <div
        role="menu"
        aria-label="More views"
        className="fixed inset-x-2 bottom-[calc(3.75rem+env(safe-area-inset-bottom))] z-50 grid animate-[ck-in_160ms_ease-out_both] grid-cols-3 gap-1 rounded-2xl border border-border-strong bg-panel p-2 shadow-[0_16px_48px_rgb(0_0_0/0.4)] sm:hidden"
      >
        {mobileMoreViews().map((item) => (
          <button
            key={item.view}
            type="button"
            role="menuitem"
            aria-current={view === item.view ? "page" : undefined}
            onClick={() => go(item.view)}
            className={`flex min-h-16 cursor-pointer flex-col items-center justify-center gap-1 rounded-xl border-0 bg-transparent px-1 text-[12px] transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
              view === item.view ? "bg-panel-2 text-text" : "text-muted hover:bg-panel-2 hover:text-text"
            }`}
          >
            <Glyph name={item.glyph} size={20} />
            {item.label}
          </button>
        ))}
      </div>
    </>
  ) : null;
  return (
    <>
      {typeof document === "undefined" ? sheet : createPortal(sheet, document.body)}
      <nav aria-label="cockpit" className="fixed inset-x-0 bottom-0 z-50 flex border-t border-border bg-panel pb-[env(safe-area-inset-bottom)] sm:hidden">
        {MOBILE_TABS.map((tab) => {
          const on = tab.id === active;
          const count = tab.id === "inbox" && inboxCount > 0 ? inboxCount : null;
          const label = count !== null ? `${tab.label} · ${count}` : tab.label;
          return (
            <button
              key={tab.id}
              type="button"
              aria-label={label}
              aria-current={on ? "page" : undefined}
              onClick={() => (tab.view ? go(tab.view) : setMoreOpen((open) => !open))}
              className={`relative flex h-14 min-w-0 flex-1 cursor-pointer flex-col items-center justify-center gap-0.5 border-0 bg-transparent transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent ${on ? "text-accent" : "text-muted"}`}
            >
              <span className="relative grid place-items-center">
                <Glyph name={tab.glyph} size={20} />
                {count !== null && (
                  <span aria-hidden="true" className="absolute -right-2 -top-1 grid h-4 min-w-4 place-items-center rounded-full bg-warn px-1 font-mono text-[9.5px] font-semibold text-bg">
                    {count}
                  </span>
                )}
              </span>
              <span className="text-[10.5px] leading-none">{tab.label}</span>
            </button>
          );
        })}
      </nav>
    </>
  );
}
