// "Colonizer updated": the card shown when a new build has installed and its service worker is
// waiting. Non-blocking, in the fixed column with the storage alert: the running build keeps
// working — its chunks stay cached until the reload — so this asks rather than swapping under the
// operator. "Later" puts off this waiting build only, like Setup's "Not now": the next one asks
// again.
import { useState } from "react";
import { useAppUpdate } from "../installApp";
import { Button } from "./ui";

export function UpdatePrompt() {
  const { ready, token, reload } = useAppUpdate();
  // "Later" puts off this waiting build only: the token changes when a newer worker takes its
  // place, so the next update asks again.
  const [dismissed, setDismissed] = useState<number | null>(null);
  if (!ready || dismissed === token) return null;
  return (
    <div role="status" className="rounded-2xl border border-border bg-panel p-4 shadow-[var(--shadow)]">
      <p className="text-[14px] font-semibold">Colonizer updated</p>
      <p className="mt-1.5 text-[12.5px] text-muted">A new build is ready. This one keeps working until you reload.</p>
      <div className="mt-3 flex justify-end gap-2">
        <Button size="sm" onClick={() => setDismissed(token)}>
          Later
        </Button>
        <Button variant="primary" size="sm" onClick={reload}>
          Reload
        </Button>
      </div>
    </div>
  );
}
