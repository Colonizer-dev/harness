import { useEffect, useRef, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import { GuideIcon } from "../settingsGuide";
import { IconAlert, IconChevron, IconSearch, IconX } from "../icons";
import { Spinner, cx } from "../ui";
import { type Attention, type GroupInfo, type SearchHit } from "./nav";
import type { SectionId } from "./ui";

// ---------------------------------------------------------------------------
// The settings menu (issue #1180): one vertical list of groups, each with an icon, a one-line
// description and a status dot, its pages indented underneath; a search box on top that jumps to a
// field; an icon rail at medium widths; and a plain drill-down list on a phone. Everything is
// driven by the same `NavGroupModel`s, so the three shapes can never disagree about what exists.
// ---------------------------------------------------------------------------

export interface NavPage {
  id: SectionId;
  label: string;
  hint: string;
  /** Amber needs you, red is broken; nothing when all is well. */
  attention?: Attention | null;
  /** What a screen reader hears for the dot. */
  attentionText?: string;
  /** A short quiet fact on the right: "On", "v0.1.4", a count. */
  badge?: string;
  /** Unsaved changes in this page's form. */
  dirty?: boolean;
}

export interface NavGroupModel {
  info: GroupInfo;
  pages: NavPage[];
  /** The worst attention among the pages. */
  attention: Attention | null;
  loading?: boolean;
  error?: string | null;
}

const DOT: Record<Attention, string> = { warn: "bg-warn", err: "bg-err" };
const DOT_TEXT: Record<Attention, string> = { warn: "needs you", err: "broken" };

/** The small dot: only drawn when there is something to say. */
export function StatusDot({ attention, text, className }: { attention: Attention | null | undefined; text?: string; className?: string }) {
  if (!attention) return null;
  return (
    <>
      <span aria-hidden="true" className={cx("size-2 shrink-0 rounded-full", DOT[attention], className)} />
      <span className="sr-only">{text ?? DOT_TEXT[attention]}</span>
    </>
  );
}

/** Up/down arrows move focus through every `data-nav` button inside, in document order. */
function moveFocus(e: KeyboardEvent<HTMLElement>) {
  if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
  const items = Array.from(e.currentTarget.querySelectorAll<HTMLElement>("[data-nav]"));
  if (items.length === 0) return;
  const at = items.indexOf(document.activeElement as HTMLElement);
  if (at < 0 && e.key !== "ArrowDown") return;
  e.preventDefault();
  const next = e.key === "ArrowDown" ? (at + 1) % items.length : e.key === "ArrowUp" ? (at - 1 + items.length) % items.length : e.key === "Home" ? 0 : items.length - 1;
  items[next].focus();
}

/** The search field, shared by the sidebar, the medium layout and the phone list. */
export function SettingsSearchBox({
  value,
  onChange,
  onSubmit,
  inputRef,
  className,
}: {
  value: string;
  onChange: (value: string) => void;
  /** Enter: open the best match. */
  onSubmit: () => void;
  inputRef?: RefObject<HTMLInputElement | null>;
  className?: string;
}) {
  return (
    <div className={cx("relative", className)}>
      <IconSearch size={15} className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-muted" />
      <input
        ref={inputRef}
        type="search"
        role="searchbox"
        aria-label="Search settings"
        placeholder="Search settings"
        autoComplete="off"
        spellCheck={false}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            onSubmit();
          } else if (e.key === "Escape" && value) {
            e.preventDefault();
            e.stopPropagation();
            onChange("");
          } else if (e.key === "ArrowDown") {
            e.preventDefault();
            (e.currentTarget.closest("[data-search-scope]")?.querySelector("[data-nav]") as HTMLElement | null)?.focus();
          }
        }}
        className="h-9 w-full rounded-lg border border-border bg-panel pl-9 pr-14 text-body-sm text-text outline-none transition-colors placeholder:text-muted hover:border-border-strong focus:border-accent focus:ring-2 focus:ring-accent-ring [&::-webkit-search-cancel-button]:hidden"
      />
      {value ? (
        <button
          type="button"
          aria-label="Clear search"
          onClick={() => {
            onChange("");
            inputRef?.current?.focus();
          }}
          className="absolute right-1.5 top-1/2 grid size-6 -translate-y-1/2 cursor-pointer place-items-center rounded-md text-muted hover:bg-panel-2 hover:text-text"
        >
          <IconX size={13} />
        </button>
      ) : (
        <kbd aria-hidden="true" className="pointer-events-none absolute right-2.5 top-1/2 hidden -translate-y-1/2 rounded border border-border px-1.5 text-meta-lg text-muted sm:block">
          /
        </kbd>
      )}
    </div>
  );
}

/** What the search found: each hit as `Models › Model providers › MiniMax`, and its help. */
export function SettingsSearchResults({ query, hits, onPick }: { query: string; hits: SearchHit[]; onPick: (hit: SearchHit) => void }) {
  if (hits.length === 0) {
    return (
      <div className="px-3 py-8 text-center">
        <p className="text-body-sm font-medium text-text">Nothing matches “{query.trim()}”</p>
        <p className="mt-1 text-small text-muted">Try a plainer word, like “key”, “phone”, “update” or “model”.</p>
      </div>
    );
  }
  return (
    <ul aria-label={`${hits.length} ${hits.length === 1 ? "result" : "results"}`} className="space-y-0.5">
      {hits.map((hit) => (
        <li key={`${hit.entry.section}|${hit.entry.label}`}>
          <button
            type="button"
            data-nav
            onClick={() => onPick(hit)}
            className="flex w-full cursor-pointer flex-col gap-0.5 rounded-lg px-3 py-2 text-left hover:bg-panel-2 focus-visible:bg-panel-2"
          >
            <span className="flex items-center gap-1.5 text-meta-lg text-muted">
              {hit.crumbs.slice(0, -1).map((c, i) => (
                <span key={i} className="flex items-center gap-1.5">
                  {i > 0 && <span aria-hidden="true">›</span>}
                  {c}
                </span>
              ))}
            </span>
            <span className="text-body-sm font-medium text-text">{hit.crumbs[hit.crumbs.length - 1]}</span>
            <span className="line-clamp-2 text-small text-muted">{hit.entry.help}</span>
          </button>
        </li>
      ))}
    </ul>
  );
}

function GroupTile({ info, active }: { info: GroupInfo; active: boolean }) {
  return (
    <span className={cx("grid size-8 shrink-0 place-items-center rounded-lg transition-colors", active ? "bg-accent-soft text-accent" : "bg-panel-2 text-muted")}>
      <GuideIcon name={info.icon} size={17} />
    </span>
  );
}

/**
 * The wide layout's sidebar: search on top, then the groups. Only the group you are in opens its
 * pages, indented under it, so the list stays short; the page you are on has an accent bar.
 */
export function SettingsSidebar({
  groups,
  active,
  onSelect,
  search,
}: {
  groups: NavGroupModel[];
  active: SectionId | null;
  onSelect: (id: SectionId) => void;
  /** The search box and its results, built by the body (it owns the query). */
  search: { box: ReactNode; results: ReactNode | null };
}) {
  return (
    <nav aria-label="Settings" data-search-scope onKeyDown={moveFocus} className="flex min-h-0 w-[288px] shrink-0 flex-col border-r border-border bg-panel/50">
      <div className="shrink-0 border-b border-border p-3">{search.box}</div>
      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-2 py-2">
        {search.results ?? (
          <ul className="space-y-0.5">
            {groups.map((group) => {
              const open = group.pages.some((p) => p.id === active);
              return (
                <li key={group.info.id}>
                  <button
                    type="button"
                    data-nav
                    aria-expanded={open}
                    onClick={() => group.pages[0] && !open && onSelect(group.pages[0].id)}
                    className={cx(
                      "flex w-full cursor-pointer items-center gap-3 rounded-lg px-2 py-1.5 text-left transition-colors",
                      open ? "bg-panel-2/60" : "hover:bg-panel-2",
                    )}
                  >
                    <GroupTile info={group.info} active={open} />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-body-sm font-semibold text-text">{group.info.label}</span>
                      <span className="block truncate text-small text-muted">{group.info.blurb}</span>
                    </span>
                    <StatusDot attention={group.attention} />
                  </button>
                  {open && (
                    <ul className="relative mb-1.5 ml-[22px] mt-0.5 space-y-px border-l border-border pl-2">
                      {group.loading && (
                        <li className="flex items-center gap-2 px-2.5 py-1.5 text-small text-muted">
                          <Spinner className="size-3" /> Loading…
                        </li>
                      )}
                      {group.error && <li className="px-2.5 py-1.5 text-small text-err">{group.error}</li>}
                      {group.pages.map((page) => {
                        const current = page.id === active;
                        return (
                          <li key={page.id} className="relative">
                            {current && <span aria-hidden="true" className="absolute -left-[9px] bottom-1 top-1 w-[3px] rounded-full bg-accent" />}
                            <button
                              type="button"
                              data-nav
                              aria-current={current ? "page" : undefined}
                              title={page.hint}
                              onClick={() => onSelect(page.id)}
                              className={cx(
                                "flex w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-body-sm transition-colors",
                                current ? "bg-panel-2 font-medium text-text" : "text-muted hover:bg-panel-2 hover:text-text",
                              )}
                            >
                              <span className="min-w-0 flex-1 truncate">{page.label}</span>
                              {page.dirty && (
                                <>
                                  <span aria-hidden="true" className="size-1.5 shrink-0 rounded-full bg-accent" />
                                  <span className="sr-only">unsaved changes</span>
                                </>
                              )}
                              {page.badge && <span className="shrink-0 text-meta-lg tabular-nums text-muted">{page.badge}</span>}
                              <StatusDot attention={page.attention} text={page.attentionText} />
                            </button>
                          </li>
                        );
                      })}
                    </ul>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </nav>
  );
}

/** The medium layout's rail: the groups as icons, with their dots. The pages come as chips above the page. */
export function SettingsRail({ groups, active, onSelect }: { groups: NavGroupModel[]; active: SectionId | null; onSelect: (id: SectionId) => void }) {
  return (
    <nav aria-label="Settings groups" onKeyDown={moveFocus} className="flex w-[60px] shrink-0 flex-col items-center gap-1 border-r border-border bg-panel/50 py-3">
      {groups.map((group) => {
        const open = group.pages.some((p) => p.id === active);
        return (
          <button
            key={group.info.id}
            type="button"
            data-nav
            aria-label={group.info.label}
            aria-current={open ? "true" : undefined}
            title={`${group.info.label} — ${group.info.blurb}`}
            onClick={() => group.pages[0] && !open && onSelect(group.pages[0].id)}
            className="relative grid size-11 cursor-pointer place-items-center rounded-xl transition-colors hover:bg-panel-2"
          >
            {open && <span aria-hidden="true" className="absolute -left-[9px] bottom-2 top-2 w-[3px] rounded-full bg-accent" />}
            <GroupTile info={group.info} active={open} />
            {group.attention && <StatusDot attention={group.attention} className="absolute right-1.5 top-1.5 ring-2 ring-bg" />}
          </button>
        );
      })}
    </nav>
  );
}

/** The pages of the open group, as chips above the page: how the rail layout moves between them. */
export function PageChips({ group, active, onSelect }: { group: NavGroupModel | undefined; active: SectionId | null; onSelect: (id: SectionId) => void }) {
  if (!group || group.pages.length < 2) return null;
  return (
    <div className="shrink-0 border-b border-border">
      <ul aria-label={`${group.info.label} pages`} onKeyDown={moveFocus} className="scroll-thin mx-auto flex max-w-[760px] gap-1.5 overflow-x-auto px-5 py-2">
        {group.pages.map((page) => {
          const current = page.id === active;
          return (
            <li key={page.id} className="shrink-0">
              <button
                type="button"
                data-nav
                aria-current={current ? "page" : undefined}
                onClick={() => onSelect(page.id)}
                className={cx(
                  "flex cursor-pointer items-center gap-1.5 rounded-full border px-3 py-1 text-body-sm transition-colors",
                  current ? "border-accent/40 bg-accent-soft font-medium text-text" : "border-border text-muted hover:bg-panel-2 hover:text-text",
                )}
              >
                {page.label}
                <StatusDot attention={page.attention} text={page.attentionText} />
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/**
 * The phone's screen: search, then every group as a card of pages. Tapping a page drills into it
 * (the body shows it with a back arrow). One level, so nothing is hidden behind a second tap.
 */
export function SettingsList({
  groups,
  onSelect,
  initialFocus,
  search,
}: {
  groups: NavGroupModel[];
  onSelect: (id: SectionId) => void;
  /** Focus this page when the list appears (the one a narrow window just left). */
  initialFocus?: SectionId;
  search: { box: ReactNode; results: ReactNode | null };
}) {
  const root = useRef<HTMLElement>(null);
  useEffect(() => {
    if (!initialFocus) return;
    root.current?.querySelector<HTMLElement>(`[data-page-id="${CSS.escape(initialFocus)}"]`)?.focus({ preventScroll: false });
  }, [initialFocus]);
  return (
    <nav ref={root} aria-label="Settings" data-search-scope className="scroll-thin min-h-0 flex-1 overflow-y-auto">
      <div className="sticky top-0 z-[1] border-b border-border bg-bg/95 px-4 py-3 backdrop-blur">{search.box}</div>
      <div className="space-y-4 px-4 py-4">
        {search.results ??
          groups.map((group) => (
            <section key={group.info.id} aria-labelledby={`settings-group-${group.info.id}`} className="overflow-hidden rounded-2xl border border-border bg-panel">
              <header className="flex items-center gap-3 px-4 py-3">
                <GroupTile info={group.info} active />
                <div className="min-w-0 flex-1">
                  <h3 id={`settings-group-${group.info.id}`} className="truncate text-body-lg font-semibold text-text">
                    {group.info.label}
                  </h3>
                  <p className="truncate text-small text-muted">{group.info.blurb}</p>
                </div>
                <StatusDot attention={group.attention} />
              </header>
              {group.loading && (
                <p className="flex items-center gap-2 border-t border-border px-4 py-3 text-small text-muted">
                  <Spinner className="size-3" /> Loading…
                </p>
              )}
              {group.error && <p className="border-t border-border px-4 py-3 text-small text-err">{group.error}</p>}
              <ul className="divide-y divide-border border-t border-border">
                {group.pages.map((page) => (
                  <li key={page.id}>
                    <button
                      type="button"
                      data-nav
                      data-page-id={page.id}
                      onClick={() => onSelect(page.id)}
                      className="flex min-h-[52px] w-full cursor-pointer items-center gap-3 px-4 py-2.5 text-left hover:bg-panel-2"
                    >
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-body font-medium text-text">{page.label}</span>
                        <span className="block truncate text-small text-muted">{page.hint}</span>
                      </span>
                      {page.dirty && <span aria-hidden="true" className="size-1.5 shrink-0 rounded-full bg-accent" />}
                      {page.badge && <span className="shrink-0 text-meta-lg tabular-nums text-muted">{page.badge}</span>}
                      <StatusDot attention={page.attention} text={page.attentionText} />
                      <IconChevron size={14} className="shrink-0 text-muted" />
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          ))}
      </div>
    </nav>
  );
}

export interface AttentionItem {
  tone: Attention;
  /** Plain words: what is wrong, and what to do about it. */
  text: string;
  /** A button that goes and fixes it. */
  action?: { label: string; run: () => void };
}

/** The "needs you" callout at the top of a page: anything that needs action, before the settings. */
export function NeedsYou({ items }: { items: AttentionItem[] }) {
  if (items.length === 0) return null;
  const worst: Attention = items.some((i) => i.tone === "err") ? "err" : "warn";
  return (
    <section
      aria-label={worst === "err" ? "Something is broken" : "Needs you"}
      className={cx("mb-4 overflow-hidden rounded-xl border", worst === "err" ? "border-err/40 bg-err-soft" : "border-warn/40 bg-warn-soft")}
    >
      <h4 className="flex items-center gap-2 px-4 pt-3 text-body-sm font-semibold text-text">
        <IconAlert size={15} className={worst === "err" ? "text-err" : "text-warn"} />
        {worst === "err" ? "Something is broken" : "Needs you"}
      </h4>
      <ul className="divide-y divide-border/60 px-4 pb-1.5 pt-1">
        {items.map((item) => (
          <li key={item.text} className="flex flex-wrap items-center gap-x-3 gap-y-1.5 py-2">
            <span className="min-w-0 flex-1 basis-56 text-body-sm text-text">{item.text}</span>
            {item.action && (
              <button
                type="button"
                onClick={item.action.run}
                className="shrink-0 cursor-pointer rounded-lg border border-border-strong bg-panel px-3 py-1 text-small-lg font-medium text-text hover:bg-panel-2"
              >
                {item.action.label}
              </button>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}

