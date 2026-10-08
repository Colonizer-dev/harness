// Cancel a red-team run (#1145): a button that asks first, because it stops every hunter of the run.
// Shared by the wizard's repository list, the run history and the hunter colony's inspector.
import { useEffect, useRef, useState } from "react";
import { errorMessage, useToast } from "../context";
import { Button, Spinner } from "../components/ui";
import type { RedTeamRun } from "../types";
import { plural } from "./redTeamPlan";

/** The confirm dialog's question: how many hunters stop, on what, and that the findings stay. */
export function cancelPrompt(run: Pick<RedTeamRun, "repo" | "hunters" | "swarm_size">): string {
  const n = run.hunters.length || run.swarm_size;
  return `Stop ${plural(n, "hunter")} on ${run.repo.split("/")[1] ?? run.repo}? Findings so far are kept.`;
}

export function CancelRunButton({
  run,
  onCancel,
  label = "Cancel run",
  size = "sm",
  className,
}: {
  run: RedTeamRun;
  onCancel: (id: string) => Promise<void>;
  label?: string;
  size?: "sm" | "md";
  className?: string;
}) {
  const toast = useToast();
  const ref = useRef<HTMLDialogElement>(null);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal?.();
    else if (!open && dialog.open) dialog.close();
  }, [open]);
  const confirm = async () => {
    setBusy(true);
    try {
      await onCancel(run.id);
      toast(`Red-team run on ${run.repo} cancelled`);
      setOpen(false);
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <Button
        size={size}
        variant="ghost"
        className={className}
        onClick={(e) => {
          e.stopPropagation();
          setOpen(true);
        }}
      >
        {label}
      </Button>
      <dialog
        ref={ref}
        onClose={() => setOpen(false)}
        onClick={(e) => e.stopPropagation()}
        aria-label="Cancel red-team run"
        className="m-auto w-[min(420px,calc(100vw-24px))] rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50"
      >
        <div className="space-y-4 p-5">
          <p className="text-body-lg font-semibold">{cancelPrompt(run)}</p>
          <p className="text-small-lg text-muted">Queued hunters leave the queue. Nothing is filed after this.</p>
          <div className="flex justify-end gap-2">
            <Button onClick={() => setOpen(false)}>Keep running</Button>
            <Button variant="danger" disabled={busy} onClick={confirm}>
              {busy && <Spinner />} Cancel run
            </Button>
          </div>
        </div>
      </dialog>
    </>
  );
}
