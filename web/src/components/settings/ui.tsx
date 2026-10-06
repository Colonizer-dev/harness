import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { GuideIcon, guideFor } from "../settingsGuide";
import { IconChevron } from "../icons";
import { Badge, InfoButton, Spinner, cx, type Tone } from "../ui";

// ---------------------------------------------------------------------------
// The shared pieces of every settings page: the section list, the pane frame and
// the row layout. The section ids the whole screen is keyed by live here too.
// ---------------------------------------------------------------------------

export type SectionId = "cockpit" | "setup" | "connections" | "providers" | "runtime" | "live-map" | "remote" | "phone" | "tokens" | "fleet" | "updates" | "usage" | "notifications" | "desktop" | "built-with" | `module:${string}` | `org:${string}`;

const PANE_TITLE_ID = "settings-pane-title";

/** The page's hero card (settingsGuide.tsx), which every Pane shows at the top of its body. */
export const HeroContext = createContext<ReactNode>(null);

type NavItem = {
  id: SectionId;
  label: string;
  hint?: string;
  /** A coloured dot; `toneText` is what a screen reader hears for it. */
  tone?: Tone | null;
  toneText?: string;
  badge?: string;
  dirty?: boolean;
};

export type NavGroup = { label: string; items: NavItem[]; loading?: boolean; error?: string | null };

export function SectionNav({
  layout,
  groups,
  active,
  onSelect,
  initialFocus,
}: {
  layout: "side" | "list";
  groups: NavGroup[];
  active: SectionId | null;
  onSelect: (id: SectionId) => void;
  /** Focus this item when the list appears (the section a narrow window just left). */
  initialFocus?: SectionId;
}) {
  const refs = useRef<Partial<Record<SectionId, HTMLButtonElement | null>>>({});
  const flat = groups.flatMap((g) => g.items);
  const side = layout === "side";

  useEffect(() => {
    if (initialFocus) refs.current[initialFocus]?.focus();
  }, [initialFocus]);

  // In the side layout the list is one Tab stop: arrows move between sections.
  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (!side || !["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
    const index = flat.findIndex((item) => item.id === active);
    let next = index;
    if (e.key === "ArrowDown") next = (index + 1) % flat.length;
    else if (e.key === "ArrowUp") next = (index - 1 + flat.length) % flat.length;
    else if (e.key === "Home") next = 0;
    else next = flat.length - 1;
    const target = flat[next];
    if (!target) return;
    e.preventDefault();
    onSelect(target.id);
    refs.current[target.id]?.focus();
  };

  return (
    <nav
      aria-label="Settings sections"
      onKeyDown={onKeyDown}
      className={cx(
        "scroll-thin min-h-0 overflow-y-auto",
        side ? "w-[200px] shrink-0 space-y-4 border-r border-border px-2.5 py-3" : "flex-1 space-y-4 px-5 py-3",
      )}
    >
      {groups.map((group) => (
        <div key={group.label}>
          <h3 className={cx("text-meta-lg font-semibold uppercase tracking-wide text-faint", side ? "mb-1 px-2.5" : "mb-1")}>{group.label}</h3>
          {group.loading && (
            <p className={cx("flex items-center gap-2 py-1.5 text-small-lg text-muted", side && "px-2.5")}>
              <Spinner className="size-3" /> Loading…
            </p>
          )}
          {group.error && <p className={cx("py-1.5 text-small-lg text-err", side && "px-2.5")}>{group.error}</p>}
          <ul className={side ? "space-y-0.5" : "divide-y divide-border overflow-hidden rounded-xl border border-border"}>
            {group.items.map((item) => {
              const current = item.id === active;
              return (
                <li key={item.id}>
                  <button
                    type="button"
                    ref={(el) => {
                      refs.current[item.id] = el;
                    }}
                    aria-current={current ? "true" : undefined}
                    tabIndex={side ? (current ? 0 : -1) : 0}
                    onClick={() => onSelect(item.id)}
                    className={cx(
                      "flex w-full cursor-pointer items-center gap-2 text-left",
                      side
                        ? cx("rounded-lg px-2.5 py-1.5 text-body-sm", current ? "bg-panel-2 font-medium text-text" : "text-muted hover:bg-panel-2 hover:text-text")
                        : "px-3.5 py-3 text-body hover:bg-panel-2",
                    )}
                  >
                    <span className="min-w-0 flex-1">
                      <span className="block truncate">{item.label}</span>
                      {!side && item.hint && <span className="block truncate text-small text-muted">{item.hint}</span>}
                    </span>
                    {item.dirty && (
                      <>
                        <span aria-hidden="true" className="size-1.5 shrink-0 rounded-full bg-accent" />
                        <span className="sr-only">unsaved changes</span>
                      </>
                    )}
                    {item.badge && <span className="shrink-0 text-meta-lg text-faint">{item.badge}</span>}
                    {item.tone && (
                      <>
                        <span
                          aria-hidden="true"
                          className={cx("size-2 shrink-0 rounded-full", item.tone === "ok" ? "bg-ok" : item.tone === "err" ? "bg-err" : "bg-warn")}
                        />
                        {item.toneText && <span className="sr-only">{item.toneText}</span>}
                      </>
                    )}
                    {!side && <IconChevron size={14} className="shrink-0 text-faint" />}
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </nav>
  );
}

/**
 * The wide layout's nav, across the top: the groups as tabs, and the chosen group's sections as
 * chips beneath, each with its icon. The chip row is one Tab stop; the arrows move along it.
 */
export function TopNav({ groups, active, onSelect }: { groups: NavGroup[]; active: SectionId | null; onSelect: (id: SectionId) => void }) {
  const owner = groups.find((g) => g.items.some((item) => item.id === active)) ?? groups[0];
  const [groupLabel, setGroupLabel] = useState(owner?.label);
  // Following the page: a section opened from elsewhere (a link, a deep link) brings its group along.
  const [lastActive, setLastActive] = useState(active);
  if (active !== lastActive) {
    setLastActive(active);
    if (owner) setGroupLabel(owner.label);
  }
  const group = groups.find((g) => g.label === groupLabel) ?? owner;
  const refs = useRef<Partial<Record<SectionId, HTMLButtonElement | null>>>({});
  const items = group?.items ?? [];

  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (!["ArrowRight", "ArrowLeft", "Home", "End"].includes(e.key) || items.length === 0) return;
    const index = Math.max(0, items.findIndex((item) => item.id === active));
    const next =
      e.key === "ArrowRight" ? (index + 1) % items.length
      : e.key === "ArrowLeft" ? (index - 1 + items.length) % items.length
      : e.key === "Home" ? 0
      : items.length - 1;
    e.preventDefault();
    onSelect(items[next].id);
    refs.current[items[next].id]?.focus();
  };

  return (
    <nav aria-label="Settings sections" className="shrink-0 border-b border-border">
      <div role="tablist" aria-label="Settings groups" className="page-pad flex gap-1 px-4 pt-2.5 [--pad-x:1rem]">
        {groups.map((g) => {
          const current = g.label === group?.label;
          return (
            <button
              key={g.label}
              type="button"
              role="tab"
              aria-selected={current}
              onClick={() => {
                setGroupLabel(g.label);
                if (g.items[0] && !g.items.some((item) => item.id === active)) onSelect(g.items[0].id);
              }}
              className={cx(
                "relative cursor-pointer rounded-md px-3 py-1.5 text-body-sm font-medium transition-colors",
                current ? "text-text" : "text-muted hover:bg-panel-2 hover:text-text",
              )}
            >
              {g.label}
              <span className="ml-1.5 text-meta-lg font-normal tabular-nums text-faint">{g.items.length || ""}</span>
              {current && <span aria-hidden="true" className="absolute inset-x-2 -bottom-[1px] h-0.5 rounded-full bg-accent" />}
            </button>
          );
        })}
      </div>
      <div className="border-t border-border">
        {group?.loading && (
          <p className="flex items-center gap-2 px-5 py-2.5 text-small-lg text-muted">
            <Spinner className="size-3" /> Loading…
          </p>
        )}
        {group?.error && <p className="px-5 py-2.5 text-small-lg text-err">{group.error}</p>}
        <ul onKeyDown={onKeyDown} className="page-pad scroll-thin flex gap-1.5 overflow-x-auto px-4 py-2.5 [--pad-x:1rem]">
          {items.map((item) => {
            const current = item.id === active;
            return (
              <li key={item.id} className="shrink-0">
                <button
                  type="button"
                  ref={(el) => {
                    refs.current[item.id] = el;
                  }}
                  aria-current={current ? "true" : undefined}
                  tabIndex={current || (!items.some((i) => i.id === active) && item === items[0]) ? 0 : -1}
                  title={item.hint}
                  onClick={() => onSelect(item.id)}
                  className={cx(
                    "flex cursor-pointer items-center gap-2 rounded-lg border px-3 py-1.5 text-body-sm transition-colors",
                    current ? "border-border-strong bg-panel-2 font-medium text-text" : "border-transparent text-muted hover:bg-panel-2 hover:text-text",
                  )}
                >
                  <GuideIcon name={guideFor(item.id).icon} size={15} className={current ? "text-accent" : undefined} />
                  <span className="whitespace-nowrap">{item.label}</span>
                  {item.dirty && (
                    <>
                      <span aria-hidden="true" className="size-1.5 shrink-0 rounded-full bg-accent" />
                      <span className="sr-only">unsaved changes</span>
                    </>
                  )}
                  {item.badge && <span className="text-meta-lg tabular-nums text-faint">{item.badge}</span>}
                  {item.tone && (
                    <>
                      <span
                        aria-hidden="true"
                        className={cx("size-2 shrink-0 rounded-full", item.tone === "ok" ? "bg-ok" : item.tone === "err" ? "bg-err" : "bg-warn")}
                      />
                      {item.toneText && <span className="sr-only">{item.toneText}</span>}
                    </>
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      </div>
    </nav>
  );
}

// ---------------------------------------------------------------------------
// Pane and row layout shared by every section
// ---------------------------------------------------------------------------

export function Pane({
  title,
  subtitle,
  info,
  aside,
  back,
  footer,
  children,
}: {
  title: string;
  subtitle?: string;
  /** Longer explanation, behind the "i" next to the title. */
  info?: ReactNode;
  aside?: ReactNode;
  back?: () => void;
  footer?: ReactNode;
  children: ReactNode;
}) {
  const titleRef = useRef<HTMLHeadingElement>(null);
  const hero = useContext(HeroContext);
  const stacked = Boolean(back);
  // In the narrow, back-navigable layout the pane replaces the list, so focus must follow.
  useEffect(() => {
    if (stacked) titleRef.current?.focus();
  }, [stacked]);

  return (
    <section aria-labelledby={PANE_TITLE_ID} className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="page-pad flex shrink-0 items-start gap-2 border-b border-border px-5 py-3.5">
        {back && (
          <button
            type="button"
            onClick={back}
            aria-label="Back to all settings"
            className="-ml-1.5 grid size-8 shrink-0 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
          >
            <IconChevron size={16} className="rotate-180" />
          </button>
        )}
        <div className="min-w-0 flex-1">
          <h3
            id={PANE_TITLE_ID}
            ref={titleRef}
            tabIndex={stacked ? -1 : undefined}
            className="flex items-center gap-1 rounded text-lead font-semibold leading-8"
          >
            {title}
            {info && <InfoButton label={title}>{info}</InfoButton>}
          </h3>
          {subtitle && <p className="-mt-1 text-small-lg text-muted">{subtitle}</p>}
        </div>
        {aside && <div className="flex shrink-0 items-center leading-8">{aside}</div>}
      </div>
      <div className="page-pad scroll-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        {hero}
        {children}
      </div>
      {footer && <div className="page-pad flex shrink-0 flex-wrap items-center gap-2 border-t border-border px-5 py-3">{footer}</div>}
    </section>
  );
}

/** One setting: label on the left, control on the right, explanation behind the "i". The label's id is `${id}-label`. */
export function Row({
  id,
  label,
  info,
  inline,
  children,
}: {
  id?: string;
  label: string;
  info?: ReactNode;
  /** For switches: keeps the control on the label's line at every width. */
  inline?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 py-3">
      <div className="min-w-0 flex-1 basis-40">
        <div className="flex items-center gap-1">
          <label id={id ? `${id}-label` : undefined} htmlFor={id} className="text-body font-medium">
            {label}
          </label>
          {info && <InfoButton label={label}>{info}</InfoButton>}
        </div>
      </div>
      <div className={cx("min-w-0", inline ? "flex shrink-0 justify-end sm:w-[260px]" : "w-full sm:w-[260px]")}>{children}</div>
    </div>
  );
}

export function Code({ children }: { children: ReactNode }) {
  return <code className="rounded bg-panel-3 px-1 font-mono text-meta-lg">{children}</code>;
}

// The account avatar beside the GitHub login row is the shared Avatar (`alt=""` — the login is
// already shown as text), on the same quiet tile as a provider mark.
export function ConnectionCard({
  name,
  mark,
  connected,
  detail,
  detailTone,
  info,
  children,
}: {
  name: string;
  /** Optional tile at the front of the header row, e.g. an account avatar. */
  mark?: ReactNode;
  connected: boolean | null;
  detail?: string;
  detailTone?: "err";
  info: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="rounded-xl border border-border">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 px-4 py-3">
        {mark}
        <span className="flex items-center gap-1 text-body-lg font-semibold">
          {name}
          <InfoButton label={name}>{info}</InfoButton>
        </span>
        {connected != null && <Badge tone={connected ? "ok" : "err"}>{connected ? "Connected" : "Not connected"}</Badge>}
        {detail && <span className={cx("text-small-lg [overflow-wrap:anywhere]", detailTone === "err" ? "text-err" : "text-muted")}>{detail}</span>}
      </div>
      <div className="space-y-3 border-t border-border px-4 py-3">{children}</div>
    </div>
  );
}
