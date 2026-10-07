import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { IconChevron, IconInfo } from "../icons";
import { Badge, InfoButton, cx } from "../ui";

// ---------------------------------------------------------------------------
// The shared pieces of every settings page: the pane frame and the row layout. The section ids the whole screen is keyed by live here too.
// ---------------------------------------------------------------------------

export type SectionId = "cockpit" | "setup" | "connections" | "providers" | "runtime" | "live-map" | "remote" | "phone" | "tokens" | "fleet" | "updates" | "usage" | "notifications" | "desktop" | "built-with" | "secrets" | `module:${string}` | `org:${string}`;

const PANE_TITLE_ID = "settings-pane-title";

/** The page's hero card (settingsGuide.tsx), which every Pane shows at the top of its body. */
export const HeroContext = createContext<ReactNode>(null);

// ---------------------------------------------------------------------------
// Pane and row layout shared by every section
// ---------------------------------------------------------------------------

/** The group a page sits in, shown small above its title (SettingsBody provides it). */
export const CrumbContext = createContext<string | null>(null);

/** The reading frame of a settings page: 760px, which leaves 720px of content (padding 20px each side). */
const READING_WIDTH = "mx-auto w-full max-w-[760px]";

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
  const crumb = useContext(CrumbContext);
  const stacked = Boolean(back);
  // In the narrow, back-navigable layout the pane replaces the list, so focus must follow.
  useEffect(() => {
    if (stacked) titleRef.current?.focus();
  }, [stacked]);

  return (
    <section aria-labelledby={PANE_TITLE_ID} className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="shrink-0 border-b border-border">
        <div className={cx(READING_WIDTH, "flex items-start gap-2 px-5 py-3.5")}>
          {back && (
            <button
              type="button"
              onClick={back}
              aria-label="Back to all settings"
              className="-ml-2 mt-0.5 grid size-9 shrink-0 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
            >
              <IconChevron size={17} className="rotate-180" />
            </button>
          )}
          <div className="min-w-0 flex-1">
            {crumb && <p className="text-meta-lg font-medium uppercase tracking-wide text-muted">{crumb}</p>}
            <h3
              id={PANE_TITLE_ID}
              ref={titleRef}
              tabIndex={stacked ? -1 : undefined}
              className="flex items-center gap-1 rounded text-title font-semibold leading-8"
              // A heading is focused by script on a phone, never by the keyboard: no ring to draw.
              style={{ outline: "none" }}
            >
              {title}
              {info && <InfoButton label={title}>{info}</InfoButton>}
            </h3>
            {subtitle && <p className="text-body-sm text-muted">{subtitle}</p>}
          </div>
          {aside && <div className="flex shrink-0 items-center leading-8">{aside}</div>}
        </div>
      </div>
      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto">
        <div className={cx(READING_WIDTH, "px-5 py-5")}>
          {hero}
          {children}
        </div>
      </div>
      {footer && (
        <div className="shrink-0 border-t border-border">
          <div className={cx(READING_WIDTH, "flex flex-wrap items-center gap-2 px-5 py-3")}>{footer}</div>
        </div>
      )}
    </section>
  );
}

/**
 * One setting: its label and plain-language help on the left, the control on the right. `help` is
 * the one sentence that says what it does; `info` is the longer story, behind the "i".
 * The label's id is `${id}-label`.
 */
export function Row({
  id,
  label,
  help,
  info,
  inline,
  children,
}: {
  id?: string;
  label: string;
  help?: ReactNode;
  info?: ReactNode;
  /** For switches: keeps the control on the label's line at every width. */
  inline?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 py-3.5">
      <div className="min-w-0 flex-1 basis-48">
        <div className="flex items-center gap-1">
          <label id={id ? `${id}-label` : undefined} htmlFor={id} className="text-body font-medium">
            {label}
          </label>
          {info && <InfoButton label={label}>{info}</InfoButton>}
        </div>
        {help && <p className="mt-0.5 text-small-lg leading-snug text-muted">{help}</p>}
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

/**
 * A page's one-line help with a small "i" that shows its hero diagram on demand, for pages where
 * the list is the point and a third of the screen of illustration before it is not.
 */
export function IntroLine({ text, label, children }: { text: string; label: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="mb-3">
      <p className="flex items-start gap-1.5 text-small-lg leading-snug text-muted">
        <span className="min-w-0 flex-1">{text}</span>
        <button
          type="button"
          aria-expanded={open}
          aria-label={label}
          title={label}
          onClick={() => setOpen((v) => !v)}
          className={cx(
            "-mt-0.5 grid size-6 shrink-0 cursor-pointer place-items-center rounded-md text-faint transition-colors hover:bg-panel-2 hover:text-text",
            open && "bg-panel-2 text-text",
          )}
        >
          <IconInfo size={15} />
        </button>
      </p>
      {open && <div className="mt-3">{children}</div>}
    </div>
  );
}
