// Burn-down (issue #210): how the weekly token plan is being spent down to a reserve, in one card
// on the Overview. It polls /api/burn-down on its own and renders nothing until there is something
// to show, so Cockpit and App stay untouched — a fetch failure hides it as quietly as the first
// poll did.
import { useEffect, useState } from "react";

import { Badge, Button } from "../components/ui";
import { errorMessage, useApi, useToast } from "../context";
import type { BurnDownStatus } from "../types";
import { formatCountdown, msToReset, msUntil, shouldShow, stateLabel, stateTone, usd } from "./burnDown";

const POLL_MS = 10_000;
const TICK_MS = 1_000;

export function BurnDownCard() {
  const api = useApi();
  const toast = useToast();
  const [status, setStatus] = useState<BurnDownStatus | null>(null);
  // Re-read the clock every second so the countdown moves between polls; the status only refreshes
  // on POLL_MS.
  const [now, setNow] = useState(() => new Date());
  const [stopping, setStopping] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const load = () =>
      api
        .burnDown()
        .then((s) => {
          if (!cancelled) setStatus(s);
        })
        .catch(() => {
          // One bad poll hides the card, the same as one before the API existed; the next lands
          // on schedule and brings it back. Nothing here is worth an error toast.
          if (!cancelled) setStatus(null);
        });
    load();
    const poll = setInterval(load, POLL_MS);
    const tick = setInterval(() => setNow(new Date()), TICK_MS);
    return () => {
      cancelled = true;
      clearInterval(poll);
      clearInterval(tick);
    };
  }, [api]);

  if (!status || !shouldShow(status)) return null;

  const stop = async () => {
    if (!window.confirm("Stop burn-down and halt every colony it launched?")) return;
    setStopping(true);
    try {
      await api.stopBurnDown();
      toast("Burn-down stopped");
      setStatus(await api.burnDown());
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setStopping(false);
    }
  };

  // Only while it is burning — or armed with colonies — is there something to stop.
  const canStop = status.state === "burning" || (status.enabled && status.colonies.live + status.colonies.queued > 0);
  const windowIn = msUntil(status.window_start, now);
  const countdown =
    status.state === "outside_window" && windowIn !== null
      ? `window opens in ${formatCountdown(windowIn)}`
      : msToReset(status, now) !== null
        ? `reset in ${formatCountdown(msToReset(status, now))}`
        : null;
  const money =
    status.remaining_usd != null ? (
      <>
        {usd(status.remaining_usd)} left · reserve {status.reserve_usd != null ? usd(status.reserve_usd) : "unset"}
      </>
    ) : (
      <>allowance not set — scheduler idle</>
    );

  return (
    <section className="flex flex-col gap-1.5 rounded-2xl border border-border bg-panel px-4 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-[10.5px] tracking-[0.12em] text-faint">BURN-DOWN</span>
        <Badge tone={stateTone(status.state)}>{stateLabel(status.state)}</Badge>
        {status.estimate && (
          <Badge tone="neutral" title="The allowance and remaining spend are measured-window estimates, never the plan's real numbers">
            estimate
          </Badge>
        )}
        {canStop && (
          <Button size="sm" variant="danger" className="ml-auto" disabled={stopping} onClick={() => void stop()}>
            {stopping ? "Stopping…" : "Stop"}
          </Button>
        )}
      </div>
      <div className="flex flex-wrap gap-x-3 gap-y-0.5 font-mono text-[11px] text-faint tabular-nums">
        {countdown && <span>{countdown}</span>}
        <span>{money}</span>
        <span>
          {status.launches_needed != null
            ? `launched ${status.launches_done} of ~${status.launches_needed} planned`
            : `launched ${status.launches_done}`}
        </span>
        <span>
          {status.colonies.live} live · {status.colonies.queued} queued
        </span>
      </div>
    </section>
  );
}