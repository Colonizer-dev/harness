// The narrow-screen cockpit (issue #516): below Tailwind's `sm` breakpoint the sidebar rail gives
// way to a five-tab bar fixed to the foot of the screen. The mapping is pure and pinned here so
// the bar and the "More" sheet can never disagree with the rail about what a view is called —
// `CockpitView` stays the one source of truth; these are only names and groupings over it.
import type { CockpitView } from "./NavRail";

/** The four views that earn a tab of their own, plus "More" for everything else. */
export type MobileTabId = "nest" | "inbox" | "chat" | "code" | "more";

export interface MobileTab {
  id: MobileTabId;
  label: string;
  /** The view the tab opens; null for "More", which opens the sheet instead of a view. */
  view: CockpitView | null;
  /** The NavRail glyph that draws the tab. */
  glyph: string;
}

/** The five bottom tabs, left to right: Nest, Inbox, Chat, Code, More. */
export const MOBILE_TABS: readonly MobileTab[] = [
  { id: "nest", label: "Nest", view: "home", glyph: "home" },
  { id: "inbox", label: "Inbox", view: "inbox", glyph: "inbox" },
  { id: "chat", label: "Chat", view: "chat", glyph: "chat" },
  { id: "code", label: "Code", view: "code", glyph: "code" },
  { id: "more", label: "More", view: null, glyph: "all" },
];

/**
 * Which tab a view lights up. The four primary views are themselves; an open colony reads as Nest
 * (that is where colonies are reached from); everything else — overview, launch, history, loops,
 * memory, host, secrets, settings — belongs to the "More" sheet.
 */
export function mobileTabFor(view: CockpitView): MobileTabId {
  if (view === "home" || view === "colony") return "nest";
  if (view === "inbox") return "inbox";
  if (view === "chat") return "chat";
  if (view === "code") return "code";
  return "more";
}

/** One row in the "More" sheet: a view outside the primary four, plus Settings at the end. */
export interface MobileMoreItem {
  view: CockpitView;
  label: string;
  glyph: string;
}

/** The sheet's rows, in the desktop rail's order; Settings, the rail's foot row, closes it. */
export function mobileMoreViews(): MobileMoreItem[] {
  return [
    { view: "overview", label: "Overview", glyph: "overview" },
    { view: "launch", label: "New colony", glyph: "plus" },
    { view: "history", label: "History", glyph: "history" },
    { view: "loops", label: "Loops", glyph: "loops" },
    { view: "memory", label: "Memory", glyph: "memory" },
    { view: "host", label: "Host", glyph: "host" },
    { view: "secrets", label: "Secrets", glyph: "secrets" },
    { view: "settings", label: "Settings", glyph: "settings" },
  ];
}
