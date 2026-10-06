import { useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import type { TelemetryStatus } from "../../types";
import { Spinner, Switch, cx } from "../ui";
import { IconExternal } from "../icons";
import { Code, Pane, Row } from "./ui";

// ---------------------------------------------------------------------------
// Live map: a heartbeat to colonizer.dev, off until switched on (docs/telemetry.md)
// ---------------------------------------------------------------------------

const TELEMETRY_DOCS = "https://colonizer.dev/docs/telemetry";

export function LiveMapPane({
  telemetry,
  onChanged,
  back,
}: {
  telemetry: TelemetryStatus | null;
  onChanged: (telemetry: TelemetryStatus) => void;
  back?: () => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState(false);

  const set = async (enabled: boolean) => {
    setSaving(true);
    try {
      onChanged(await api.setTelemetry(enabled));
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  const info = (
    <>
      <p>
        While it is on, the Mothership sends a heartbeat every 5 minutes, and within a minute when the number of running colonies changes.
        colonizer.dev/live shows a dot for its area, about 25 km across, lit while colonies run.
      </p>
      <p>
        Switching it off takes the dot away at once and forgets the random id, so a later period on the map can’t be tied to this
        one. The id is forgotten even if the service can’t be reached; then the dot goes out within 12 minutes instead of at once.
      </p>
    </>
  );

  return (
    <Pane title="Live map" subtitle="This mothership as a dot on colonizer.dev/live" info={info} back={back}>
      {!telemetry ? (
        <p className="flex items-center gap-2 text-body-sm text-muted">
          <Spinner /> Loading…
        </p>
      ) : (
        <div className="space-y-4">
          <Row id="live-map-switch" label="Show this mothership on the live map" inline>
            <Switch
              id="live-map-switch"
              labelledBy="live-map-switch-label"
              label="Show this mothership on the live map"
              checked={telemetry.enabled === true}
              disabled={saving || telemetry.blocked_by !== null}
              onChange={(checked) => void set(checked)}
            />
          </Row>
          {telemetry.blocked_by && (
            <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-muted">
              Kept off by <Code>{telemetry.blocked_by}</Code> in the Mothership’s environment.
            </p>
          )}
          <div>
            <h4 className="mb-1.5 text-small-lg font-semibold">What is sent</h4>
            <pre className="scroll-thin overflow-x-auto rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 font-mono text-small leading-5">
              {JSON.stringify(
                { ...telemetry.heartbeat, install_id: telemetry.heartbeat.install_id ?? "(random, created when you switch it on)" },
                null,
                2,
              )}
            </pre>
            <p className="mt-2 text-small-lg text-muted">
              Nothing else: no repositories, issues, code, names or paths. The service sees this machine’s IP address, as any website
              would, turns it into a 25 km area and doesn’t store it. A heartbeat stops counting 12 minutes after it arrives, and
              its row is deleted about an hour after arrival, once anything else reaches the service. If this is the only mothership in its area, that dot is this one.
            </p>
          </div>
          {telemetry.enabled && (telemetry.last_sent_at || telemetry.last_error) && (
            <p className={cx("text-small-lg [overflow-wrap:anywhere]", telemetry.last_error ? "text-err" : "text-muted")}>
              {telemetry.last_error
                ? `Last heartbeat failed: ${telemetry.last_error}`
                : `Last heartbeat ${new Date(telemetry.last_sent_at!).toLocaleTimeString()}`}
            </p>
          )}
          <div className="flex flex-wrap gap-x-4 gap-y-1 text-small-lg">
            <a className="inline-flex items-center gap-1 text-accent hover:underline" href={telemetry.map_url} target="_blank" rel="noreferrer">
              Open the live map <IconExternal size={12} />
            </a>
            <a className="inline-flex items-center gap-1 text-accent hover:underline" href={TELEMETRY_DOCS} target="_blank" rel="noreferrer">
              How it works <IconExternal size={12} />
            </a>
          </div>
        </div>
      )}
    </Pane>
  );
}
