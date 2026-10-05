import { useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import type { UsageStatus } from "../../types";
import { Spinner, Switch } from "../ui";
import { Code, Pane, Row } from "./ui";

// ---------------------------------------------------------------------------
// Usage data: the anonymous batch a sender would one day transmit — on by default, shown in full
// ---------------------------------------------------------------------------

export function UsagePane({
  usage,
  onChanged,
  back,
}: {
  usage: UsageStatus | null;
  onChanged: (usage: UsageStatus) => void;
  back?: () => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState(false);

  const set = async (enabled: boolean) => {
    setSaving(true);
    try {
      onChanged(await api.setUsage(enabled));
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  const info = (
    <>
      <p>
        The batch is built on this machine and shown here, and it is the exact value the sender posts — composed from Cratefield’s
        module-telemetry, validated against the same grammar the collector parses. It is sent at most once a day, and only when the
        Mothership was given a collector endpoint to post it to: with no <Code>COLONIZER_TELEMETRY_ENDPOINT</Code> in its
        environment, nothing is ever sent.
      </p>
      <p>
        The batch carries a random install id while it is on — which is the default — kept for at most thirty days and forgotten when
        switching off, so a later period could never be tied to this one.
      </p>
    </>
  );

  return (
    <Pane title="Usage data" subtitle="An anonymous batch, shown here in full — sent at most once a day, only to an endpoint you name" info={info} back={back}>
      {!usage ? (
        <p className="flex items-center gap-2 text-body-sm text-muted">
          <Spinner /> Loading…
        </p>
      ) : (
        <div className="space-y-4">
          <Row id="usage-switch" label="Allow an anonymous usage batch" inline>
            <Switch
              id="usage-switch"
              labelledBy="usage-switch-label"
              label="Allow an anonymous usage batch"
              checked={usage.enabled}
              disabled={saving || usage.blocked_by !== null}
              onChange={(checked) => void set(checked)}
            />
          </Row>
          {usage.blocked_by && (
            <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-muted">
              Kept off by <Code>{usage.blocked_by}</Code> in the Mothership’s environment.
            </p>
          )}
          <p className="text-small-lg text-muted">
            On by default. Switch it off here or with <Code>colonizer telemetry off</Code>; the Mothership’s environment can also hold it
            off whatever this switch says — <Code>COLONIZER_TELEMETRY=0</Code>, <Code>DO_NOT_TRACK=1</Code> or <Code>CI=true</Code>.
          </p>
          <div>
            <h4 className="mb-1.5 text-small-lg font-semibold">The whole batch</h4>
            <pre className="scroll-thin overflow-x-auto rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 font-mono text-small leading-5">
              {JSON.stringify(usage.batch, null, 2)}
            </pre>
            <p className="mt-2 text-small-lg text-muted">
              Every byte a send carries, verbatim — <Code>install</Code> is all zeros while the switch is off, a batch the sender will
              not post. Nothing else is in it: no repository, branch or issue names, no paths, no prompts or agent output, no tokens or
              URLs, and no setting values — setting names only.
            </p>
          </div>
        </div>
      )}
    </Pane>
  );
}
