// The live-map consent prompt (docs/telemetry.md): asked once, after setup. The live map stays off
// until the user says yes, and nothing is sent before a click.
//
// Where it sits depends on the screen. On a desktop or a narrow desktop window it is a fixed card in
// the corner, as it always was. On a phone (below `sm`) a fixed sheet at the foot of the screen
// covered the inbox's option rows and buttons, and the page could not scroll them clear, so there it
// renders in the flow instead: a card at the top of the Nest and of the Inbox list (`inline`).
import { useState, type ReactElement } from "react";

import { useApi } from "../context";
import type { TelemetryStatus } from "../types";
import { Button, cx } from "./ui";

/** A phone: below Tailwind's `sm`, where the cockpit shows the mobile tab bar (issue #516). */
export const PHONE_QUERY = "(max-width: 639px)";

/** Where the prompt goes: in the page flow on a phone, else the fixed corner column. */
export const liveMapPlacement = (phone: boolean): "inline" | "fixed" => (phone ? "inline" : "fixed");

export function LiveMapPrompt({
  onAnswered,
  onDetails,
  inline = false,
}: {
  onAnswered: (telemetry: TelemetryStatus) => void;
  onDetails: () => void;
  /** In the page flow (a phone) rather than a floating card: no shadow, tighter padding. */
  inline?: boolean;
}): ReactElement {
  const api = useApi();
  const [busy, setBusy] = useState(false);
  const answer = async (enabled: boolean) => {
    setBusy(true);
    try {
      onAnswered(await api.setTelemetry(enabled));
    } catch {
      setBusy(false);
    }
  };
  return (
    <div
      role="region"
      aria-label="Live map"
      data-placement={inline ? "inline" : "fixed"}
      className={cx("rounded-2xl border border-border bg-panel", inline ? "p-3.5" : "p-4 shadow-[var(--shadow)]")}
    >
      <p className="text-[14px] font-semibold">Put this mothership on the live map?</p>
      <p className="mt-1.5 text-[12.5px] text-muted">
        colonizer.dev/live shows where colonies are running, to within about 25 km. If yours is the only mothership in its area,
        that dot is you. It gets a heartbeat every 5 minutes: a random id, the version, the platform and how many colonies run.
        Nothing about your code. Off unless you say yes.
      </p>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <Button variant="primary" size="sm" disabled={busy} onClick={() => void answer(true)}>
          Show on the map
        </Button>
        <Button size="sm" disabled={busy} onClick={() => void answer(false)}>
          No thanks
        </Button>
        <button type="button" onClick={onDetails} className="ml-auto cursor-pointer text-[12.5px] text-accent hover:underline">
          What is sent
        </button>
      </div>
    </div>
  );
}
