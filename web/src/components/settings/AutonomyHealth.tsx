import type { AutonomyStatus } from "../../types";
import { cx, timeAgo } from "../ui";

/**
 * The autonomy judge's recent health (issue #875), shown above its settings: a compact line while
 * it is answering, and the last provider error — with how long the run of failures is — when it is
 * not. The pane renders it only for the autonomy module; a fetch failure (an older mothership) has
 * nothing to show and renders nothing.
 */
export function AutonomyHealth({ status, now = new Date() }: { status: AutonomyStatus; now?: Date }) {
  const failing = status.consecutive_failures > 0;
  const answered = status.last_success
    ? `Last answered ${timeAgo(status.last_success.at, now)} by ${status.last_success.model}`
    : "No answer yet";
  return (
    <div
      role="status"
      className={cx(
        "rounded-xl border px-4 py-3 text-[12.5px]",
        failing ? "border-warn/40 bg-warn-soft text-warn" : "border-border bg-panel-2/40 text-muted",
      )}
    >
      <p className="flex items-center gap-1.5">
        <span aria-hidden="true" className={cx("size-1.5 shrink-0 rounded-full", failing ? "bg-warn" : "bg-ok")} />
        {answered}
        {failing && <span>· {status.consecutive_failures} in a row</span>}
      </p>
      {status.last_error && <p className="mt-1 [overflow-wrap:anywhere]">{judgeErrorLine(status.last_error, now)}</p>}
    </div>
  );
}

/** The judge's last failure as one clause: when, which model, how it failed, and what the provider said. */
function judgeErrorLine(error: NonNullable<AutonomyStatus["last_error"]>, now: Date): string {
  return [
    `Failed ${timeAgo(error.at, now)} by ${error.model}`,
    error.kind.replace(/_/g, " "),
    error.status !== null ? `HTTP ${error.status}` : null,
    error.message || null,
  ]
    .filter(Boolean)
    .join(" · ");
}
