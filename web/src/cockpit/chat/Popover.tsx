// Popovers for the Chat view: a floating panel anchored to a trigger, and a searchable,
// keyboard-driven list inside it. They stand in for native <select>s, which cannot show marks,
// status dots or hints, and cannot be searched.
import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactElement,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import { IconCheck, IconChevronDown, IconSearch } from "../../components/icons";
import { cx } from "../../components/ui";

type Placement = "bottom-start" | "bottom-end" | "top-start" | "top-end";

/** A panel floating by `anchor`, flipped above or below to fit; closes on Escape or a click outside. */
export function Popover({
  open,
  onClose,
  anchor,
  placement = "bottom-start",
  width = 320,
  label,
  children,
}: {
  open: boolean;
  onClose: () => void;
  anchor: RefObject<HTMLElement | null>;
  placement?: Placement;
  width?: number;
  label: string;
  children: ReactNode;
}): ReactElement | null {
  const panel = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ top?: number; bottom?: number; left: number; maxHeight: number } | null>(null);

  const place = useCallback(() => {
    const a = anchor.current?.getBoundingClientRect();
    if (!a) return;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const w = Math.min(width, vw - 16);
    const wantTop = placement.startsWith("top");
    const below = vh - a.bottom - 12;
    const above = a.top - 12;
    const top = wantTop ? above > 220 || above > below : !(below > 220 || below > above);
    let left = placement.endsWith("end") ? a.right - w : a.left;
    left = Math.max(8, Math.min(left, vw - w - 8));
    setPos(top ? { bottom: vh - a.top + 6, left, maxHeight: Math.min(480, above) } : { top: a.bottom + 6, left, maxHeight: Math.min(480, below) });
  }, [anchor, placement, width]);

  useLayoutEffect(() => {
    if (!open) return;
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open, place]);

  useEffect(() => {
    if (!open) return;
    const down = (e: MouseEvent) => {
      const t = e.target as Node;
      if (panel.current?.contains(t) || anchor.current?.contains(t)) return;
      onClose();
    };
    const key = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
        anchor.current?.focus();
      }
    };
    document.addEventListener("mousedown", down);
    document.addEventListener("keydown", key, true);
    return () => {
      document.removeEventListener("mousedown", down);
      document.removeEventListener("keydown", key, true);
    };
  }, [open, onClose, anchor]);

  if (!open || !pos) return null;
  return createPortal(
    <div
      ref={panel}
      role="dialog"
      aria-label={label}
      style={{ position: "fixed", top: pos.top, bottom: pos.bottom, left: pos.left, width: Math.min(width, window.innerWidth - 16), maxHeight: pos.maxHeight }}
      className="chat-pop z-50 flex flex-col overflow-hidden rounded-xl border border-border bg-panel text-text shadow-[0_12px_40px_-8px_rgba(0,0,0,0.45)]"
    >
      {children}
    </div>,
    document.body,
  );
}

export interface ListItem {
  id: string;
  label: string;
  group?: string;
  /** A second line, muted. */
  hint?: ReactNode;
  leading?: ReactNode;
  trailing?: ReactNode;
  /** Why it cannot be picked; shown instead of the hint. */
  disabled?: string | null;
  /** Extra words the search matches. */
  keywords?: string;
}

/** Items whose label, group, id or keywords contain every typed word. Pure, for the tests. */
export function filterItems<T extends ListItem>(items: readonly T[], query: string): T[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return [...items];
  return items.filter((i) => {
    const hay = `${i.label} ${i.group ?? ""} ${i.id} ${i.keywords ?? ""}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

/** A searchable list: type to filter, ↑/↓ to move, Enter to pick. */
export function SearchList<T extends ListItem>({
  items,
  onPick,
  selected,
  placeholder = "Search…",
  emptyText = "Nothing matches.",
  searchable = true,
  header,
  footer,
  loading,
  onQueryChange,
}: {
  items: readonly T[];
  onPick: (item: T) => void;
  selected?: string | null;
  placeholder?: string;
  emptyText?: string;
  searchable?: boolean;
  header?: ReactNode;
  footer?: ReactNode;
  loading?: boolean;
  /** The caller filters: it gets every keystroke and passes the items to show. */
  onQueryChange?: (query: string) => void;
}): ReactElement {
  const [query, setQuery] = useState("");
  const shown = useMemo(() => (onQueryChange ? [...items] : filterItems(items, query)), [items, query, onQueryChange]);
  const firstEnabled = shown.findIndex((i) => !i.disabled);
  const [active, setActive] = useState(() => Math.max(0, shown.findIndex((i) => i.id === selected)));
  const list = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const base = useId();

  useEffect(() => {
    setActive(query ? Math.max(0, firstEnabled) : Math.max(0, shown.findIndex((i) => i.id === selected), firstEnabled));
    // Only when the filter changes: moving with the arrows must not snap back.
  }, [query, items.length, items[0]?.id]);

  useEffect(() => {
    (searchable ? input.current : list.current)?.focus();
  }, [searchable]);

  useEffect(() => {
    list.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const move = (delta: number) => {
    if (shown.length === 0) return;
    let i = active;
    for (let n = 0; n < shown.length; n++) {
      i = (i + delta + shown.length) % shown.length;
      if (!shown[i].disabled) break;
    }
    setActive(i);
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      move(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      move(-1);
    } else if (e.key === "Home") {
      e.preventDefault();
      setActive(Math.max(0, firstEnabled));
    } else if (e.key === "End") {
      e.preventDefault();
      setActive(shown.length - 1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      const item = shown[active];
      if (item && !item.disabled) onPick(item);
    }
  };

  let lastGroup: string | undefined;
  return (
    <div className="flex min-h-0 flex-1 flex-col" onKeyDown={onKey}>
      {header}
      {searchable && (
        <div className="flex items-center gap-2 border-b border-border px-3 py-2">
          <IconSearch size={14} className="text-faint" />
          <input
            ref={input}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              onQueryChange?.(e.target.value);
            }}
            placeholder={placeholder}
            aria-label={placeholder}
            role="combobox"
            aria-expanded="true"
            aria-controls={`${base}-list`}
            aria-activedescendant={shown[active] ? `${base}-${active}` : undefined}
            className="min-w-0 flex-1 border-0 bg-transparent text-[13px] text-text outline-none placeholder:text-faint"
          />
        </div>
      )}
      <div ref={list} id={`${base}-list`} role="listbox" tabIndex={searchable ? -1 : 0} className="scroll-thin min-h-0 flex-1 overflow-y-auto p-1 outline-none">
        {loading && <div className="px-3 py-3 text-[12.5px] text-faint">Loading…</div>}
        {!loading && shown.length === 0 && <div className="px-3 py-3 text-[12.5px] text-faint">{emptyText}</div>}
        {shown.map((item, i) => {
          const groupRow = item.group && item.group !== lastGroup ? item.group : null;
          lastGroup = item.group;
          return (
            <div key={item.id}>
              {groupRow && <div className="px-2.5 pb-1 pt-2 text-[10.5px] font-semibold uppercase tracking-wider text-faint">{groupRow}</div>}
              <div
                id={`${base}-${i}`}
                data-index={i}
                role="option"
                aria-selected={item.id === selected}
                aria-disabled={Boolean(item.disabled)}
                onMouseMove={() => !item.disabled && setActive(i)}
                onClick={() => !item.disabled && onPick(item)}
                className={cx(
                  "flex cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-1.5",
                  i === active && !item.disabled && "bg-panel-2",
                  item.disabled && "cursor-not-allowed opacity-55",
                )}
              >
                {item.leading}
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[13px]">{item.label}</div>
                  {(item.disabled || item.hint) && <div className="truncate text-[11.5px] text-faint">{item.disabled || item.hint}</div>}
                </div>
                {item.trailing}
                {item.id === selected && <IconCheck size={14} className="shrink-0 text-accent" />}
              </div>
            </div>
          );
        })}
      </div>
      {footer}
    </div>
  );
}

/** A custom select: a button showing the choice, opening a searchable list. */
export function Select<T extends ListItem>({
  value,
  items,
  onChange,
  ariaLabel,
  placeholder = "Choose…",
  width = 320,
  className,
  searchable = true,
  renderValue,
}: {
  value: string | null;
  items: readonly T[];
  onChange: (item: T) => void;
  ariaLabel: string;
  placeholder?: string;
  width?: number;
  className?: string;
  searchable?: boolean;
  renderValue?: (item: T | undefined) => ReactNode;
}): ReactElement {
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const current = items.find((i) => i.id === value);
  return (
    <>
      <button
        ref={button}
        type="button"
        aria-label={ariaLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" && !open) {
            e.preventDefault();
            setOpen(true);
          }
        }}
        className={cx(
          "flex min-w-0 cursor-pointer items-center gap-2 rounded-lg border border-border bg-panel px-2.5 py-1.5 text-left text-[13px] text-text hover:border-border-strong focus-visible:border-accent focus-visible:outline-none",
          className,
        )}
      >
        {renderValue ? (
          renderValue(current)
        ) : (
          <>
            {current?.leading}
            <span className={cx("min-w-0 flex-1 truncate", !current && "text-faint")}>{current?.label ?? placeholder}</span>
          </>
        )}
        <IconChevronDown size={14} className="shrink-0 text-faint" />
      </button>
      <Popover open={open} onClose={() => setOpen(false)} anchor={button} width={width} label={ariaLabel}>
        <SearchList
          items={items}
          selected={value}
          searchable={searchable && items.length > 6}
          onPick={(item) => {
            onChange(item);
            setOpen(false);
            button.current?.focus();
          }}
        />
      </Popover>
    </>
  );
}
