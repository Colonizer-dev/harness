import { useEffect, useId, useRef, useState, type ButtonHTMLAttributes, type ReactNode } from "react";
import type { Attention, HarnessStatus, ModelOption, Session, SessionStatus } from "../types";
import { IconAlert, IconInfo } from "./icons";

export function cx(...classes: (string | false | null | undefined)[]): string {
  return classes.filter(Boolean).join(" ");
}

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "secondary" | "ghost" | "danger";
  size?: "sm" | "md";
};

export function buttonClass(variant: ButtonProps["variant"] = "secondary", size: ButtonProps["size"] = "md"): string {
  return cx(
    "inline-flex shrink-0 cursor-pointer select-none items-center justify-center gap-1.5 whitespace-nowrap rounded-lg font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-50",
    size === "sm" ? "h-7 px-2.5 text-[12.5px]" : "h-9 px-3.5 text-sm",
    variant === "primary" && "bg-accent text-on-accent hover:bg-accent-hover disabled:hover:bg-accent",
    variant === "secondary" && "border border-border bg-panel text-text hover:bg-panel-2",
    variant === "ghost" && "text-muted hover:bg-panel-2 hover:text-text",
    variant === "danger" && "border border-border bg-panel text-err hover:bg-err-soft",
  );
}

export function Button({ variant, size, className, type = "button", ...props }: ButtonProps) {
  return <button type={type} className={cx(buttonClass(variant, size), className)} {...props} />;
}

export type Tone = "neutral" | "info" | "ok" | "warn" | "err" | "accent";

const TONE: Record<Tone, string> = {
  neutral: "border-border bg-panel-2 text-muted",
  info: "border-transparent bg-info-soft text-info",
  ok: "border-transparent bg-ok-soft text-ok",
  warn: "border-transparent bg-warn-soft text-warn",
  err: "border-transparent bg-err-soft text-err",
  accent: "border-transparent bg-accent-soft text-accent",
};

export function Badge({
  tone = "neutral",
  pulse = false,
  children,
  className,
  title,
}: {
  tone?: Tone;
  pulse?: boolean;
  children: ReactNode;
  className?: string;
  title?: string;
}) {
  return (
    <span
      title={title}
      className={cx(
        "inline-flex items-center gap-1.5 whitespace-nowrap rounded-full border px-2 py-0.5 text-[11.5px] font-semibold leading-4",
        TONE[tone],
        className,
      )}
    >
      {pulse && <span className="pulse-soft size-1.5 rounded-full bg-current" />}
      {children}
    </span>
  );
}

export const SESSION_STATUS: Record<SessionStatus, { label: string; tone: Tone; live: boolean }> = {
  queued: { label: "Queued", tone: "neutral", live: false },
  starting: { label: "Starting", tone: "info", live: true },
  running: { label: "Working", tone: "info", live: true },
  waiting_for_answer: { label: "Needs your answer", tone: "accent", live: true },
  idle: { label: "Idle", tone: "neutral", live: true },
  publishing: { label: "Opening PR", tone: "info", live: false },
  pr_opened: { label: "PR opened", tone: "info", live: false },
  merged: { label: "PR merged", tone: "ok", live: false },
  closed: { label: "PR closed", tone: "neutral", live: false },
  no_changes: { label: "No changes", tone: "warn", live: false },
  stopped: { label: "Stopped", tone: "neutral", live: false },
  failed: { label: "Failed", tone: "err", live: false },
};

/** Sessions whose microVM is up. */
export function isLive(status: SessionStatus): boolean {
  return SESSION_STATUS[status]?.live ?? false;
}

/** Whether the mesh is actually broken, as opposed to unavailable on this platform.
 *
 *  A Mac vendors no `tailscaled`, so the mothership reports `state: "unavailable"` and colonies use a
 *  loopback port (#32). That is by design and must not read as a fault — it used to, because every
 *  reader tested `mesh.error` and the payload put the explanation there. Kept in one place so the
 *  sidebar dot, the Settings nav and the Runtime row cannot disagree. */
export function meshBroken(mesh: HarnessStatus["mesh"]): boolean {
  if (!mesh || !mesh.enabled) return false;
  if (mesh.state === "unavailable") return false;
  return mesh.state === "error" || Boolean(mesh.error);
}

/** Deliberately not `isLive`: a colony mid-publish holds a parallelism slot though its microVM is gone.
 *  Mirrors `has_room`'s busy closure in crates/colonizer/src/sessions.rs; keep the two in step. */
export function occupiesSlot(status: SessionStatus): boolean {
  return isLive(status) || status === "publishing";
}

/** Statuses the publish endpoint accepts: live colonies, plus stopped, failed and no-changes ones whose worktree can still be finished. */
const PUBLISHABLE: SessionStatus[] = ["running", "waiting_for_answer", "idle", "stopped", "failed", "no_changes"];

/** Whether publishing this colony is possible: it kept its worktree (`git_admin_dir`, the server's own condition) and its status is one the endpoint reconciles — mirrored from the server, so the button never offers a publish that would 409. */
export function canPublish(session: Pick<Session, "status" | "cleaned_up" | "git_admin_dir">): boolean {
  return !session.cleaned_up && session.git_admin_dir != null && PUBLISHABLE.includes(session.status);
}

export function StatusBadge({ status }: { status: SessionStatus }) {
  const meta = SESSION_STATUS[status] ?? { label: status, tone: "neutral" as Tone, live: false };
  const animated = status === "starting" || status === "running" || status === "publishing" || status === "waiting_for_answer";
  return (
    <Badge tone={meta.tone} pulse={animated}>
      {meta.label}
    </Badge>
  );
}

export function Spinner({ className }: { className?: string }) {
  return (
    <svg className={cx("animate-spin", className)} width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle cx="12" cy="12" r="9" stroke="currentColor" strokeOpacity="0.25" strokeWidth="3" />
      <path d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" strokeWidth="3" strokeLinecap="round" />
    </svg>
  );
}

export const inputClass =
  "w-full min-w-0 rounded-lg border border-border bg-panel px-3 py-2 text-sm text-text outline-none transition-colors placeholder:text-faint focus:border-accent focus:ring-2 focus:ring-[var(--accent-ring)]";

export function Switch({
  checked,
  onChange,
  label,
  labelledBy,
  id,
  disabled,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  /** Accessible name; ignored when a visible label is tied in with `labelledBy`. */
  label: string;
  labelledBy?: string;
  id?: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      id={id}
      aria-checked={checked}
      aria-label={labelledBy ? undefined : label}
      aria-labelledby={labelledBy}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cx(
        "relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full transition-colors disabled:cursor-not-allowed disabled:opacity-50",
        checked ? "bg-accent" : "bg-panel-3",
      )}
    >
      <span
        className={cx(
          "inline-block size-4 rounded-full bg-white shadow transition-transform",
          checked ? "translate-x-[18px]" : "translate-x-0.5",
        )}
      />
    </button>
  );
}

/**
 * A small "i" button that reveals an explanation. Hover and focus peek at it; a click
 * pins it open so it also works on touch. Escape closes it without closing a parent dialog.
 */
export function InfoButton({ label, children, className }: { label: string; children: ReactNode; className?: string }) {
  const id = useId();
  const wrap = useRef<HTMLSpanElement>(null);
  const [pinned, setPinned] = useState(false);
  const [peek, setPeek] = useState(false);
  const [align, setAlign] = useState<"start" | "end">("start");
  const open = pinned || peek;

  useEffect(() => {
    if (!pinned) return;
    const onDown = (e: MouseEvent) => {
      if (!wrap.current?.contains(e.target as Node)) setPinned(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [pinned]);

  // Open towards the middle of the window so the popover is not clipped at the pane edge.
  const place = () => {
    const rect = wrap.current?.getBoundingClientRect();
    if (rect) setAlign(rect.left + rect.width / 2 > window.innerWidth / 2 ? "end" : "start");
  };

  return (
    <span
      ref={wrap}
      className={cx("relative inline-flex", className)}
      onMouseEnter={() => {
        place();
        setPeek(true);
      }}
      onMouseLeave={() => setPeek(false)}
      onKeyDown={(e) => {
        if (e.key === "Escape" && open) {
          e.preventDefault();
          e.stopPropagation();
          setPinned(false);
          setPeek(false);
        }
      }}
    >
      <button
        type="button"
        aria-label={`About ${label}`}
        aria-expanded={open}
        aria-controls={id}
        aria-describedby={id}
        onClick={() => {
          place();
          setPinned((p) => !p);
        }}
        onFocus={() => {
          place();
          setPeek(true);
        }}
        onBlur={() => setPeek(false)}
        className={cx(
          "grid size-5 shrink-0 cursor-pointer place-items-center rounded-full transition-colors hover:bg-panel-3 hover:text-text",
          open ? "text-text" : "text-faint",
        )}
      >
        <IconInfo size={13} />
      </button>
      <span
        role="tooltip"
        id={id}
        hidden={!open}
        className={cx(
          "absolute top-full z-30 mt-1.5 w-max max-w-[min(18rem,calc(100vw-2rem))] space-y-1.5 rounded-lg border border-border bg-panel-2 px-3 py-2 text-left text-[12px] font-normal leading-snug text-text shadow-[var(--shadow)] [overflow-wrap:anywhere]",
          align === "end" ? "right-0" : "left-0",
        )}
      >
        {children}
      </span>
    </span>
  );
}

/** The GitHub org (repository owner) a colony belongs to. */
export function orgOf(session: Pick<Session, "org" | "repo">): string {
  return session.org || session.repo.split("/")[0] || "";
}

export function sameOrg(a: string | null | undefined, b: string | null | undefined): boolean {
  return (a ?? "").toLowerCase() === (b ?? "").toLowerCase();
}

export function attentionText(attention: Attention): string {
  const n = attention.nudges ?? 0;
  switch (attention.reason) {
    case "stalled":
      return `No progress, nudged ${n}×`;
    case "nudges_exhausted":
      return `Still stalled after ${n} nudge${n === 1 ? "" : "s"}`;
    case "waiting_for_answer":
      return "Waiting for your answer";
    case "autopilot_held":
      return "Autopilot held the PR";
    default:
      return "Needs attention";
  }
}

/** The amber marker for colonies the watchdog flagged. */
export function AttentionBadge({ attention, className }: { attention: Attention | null | undefined; className?: string }) {
  if (!attention) return null;
  return (
    <span
      title={attentionText(attention)}
      className={cx(
        "inline-flex items-center gap-1 whitespace-nowrap rounded-full bg-warn-soft px-2 py-0.5 text-[11.5px] font-semibold leading-4 text-warn",
        className,
      )}
    >
      <IconAlert size={11} strokeWidth={2.5} /> Needs attention
    </span>
  );
}

/** "18 min ago", for sentences like "last activity 18 min ago". */
export function minutesAgo(ts: string | null | undefined): string {
  if (!ts) return "";
  const minutes = Math.max(0, Math.round((Date.now() - new Date(ts).getTime()) / 60_000));
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  return `${Math.round(hours / 24)} d ago`;
}

/** A free-text model field with suggestions from GET /api/models. */
export function ModelInput({
  value,
  onChange,
  models,
  placeholder,
  ariaLabel,
  id,
  className,
}: {
  value: string;
  onChange: (value: string) => void;
  models: ModelOption[];
  placeholder?: string;
  ariaLabel?: string;
  id?: string;
  className?: string;
}) {
  const listId = useId();
  return (
    <>
      <input
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        list={listId}
        placeholder={placeholder ?? "opus, deepseek/deepseek-flash, …"}
        aria-label={ariaLabel}
        spellCheck={false}
        autoComplete="off"
        className={cx(inputClass, "font-mono text-[13px]", className)}
      />
      <datalist id={listId}>
        {models.map((model) => (
          <option key={model.id} value={model.id}>
            {model.label}
          </option>
        ))}
      </datalist>
    </>
  );
}

export function timeAgo(ts: string | null | undefined): string {
  if (!ts) return "";
  const seconds = Math.max(0, (Date.now() - new Date(ts).getTime()) / 1000);
  if (seconds < 45) return "just now";
  if (seconds < 3600) return `${Math.round(seconds / 60)}m ago`;
  if (seconds < 86_400) return `${Math.round(seconds / 3600)}h ago`;
  return `${Math.round(seconds / 86_400)}d ago`;
}

export function formatDuration(ms: number | null | undefined): string {
  if (ms == null) return "";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}

export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const mq = window.matchMedia(query);
    const onChange = () => setMatches(mq.matches);
    onChange();
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [query]);
  return matches;
}

/** localStorage access that tolerates private windows and blocked storage. */
export function stored(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function store(key: string, value: string | null): void {
  try {
    if (value === null) window.localStorage.removeItem(key);
    else window.localStorage.setItem(key, value);
  } catch {
    /* storage unavailable */
  }
}
