// SpotlightPanel (issue #1228): the one surface every menu in the cockpit is built on. Spotlight
// (#1218) set the standard (a frosted panel, large type, search first, keyboard first, sections with
// small headers, a solid accent selection row, a footer of key hints), and this module is that
// standard as a component, so the model menu, Colonize, the workspace switcher, the notifications
// and the Ask panel all look and behave the same:
//
// - `placement="centered"` is Spotlight's own place, over the page; `"anchored"` opens under (or over)
//   the control that opened it. On a phone either becomes a bottom sheet with the same content.
// - Esc closes (or steps back, through `onEscape`), ↑ and ↓ move the selection (wrapping), ⇥ jumps to
//   the next section, ↵ picks the selected row. Focus returns to whatever had it before.
//
// The pieces (`PanelFrame`, `PanelInput`, `PanelList`, `PanelFooter`) are exported too, because
// Spotlight composes them around its own ask and approval modes.
import {
  Fragment,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactElement,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";

import { IconSearch } from "../../components/icons";
import { Spinner, cx } from "../../components/ui";
import { navAction, sectionIndex, startIndex, stepIndex } from "./panelNav";
import { Highlight, Key } from "./Parts";
import "./spotlight.css";

/** One row of a panel. */
export interface PanelRow {
  id: string;
  title: ReactNode;
  subtitle?: ReactNode;
  /** The tile or glyph that leads the row. */
  leading?: ReactNode;
  /** What sits at the right: a count, a badge, a health dot. */
  trailing?: ReactNode;
  /** A short word at the right while the row is not selected ("Top hit", "needs approval"). */
  hint?: ReactNode;
  /** Small buttons that show on hover or selection (a row's own actions). */
  actions?: ReactNode;
  /** What ↵ does, for the footer ("open", "switch"). */
  verb?: string;
  /** Shown but not selectable: an out-of-quota model, an epic. */
  disabled?: boolean;
  /** The current choice: a check at the right. */
  checked?: boolean;
  /** A click (or a tap) only selects the row, whose own buttons then act: for rows whose action costs something. ↵ still picks. */
  clickSelects?: boolean;
  /** The solid primary action of a panel (Colonize's "Draft issues"). */
  primary?: boolean;
  ariaLabel?: string;
  onPick: () => void;
}

export interface PanelSection {
  id: string;
  title?: ReactNode;
  /** Right-aligned beside the section's header ("3 of 12", a small link). */
  aside?: ReactNode;
  rows: PanelRow[];
}

export interface PanelHint {
  keys: ReactNode[];
  label: string;
}

/** True below the `sm` breakpoint, where every panel becomes a bottom sheet. */
export function usePhone(): boolean {
  const query = "(max-width: 639px)";
  const [phone, setPhone] = useState<boolean>(() => typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia(query).matches);
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const list = window.matchMedia(query);
    const on = () => setPhone(list.matches);
    on();
    list.addEventListener("change", on);
    return () => list.removeEventListener("change", on);
  }, []);
  return phone;
}

/** The frame's position beside its anchor, measured when it opens and whenever the window changes. */
function useAnchorBox(anchor: RefObject<HTMLElement | null> | undefined, enabled: boolean): DOMRect | null {
  const [box, setBox] = useState<DOMRect | null>(null);
  useLayoutEffect(() => {
    if (!enabled || !anchor) return;
    const measure = () => setBox(anchor.current?.getBoundingClientRect() ?? null);
    measure();
    window.addEventListener("resize", measure);
    window.addEventListener("scroll", measure, true);
    return () => {
      window.removeEventListener("resize", measure);
      window.removeEventListener("scroll", measure, true);
    };
  }, [anchor, enabled]);
  return box;
}

export interface FrameProps {
  label: string;
  placement: "centered" | "anchored";
  anchor?: RefObject<HTMLElement | null>;
  /** Which edge of the anchor the panel lines up with. */
  align?: "start" | "end";
  /** Anchored panels open under their anchor, or over it (Ask's, from its button at the foot of the page). */
  side?: "below" | "above";
  /** The desktop width in px. */
  width?: number;
  onClose: () => void;
  /** Esc: return true when the panel stepped back instead of closing. */
  onEscape?: () => boolean;
  testId?: string;
  children: ReactNode;
  /** Centred panels on a phone sit at the top (Spotlight) rather than as a sheet. */
  sheet?: boolean;
  /** A docked panel (Ask): the page behind stays usable, and a click outside does not close it. A phone sheet still dims. */
  modeless?: boolean;
}

/** Backdrop, placement and the solid surface. Everything else lives inside it. */
export function PanelFrame({ label, placement, anchor, align = "end", side = "below", width, onClose, onEscape, testId, children, sheet = true, modeless = false }: FrameProps): ReactElement {
  const phone = usePhone();
  const asSheet = sheet && phone;
  const box = useAnchorBox(anchor, placement === "anchored" && !asSheet);
  const opener = useRef<Element | null>(typeof document === "undefined" ? null : document.activeElement);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const escapeRef = useRef(onEscape);
  escapeRef.current = onEscape;

  // Esc from anywhere in the panel, including a focused button; focus goes back where it came from.
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      e.preventDefault();
      if (escapeRef.current?.()) return;
      closeRef.current();
    };
    document.addEventListener("keydown", onKey);
    const from = opener.current;
    return () => {
      document.removeEventListener("keydown", onKey);
      // Only when nothing else took focus meanwhile: a development remount must not steal it back.
      if (from instanceof HTMLElement && from.isConnected)
        setTimeout(() => {
          const now = document.activeElement;
          if (!now || now === document.body || !now.isConnected) from.focus?.();
        }, 0);
    };
  }, []);

  const w = width ?? (placement === "centered" ? 680 : 440);
  let place: ReactElement;
  if (asSheet) {
    place = (
      <div className="pointer-events-none absolute inset-x-0 bottom-0 flex justify-center">
        <div data-sheet="true" className="spot-panel spot-sheet pointer-events-auto flex max-h-[min(86dvh,720px)] w-full flex-col overflow-hidden rounded-t-[22px] pb-[env(safe-area-inset-bottom)] text-text">
          <div aria-hidden="true" className="mx-auto mt-2 h-1 w-9 shrink-0 rounded-full bg-border-strong" />
          {children}
        </div>
      </div>
    );
  } else if (placement === "centered") {
    place = (
      <div className="pointer-events-none absolute inset-x-0 top-0 flex justify-center px-2 pt-[max(0.5rem,env(safe-area-inset-top))] sm:px-4 sm:pt-[11vh]">
        <div style={{ maxWidth: w }} className="spot-panel pointer-events-auto flex max-h-[calc(100dvh-1rem)] w-full flex-col overflow-hidden rounded-[22px] text-text sm:max-h-[78vh]">
          {children}
        </div>
      </div>
    );
  } else {
    const left = box ? (align === "start" ? Math.max(8, Math.min(box.left, (typeof window === "undefined" ? 1280 : window.innerWidth) - w - 8)) : Math.max(8, box.right - w)) : 8;
    const style =
      side === "below"
        ? { left, top: (box?.bottom ?? 48) + 8, width: w, maxHeight: `min(640px, calc(100dvh - ${(box?.bottom ?? 48) + 20}px))` }
        : { left, bottom: (typeof window === "undefined" ? 800 : window.innerHeight) - (box?.top ?? (typeof window === "undefined" ? 800 : window.innerHeight)) + 8, width: w, maxHeight: `min(640px, calc(${box?.top ?? 400}px - 20px))` };
    place = (
      <div style={style} className={cx("spot-panel spot-anchored pointer-events-auto absolute flex max-w-[calc(100vw-16px)] flex-col overflow-hidden rounded-[18px] text-text", side === "above" && "spot-above")}>
        {children}
      </div>
    );
  }

  const body = (
    <div className={cx("fixed inset-0 z-[70]", modeless && !asSheet && "pointer-events-none")} data-testid={testId}>
      {/* Anchored panels on a desktop dim nothing: a clear catcher closes them, as a menu does. */}
      <div
        aria-hidden="true"
        className={cx("absolute inset-0", (placement === "centered" || asSheet) && "spot-backdrop", modeless && !asSheet && "hidden")}
        onMouseDown={(e) => {
          e.preventDefault();
          onClose();
        }}
      />
      <div role="dialog" aria-modal={modeless && !asSheet ? undefined : "true"} aria-label={label} className="pointer-events-none absolute inset-0">
        {place}
      </div>
    </div>
  );
  return typeof document === "undefined" ? body : createPortal(body, document.body);
}

/** The search row: a glyph, a large field, whatever sits at its end, and the esc key. */
export function PanelInput({
  value,
  onChange,
  placeholder,
  label,
  icon,
  end,
  busy,
  onKeyDown,
  inputRef,
  listId,
  activeId,
  expanded = true,
  multiline = false,
  onClose,
  disabled,
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
  label: string;
  icon?: ReactNode;
  end?: ReactNode;
  busy?: boolean;
  onKeyDown?: (e: ReactKeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) => void;
  inputRef?: RefObject<HTMLInputElement | HTMLTextAreaElement | null>;
  listId?: string;
  activeId?: string;
  expanded?: boolean;
  multiline?: boolean;
  onClose: () => void;
  disabled?: boolean;
}): ReactElement {
  const local = useRef<HTMLInputElement | HTMLTextAreaElement | null>(null);
  const ref = inputRef ?? local;
  useEffect(() => {
    ref.current?.focus();
    if (!multiline) (ref.current as HTMLInputElement | null)?.select?.();
    // Focus once, on open.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // A multi-line field grows with what is in it, a few lines at most.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!multiline || !el) return;
    el.style.height = "0px";
    el.style.height = `${Math.min(el.scrollHeight, 132)}px`;
  }, [value, multiline, ref]);
  const common = {
    "aria-label": label,
    autoComplete: "off",
    autoCorrect: "off",
    spellCheck: false,
    value,
    placeholder,
    disabled,
    onKeyDown,
    className: "spot-input bare-field min-w-0 flex-1 border-0 bg-transparent p-0 text-text outline-none focus-visible:outline-none",
  } as const;
  return (
    <div className="flex shrink-0 items-start gap-3 px-4 py-3.5 sm:px-5 sm:py-4">
      <span aria-hidden="true" className={cx("grid size-6 shrink-0 place-items-center text-muted", multiline ? "mt-px" : "self-center")}>
        {icon ?? <IconSearch size={20} />}
      </span>
      {multiline ? (
        <textarea ref={ref as RefObject<HTMLTextAreaElement>} rows={1} onChange={(e) => onChange(e.target.value)} {...common} className={cx(common.className, "resize-none leading-snug")} />
      ) : (
        <input
          ref={ref as RefObject<HTMLInputElement>}
          role="combobox"
          aria-expanded={expanded}
          aria-controls={listId}
          aria-activedescendant={activeId}
          aria-autocomplete="list"
          enterKeyHint="go"
          onChange={(e) => onChange(e.target.value)}
          {...common}
          className={cx(common.className, "self-center")}
        />
      )}
      {busy && <Spinner className="mt-1 size-3.5 shrink-0 text-faint" />}
      {end}
      <button type="button" onClick={onClose} aria-label="close" className="shrink-0 cursor-pointer self-center border-0 bg-transparent p-0">
        <Key>esc</Key>
      </button>
    </div>
  );
}

/** A status dot in the tile that leads a row, the way Spotlight leads every result. */
export function DotTile({ color, className }: { color: string; className?: string }): ReactElement {
  return (
    <span aria-hidden="true" className={cx("spot-tile grid size-8 shrink-0 place-items-center rounded-[10px] bg-panel-3", className)}>
      <span className="size-2.5 rounded-full" style={{ background: color }} />
    </span>
  );
}

/** One row, as Spotlight draws every result: a tile, a title, a subtitle, a hint, the key. */
export function PanelRowView({
  row,
  id,
  selected,
  words = [],
  onHover,
}: {
  row: PanelRow;
  id: string;
  selected: boolean;
  words?: readonly string[];
  onHover?: () => void;
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
      aria-disabled={row.disabled || undefined}
      aria-label={row.ariaLabel}
      data-primary={row.primary || undefined}
      data-checked={row.checked || undefined}
      onMouseMove={row.disabled ? undefined : onHover}
      onClick={row.disabled ? undefined : row.clickSelects ? onHover : row.onPick}
      className={cx("spot-row group/row flex items-center gap-3 rounded-xl px-2.5 py-2", row.disabled ? "cursor-not-allowed opacity-55" : "cursor-pointer", row.primary && !selected && "spot-row-primary")}
    >
      {row.leading}
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[0.9375rem] font-medium leading-tight">{typeof row.title === "string" ? <Highlight text={row.title} words={words} /> : row.title}</span>
        {row.subtitle && <span className="spot-sub mt-0.5 block truncate text-small-lg leading-tight text-muted">{row.subtitle}</span>}
      </span>
      {row.actions && <span className="spot-actions flex shrink-0 items-center gap-1" onClick={(e) => e.stopPropagation()}>{row.actions}</span>}
      <span className="spot-hint flex shrink-0 items-center gap-2 text-small text-faint">
        {row.trailing}
        {!selected && row.hint ? <span className="max-sm:hidden">{row.hint}</span> : null}
        {row.checked && <CheckMark />}
        {selected && !row.disabled && <Key>↵</Key>}
      </span>
    </div>
  );
}

function CheckMark(): ReactElement {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-label="current" className="spot-check shrink-0 text-accent">
      <path d="m5 12.5 4.5 4.5L19 7.5" />
    </svg>
  );
}

/** The sections and their rows. */
export function PanelList({
  listId,
  sections,
  selectedId,
  words,
  onHover,
  empty,
  className,
}: {
  listId: string;
  sections: readonly PanelSection[];
  selectedId: string | undefined;
  words?: readonly string[];
  onHover: (id: string) => void;
  empty?: ReactNode;
  className?: string;
}): ReactElement {
  const index = new Map<string, number>();
  sections.forEach((s) => s.rows.forEach((r) => index.set(r.id, index.size)));
  const any = sections.some((s) => s.rows.length > 0);
  return (
    <div id={listId} role="listbox" aria-label="results" className={cx("scroll-thin min-h-0 flex-1 overflow-y-auto border-t border-border px-2 pb-2 pt-1 sm:max-h-[min(54vh,470px)]", className)}>
      {sections
        .filter((s) => s.rows.length > 0)
        .map((section) => (
          <div key={section.id} role="group" aria-label={typeof section.title === "string" ? section.title : section.id} className="spot-section">
            {(section.title || section.aside) && (
              <div className="flex items-center gap-2 px-2.5 pb-1 pt-2.5 text-meta-lg font-semibold uppercase tracking-[0.07em] text-faint">
                <span className="min-w-0 flex-1 truncate">{section.title}</span>
                {section.aside && <span className="shrink-0 font-normal normal-case tracking-normal">{section.aside}</span>}
              </div>
            )}
            {section.rows.map((row) => (
              <Fragment key={row.id}>
                <PanelRowView row={row} id={`${listId}-${index.get(row.id)}`} selected={row.id === selectedId} words={words} onHover={() => onHover(row.id)} />
              </Fragment>
            ))}
          </div>
        ))}
      {!any && empty && <div className="px-3 py-6 text-center text-body-sm text-muted">{empty}</div>}
    </div>
  );
}

/** The foot: the keys that work now, and whatever else the panel keeps there. */
export function PanelFooter({ hints, end }: { hints: readonly PanelHint[]; end?: ReactNode }): ReactElement {
  return (
    <footer className="spot-footer flex shrink-0 items-center gap-4 border-t border-border bg-panel-2/40 px-4 py-2 text-small text-faint">
      <span className="flex min-w-0 items-center gap-4 overflow-hidden max-sm:hidden">
        {hints.map((h) => (
          <span key={h.label} className="inline-flex items-center gap-1.5 whitespace-nowrap">
            {h.keys.map((k, i) => (
              <Key key={i}>{k}</Key>
            ))}{" "}
            {h.label}
          </span>
        ))}
      </span>
      <span className="ml-auto flex shrink-0 items-center gap-3 whitespace-nowrap max-sm:w-full max-sm:justify-between">{end ?? "Colonizer"}</span>
    </footer>
  );
}

/** Selection state for a list of sections: the flat order, the selected row, and the keys. */
export function usePanelNav(sections: readonly PanelSection[], resetOn: unknown, autoSelect = true) {
  const flat = useMemo(() => sections.flatMap((s) => s.rows.filter((r) => !r.disabled).map((r) => ({ row: r, section: s.id }))), [sections]);
  const [pickedId, setPickedId] = useState<string | null>(null);
  useEffect(() => setPickedId(null), [resetOn]);
  const found = pickedId === null ? -1 : flat.findIndex((f) => f.row.id === pickedId);
  const at = found >= 0 ? found : startIndex(flat.map((f) => f.row), autoSelect);
  const selected = flat[at]?.row;
  const sectionCount = new Set(flat.map((f) => f.section)).size;
  /** ↑ ↓ ⇥ ↵ for any panel's field. Returns true when it handled the key. */
  const onKey = useCallback(
    (e: ReactKeyboardEvent): boolean => {
      const action = navAction({ key: e.key, shiftKey: e.shiftKey, isComposing: e.nativeEvent.isComposing }, flat.length, sectionCount, selected !== undefined);
      if (!action) return false;
      e.preventDefault();
      if (action.type === "move") setPickedId(flat[stepIndex(at, flat.length, action.delta)]?.row.id ?? null);
      else if (action.type === "section") setPickedId(flat[sectionIndex(flat.map((f) => f.section), at, action.back)]?.row.id ?? null);
      else selected?.onPick();
      return true;
    },
    [flat, at, sectionCount, selected],
  );
  return { selected, selectedId: selected?.id, setPickedId, onKey, sectionCount };
}

export interface SpotlightPanelProps extends Omit<FrameProps, "children"> {
  query: string;
  onQuery: (query: string) => void;
  placeholder: string;
  /** The field's accessible name; defaults to the placeholder. */
  fieldLabel?: string;
  icon?: ReactNode;
  /** At the end of the search row, before esc: a scope chip, a mic. */
  headerEnd?: ReactNode;
  multiline?: boolean;
  inputRef?: RefObject<HTMLInputElement | HTMLTextAreaElement | null>;
  /** Under the search row: chips and fields that belong to the whole panel. */
  below?: ReactNode;
  sections: readonly PanelSection[];
  empty?: ReactNode;
  loading?: boolean;
  /** Between the list and the footer: an apply card, an error, a confirm step. */
  dock?: ReactNode;
  /** Replaces the list (an approval card, an answer). */
  body?: ReactNode;
  /** Keys shown after the defaults. */
  extraHints?: readonly PanelHint[];
  /** Replaces the footer's right-hand "Colonizer". */
  footerEnd?: ReactNode;
  /** Runs before the shared keys; return true to stop them. */
  onKeyDown?: (e: ReactKeyboardEvent<HTMLInputElement | HTMLTextAreaElement>, selected: PanelRow | undefined) => boolean | void;
  /** Re-selects the first row when this changes (defaults to the query). */
  resetOn?: unknown;
  /** False: nothing is selected until an arrow key or the pointer picks a row, so ↵ cannot start anything unasked. */
  autoSelect?: boolean;
  words?: readonly string[];
  /** Where the list may grow to. */
  listClassName?: string;
}

/** A search field over sections of rows, with the footer of keys: the whole panel. */
export function SpotlightPanel(p: SpotlightPanelProps): ReactElement {
  const listId = useId();
  const nav = usePanelNav(p.sections, p.resetOn ?? p.query, p.autoSelect ?? true);
  const words = p.words ?? p.query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const showList = p.body === undefined;
  const hints: PanelHint[] = [
    { keys: ["↑", "↓"], label: "select" },
    ...(nav.selected ? [{ keys: ["↵"], label: nav.selected.verb ?? "open" }] : []),
    ...(nav.sectionCount > 1 ? [{ keys: ["⇥"], label: "section" }] : []),
    ...(p.extraHints ?? []),
  ];
  const index = p.sections.flatMap((s) => s.rows).findIndex((r) => r.id === nav.selectedId);
  return (
    <PanelFrame label={p.label} placement={p.placement} anchor={p.anchor} align={p.align} side={p.side} width={p.width} sheet={p.sheet} onClose={p.onClose} onEscape={p.onEscape} testId={p.testId}>
      <PanelInput
        value={p.query}
        onChange={p.onQuery}
        placeholder={p.placeholder}
        label={p.fieldLabel ?? p.placeholder.replace(/…$/, "")}
        icon={p.icon}
        end={p.headerEnd}
        busy={p.loading}
        multiline={p.multiline}
        inputRef={p.inputRef}
        listId={listId}
        activeId={showList && index >= 0 ? `${listId}-${index}` : undefined}
        expanded={showList}
        onClose={p.onClose}
        onKeyDown={(e) => {
          if (p.onKeyDown?.(e, nav.selected) === true) return;
          nav.onKey(e);
        }}
      />
      {p.below}
      {showList ? (
        <PanelList listId={listId} sections={p.sections} selectedId={nav.selectedId} words={words} onHover={nav.setPickedId} empty={p.empty} className={p.listClassName} />
      ) : (
        p.body
      )}
      {p.dock}
      <PanelFooter hints={hints} end={p.footerEnd} />
    </PanelFrame>
  );
}
