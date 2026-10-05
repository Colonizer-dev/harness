import type { WebhookDeliveries as Deliveries, WebhookDelivery } from "../../types";
import { Button, Spinner, cx, timeAgo } from "../ui";

/**
 * The notify webhook's delivery health (issue #898), shown above the notify module's settings: one
 * line with the last success and how many deliveries wait for a retry, and the dead letter — the
 * deliveries that ran out of attempts — each with Replay and Discard. Renders nothing until the
 * webhook has done anything, so a notify module without one stays quiet.
 */
export function WebhookDeliveries({
  status,
  busy,
  onReplay,
  onDiscard,
  now = new Date(),
}: {
  status: Deliveries;
  /** The key of the dead letter whose replay or discard is in flight. */
  busy?: string | null;
  onReplay?: (key: string) => void;
  onDiscard?: (key: string) => void;
  now?: Date;
}) {
  const retrying = status.pending.length;
  const dead = status.dead_letters.length;
  if (!status.last_success_at && retrying === 0 && dead === 0) return null;
  const trouble = retrying > 0 || dead > 0;
  const parts = [
    status.last_success_at ? `Webhook last delivered ${timeAgo(status.last_success_at, now)}` : "The webhook has not delivered yet",
    retrying > 0 ? `${retrying} retrying` : null,
    dead > 0 ? `${dead} in the dead letter` : null,
  ].filter(Boolean);
  return (
    <div
      role="status"
      className={cx(
        "rounded-xl border px-4 py-3 text-[12.5px]",
        trouble ? "border-warn/40 bg-warn-soft text-warn" : "border-border bg-panel-2/40 text-muted",
      )}
    >
      <p className="flex items-center gap-1.5">
        <span aria-hidden="true" className={cx("size-1.5 shrink-0 rounded-full", trouble ? "bg-warn" : "bg-ok")} />
        {parts.join(" · ")}
      </p>
      {dead > 0 && (
        <ul className="mt-2 flex flex-col gap-2">
          {status.dead_letters.map((letter) => (
            <DeadLetter key={letter.key} letter={letter} busy={busy === letter.key} onReplay={onReplay} onDiscard={onDiscard} now={now} />
          ))}
        </ul>
      )}
    </div>
  );
}

function DeadLetter({
  letter,
  busy,
  onReplay,
  onDiscard,
  now,
}: {
  letter: WebhookDelivery;
  busy: boolean;
  onReplay?: (key: string) => void;
  onDiscard?: (key: string) => void;
  now: Date;
}) {
  return (
    <li className="flex flex-wrap items-center gap-2">
      <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">
        <span className="font-medium">{letter.event.replace(/_/g, " ")}</span> · {letter.attempts} attempts · last {timeAgo(letter.last_at, now)}
        {letter.last_error && <> · {letter.last_error}</>}
      </span>
      <Button size="sm" disabled={busy || !onReplay} onClick={() => onReplay?.(letter.key)}>
        {busy && <Spinner />} Replay
      </Button>
      <Button size="sm" variant="ghost" disabled={busy || !onDiscard} onClick={() => onDiscard?.(letter.key)}>
        Discard
      </Button>
    </li>
  );
}
