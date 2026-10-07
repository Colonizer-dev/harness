// The small pieces of Spotlight: a result row, the icon tile, the highlighted title, the key caps,
// the top-bar pill and the phone button. Presentational only.
import { Fragment, useEffect, useRef, type ReactElement, type ReactNode } from "react";

import { Avatar } from "../../components/Avatar";
import {
  IconAnt,
  IconBranch,
  IconChat,
  IconDownload,
  IconOrg,
  IconQuestion,
  IconRefresh,
  IconRepeat,
  IconSearch,
  IconSettings,
  IconSidebar,
  IconSpark,
  IconStack,
  IconStop,
} from "../../components/icons";
import { cx } from "../../components/ui";
import { AntGlyph } from "../chat/PersonaAnt";
import type { IconName, Result } from "./logic";

const IS_MAC = typeof navigator !== "undefined" && /mac|iphone|ipad/i.test(navigator.platform || navigator.userAgent);
/** The modifier key's cap: ⌘ on a Mac, Ctrl elsewhere. */
export const MOD = IS_MAC ? "⌘" : "Ctrl";

function Dot({ size = 15 }: { size?: number }): ReactElement {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" aria-hidden="true">
      <circle cx="12" cy="12" r="8.5" />
      <circle cx="12" cy="12" r="1.6" fill="currentColor" stroke="none" />
    </svg>
  );
}

function Clock({ size = 15 }: { size?: number }): ReactElement {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V12l3 2" />
    </svg>
  );
}

function Glyph({ name }: { name: IconName }): ReactElement {
  switch (name) {
    case "colony":
      return <IconAnt size={15} />;
    case "repo":
      return <IconBranch size={15} />;
    case "org":
      return <IconOrg size={15} />;
    case "settings":
      return <IconSettings size={15} />;
    case "view":
      return <IconSidebar size={15} />;
    case "colonize":
      return <AntGlyph size={22} />;
    case "stop":
      return <IconStop size={14} />;
    case "resume":
      return <IconRefresh size={15} />;
    case "front":
      return <IconStack size={15} />;
    case "model":
      return <IconSpark size={15} />;
    case "update":
      return <IconDownload size={15} />;
    case "issue":
      return <Dot />;
    case "ask":
      return <IconQuestion size={15} />;
    case "chat":
      return <IconChat size={15} />;
    case "loop":
      return <IconRepeat size={15} />;
    case "recent":
      return <Clock />;
  }
}

/** The 28px tile that leads a row: an org's avatar for its things, a glyph for the rest. */
export function Tile({ icon, org, tone }: { icon: IconName; org?: string; tone?: Result["tone"] }): ReactElement {
  const tint =
    tone === "err" ? "bg-err-soft text-err" : tone === "warn" ? "bg-warn-soft text-warn" : tone === "ok" ? "bg-ok-soft text-ok" : tone === "accent" ? "bg-accent-soft text-accent" : "bg-panel-3 text-muted";
  return (
    <span aria-hidden="true" className={cx("spot-tile grid size-8 shrink-0 place-items-center rounded-[10px]", icon === "colonize" ? "ant-glyph-host bg-accent-soft text-accent" : tint)}>
      {org && (icon === "repo" || icon === "org") ? <Avatar name={org} size={22} rounded="md" /> : <Glyph name={icon} />}
    </span>
  );
}

/** A title with the words the person typed picked out. */
export function Highlight({ text, words }: { text: string; words: readonly string[] }): ReactElement {
  const live = words.filter((w) => w.length > 0);
  if (live.length === 0) return <>{text}</>;
  const rx = new RegExp(`(${live.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`, "ig");
  const parts = text.split(rx);
  return (
    <>
      {parts.map((part, i) =>
        i % 2 === 1 ? (
          <mark key={i} className="bg-transparent font-semibold text-inherit underline decoration-current/30 decoration-1 underline-offset-[3px]">
            {part}
          </mark>
        ) : (
          <Fragment key={i}>{part}</Fragment>
        ),
      )}
    </>
  );
}

export function Key({ children, className }: { children: ReactNode; className?: string }): ReactElement {
  return <kbd className={cx("spot-key", className)}>{children}</kbd>;
}

/** One result. The selected row is the solid accent one, as Spotlight's is blue. */
export function ResultRow({
  result,
  words,
  selected,
  top,
  id,
  onPick,
  onHover,
}: {
  result: Result;
  words: readonly string[];
  selected: boolean;
  top: boolean;
  id: string;
  onPick: () => void;
  onHover: () => void;
}): ReactElement {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (selected) ref.current?.scrollIntoView?.({ block: "nearest" });
  }, [selected]);
  return (
    <div
      ref={ref}
      id={id}
      role="option"
      aria-selected={selected}
      data-section={result.section}
      onMouseMove={onHover}
      onClick={onPick}
      className="spot-row flex cursor-pointer items-center gap-3 rounded-xl px-2.5 py-2"
    >
      <Tile icon={result.icon} org={result.org} tone={result.tone} />
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[0.9375rem] font-medium leading-tight">
          <Highlight text={result.title} words={result.section === "ask" ? [] : words} />
        </span>
        {result.subtitle && <span className="spot-sub mt-0.5 block truncate text-small-lg leading-tight text-muted">{result.subtitle}</span>}
      </span>
      <span className="spot-hint flex shrink-0 items-center gap-2 text-small text-faint">
        {result.writes && <span title="Held for your approval">needs approval</span>}
        {top && !selected ? <span className="text-accent">Top hit</span> : result.hint && !result.writes ? <span className="max-sm:hidden">{result.hint}</span> : null}
        {selected && <Key>↵</Key>}
      </span>
    </div>
  );
}

/** The slim "Ask or search…" pill that sits in the middle of the top bar. */
export function SpotlightPill({ onOpen }: { onOpen: () => void }): ReactElement {
  return (
    <button
      type="button"
      onClick={onOpen}
      aria-label="Ask or search"
      aria-haspopup="dialog"
      aria-keyshortcuts="Meta+K Control+K /"
      title={`Ask or search · ${MOD}K or /`}
      className="group/pill flex h-8 w-full min-w-0 max-w-[420px] cursor-pointer items-center gap-2 rounded-full border border-border bg-panel-2/70 px-3 text-left text-small-lg text-faint shadow-[inset_0_1px_0_rgb(255_255_255/0.04)] transition-[background-color,border-color,box-shadow] hover:border-border-strong hover:bg-panel-2 hover:text-muted focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent max-sm:hidden"
    >
      <IconSearch size={14} className="shrink-0" />
      <span className="min-w-0 flex-1 truncate">Ask or search…</span>
      <span className="flex shrink-0 gap-1 opacity-80 transition-opacity group-hover/pill:opacity-100">
        <Key>{MOD}</Key>
        <Key>K</Key>
      </span>
    </button>
  );
}

/** The phone's floating search and ask button, above the tab bar. */
export function SpotlightFab({ onOpen }: { onOpen: () => void }): ReactElement {
  return (
    <button
      type="button"
      onClick={onOpen}
      aria-label="Ask or search"
      aria-haspopup="dialog"
      className="spot-fab fixed bottom-[calc(4.75rem+env(safe-area-inset-bottom))] right-4 z-30 grid size-12 cursor-pointer place-items-center rounded-full border-0 bg-accent text-on-accent transition-transform active:scale-95 sm:hidden"
    >
      <IconSearch size={20} />
    </button>
  );
}
