// Notifications: a stack of toasts at the top right. Newest on top, four at a time, the rest folded
// into a "+N more" pill. Each is an opaque card — a tinted kind icon, a title and an optional body,
// an optional action, a close button — with a thin bar that runs out as it times out. The bar is the
// timer: its CSS animation pauses on hover, and the toast leaves when it ends, so pausing is exact.
import { useState, type ReactElement } from "react";
import { cx } from "./components/ui";

export type ToastKind = "info" | "success" | "warn" | "error";

export interface ToastInput {
  title: string;
  body?: string;
  kind?: ToastKind;
  action?: { label: string; onClick: () => void };
  /** Milliseconds before it leaves by itself; errors default to 10 s, the rest to 5 s. */
  duration?: number;
}

export interface ToastItem extends Required<Pick<ToastInput, "title" | "kind" | "duration">> {
  id: number;
  body?: string;
  action?: ToastInput["action"];
  leaving?: boolean;
}

/** Shown at once; the rest collapse into the "+N more" pill. */
export const MAX_VISIBLE = 4;

export const DEFAULT_DURATION: Record<ToastKind, number> = { info: 5000, success: 5000, warn: 7000, error: 10000 };

/**
 * A plain message as a title and a body: a long one splits after its first sentence or at an em
 * dash, so the title stays one line. A short one is all title.
 */
export function splitMessage(message: string): { title: string; body?: string } {
  const text = message.trim();
  if (text.length <= 64) return { title: text };
  const cuts = [text.indexOf(" — "), text.indexOf(". "), text.indexOf("… ")].filter((i) => i > 0 && i < 90);
  if (cuts.length === 0) return { title: text };
  const at = Math.min(...cuts);
  const sep = text.slice(at, at + 3) === " — " ? 3 : 1;
  const title = text.slice(0, at + (sep === 1 ? 1 : 0)).trim();
  const body = text.slice(at + sep).trim();
  return body ? { title, body } : { title };
}

/** Normalises either call form into a toast. */
export function toToast(id: number, input: string | ToastInput, tone?: "info" | "error" | ToastKind): ToastItem {
  const spec: ToastInput = typeof input === "string" ? { ...splitMessage(input), kind: tone ?? "info" } : input;
  const kind = spec.kind ?? "info";
  return { id, title: spec.title, body: spec.body, kind, action: spec.action, duration: spec.duration ?? DEFAULT_DURATION[kind] };
}

/** Newest first; all of them when expanded, else the first MAX_VISIBLE and how many are folded. */
export function stackOf(toasts: readonly ToastItem[], expanded: boolean): { shown: ToastItem[]; hidden: number } {
  const newest = [...toasts].reverse();
  if (expanded || newest.length <= MAX_VISIBLE) return { shown: newest, hidden: 0 };
  return { shown: newest.slice(0, MAX_VISIBLE), hidden: newest.length - MAX_VISIBLE };
}

const TINT: Record<ToastKind, string> = {
  info: "bg-accent-soft text-accent",
  success: "bg-ok/15 text-ok",
  warn: "bg-warn/15 text-warn",
  error: "bg-err/15 text-err",
};

const BAR: Record<ToastKind, string> = { info: "bg-accent", success: "bg-ok", warn: "bg-warn", error: "bg-err" };

function KindIcon({ kind }: { kind: ToastKind }): ReactElement {
  const path =
    kind === "success" ? (
      <path d="m5 12.5 4.5 4.5L19 7.5" />
    ) : kind === "error" ? (
      <>
        <circle cx="12" cy="12" r="8.5" />
        <path d="m9 9 6 6M15 9l-6 6" />
      </>
    ) : kind === "warn" ? (
      <>
        <path d="M12 4 21 19.5H3z" />
        <path d="M12 10v4M12 17h.01" />
      </>
    ) : (
      <>
        <circle cx="12" cy="12" r="8.5" />
        <path d="M12 11v5M12 8h.01" />
      </>
    );
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {path}
    </svg>
  );
}

export function ToastStack({ toasts, onDismiss }: { toasts: readonly ToastItem[]; onDismiss: (id: number) => void }): ReactElement {
  const [expanded, setExpanded] = useState(false);
  const { shown, hidden } = stackOf(toasts, expanded);
  return (
    <div
      aria-label="notifications"
      className="pointer-events-none fixed inset-x-3 top-14 z-[70] flex flex-col items-stretch gap-2 sm:inset-x-auto sm:right-4 sm:w-[380px]"
    >
      {/* Two live regions, so an error interrupts while an ordinary notice waits its turn. */}
      <div aria-live="assertive" className="sr-only">
        {toasts.filter((t) => t.kind === "error" && !t.leaving).map((t) => `${t.title}. ${t.body ?? ""}`).join(" ")}
      </div>
      <div aria-live="polite" className="sr-only">
        {toasts.filter((t) => t.kind !== "error" && !t.leaving).map((t) => `${t.title}. ${t.body ?? ""}`).join(" ")}
      </div>
      {shown.map((t) => (
        <div
          key={t.id}
          role={t.kind === "error" ? "alert" : "status"}
          className={cx(
            "toast-card group pointer-events-auto relative overflow-hidden rounded-xl border border-border-strong bg-panel shadow-[0_12px_40px_rgb(0_0_0/0.35)]",
            t.leaving && "toast-leave",
          )}
        >
          <div className="flex items-start gap-3 p-3 pr-2.5">
            <span className={cx("grid size-8 shrink-0 place-items-center rounded-lg", TINT[t.kind])}>
              <KindIcon kind={t.kind} />
            </span>
            <div className="min-w-0 flex-1 pt-0.5">
              <div className="text-[13.5px] font-semibold leading-snug text-text [overflow-wrap:anywhere]">{t.title}</div>
              {t.body && <div className="mt-0.5 text-[12.5px] leading-snug text-muted [overflow-wrap:anywhere]">{t.body}</div>}
              {t.action && (
                <button
                  type="button"
                  onClick={() => {
                    t.action?.onClick();
                    onDismiss(t.id);
                  }}
                  className="mt-2 inline-flex h-7 cursor-pointer items-center rounded-md border border-border bg-panel-2 px-2.5 text-[12px] font-medium text-text hover:border-border-strong"
                >
                  {t.action.label}
                </button>
              )}
            </div>
            <button
              type="button"
              aria-label="dismiss notification"
              onClick={() => onDismiss(t.id)}
              className="grid size-6 shrink-0 cursor-pointer place-items-center rounded-md border-0 bg-transparent text-muted opacity-70 hover:bg-panel-2 hover:text-text group-hover:opacity-100"
            >
              ×
            </button>
          </div>
          {/* The timer: when this bar runs out the toast leaves; hovering the card pauses it. */}
          <div
            aria-hidden="true"
            className={cx("toast-timer absolute bottom-0 left-0 h-[2px]", BAR[t.kind])}
            style={{ animationDuration: `${t.duration}ms` }}
            onAnimationEnd={() => onDismiss(t.id)}
          />
        </div>
      ))}
      {hidden > 0 && (
        <button
          type="button"
          onClick={() => setExpanded(true)}
          className="pointer-events-auto self-end rounded-full border border-border-strong bg-panel px-3 py-1 text-[12px] font-medium text-muted shadow-[0_8px_24px_rgb(0_0_0/0.25)] hover:text-text"
        >
          +{hidden} more
        </button>
      )}
      {expanded && toasts.length > MAX_VISIBLE && (
        <button type="button" onClick={() => setExpanded(false)} className="pointer-events-auto self-end rounded-full border-0 bg-transparent px-2 py-0.5 text-[12px] text-muted hover:text-text">
          Show fewer
        </button>
      )}
    </div>
  );
}
