import type { ReactNode } from "react";
import type { HarnessStatus } from "../../types";
import { cx, meshBroken } from "../ui";
import { Pane } from "./ui";

// ---------------------------------------------------------------------------
// Runtime: what the Mothership found on this machine
// ---------------------------------------------------------------------------

export function RuntimePane({ status, back }: { status: HarnessStatus | null; back?: () => void }) {
  const rows: { label: string; value: ReactNode; bad?: boolean; mono?: boolean }[] = status
    ? [
        { label: "microsandbox", value: status.sandbox.msb_version ?? "not found", bad: !status.sandbox.msb_version },
        { label: "Image", value: status.sandbox.image, mono: true },
        {
          label: "Agent binary",
          value: status.sandbox.claude_bin ?? status.sandbox.claude_bin_error ?? "—",
          bad: Boolean(status.sandbox.claude_bin_error),
          mono: true,
        },
        {
          label: "Mesh",
          value: status.mesh
            ? status.mesh.enabled
              ? [status.mesh.provider, status.mesh.state, status.mesh.harness_ip, status.mesh.detail, status.mesh.error]
                  .filter(Boolean)
                  .join(" · ")
              : "disabled"
            : "—",
          bad: meshBroken(status.mesh),
        },
      ]
    : [];

  return (
    <Pane title="Runtime" subtitle="Detected on this machine" back={back}>
      {status ? (
        <dl className="divide-y divide-border">
          {rows.map((row) => (
            <div key={row.label} className="flex flex-wrap items-baseline gap-x-4 gap-y-1 py-2.5">
              <dt className="flex w-32 shrink-0 items-center gap-1.5 text-body-sm text-muted">
                {row.bad && <span aria-hidden="true" className="size-2 rounded-full bg-err" />}
                {row.label}
              </dt>
              <dd className={cx("min-w-0 flex-1 text-body-sm [overflow-wrap:anywhere]", row.mono && "font-mono text-small-lg", row.bad && "text-err")}>
                {row.value}
              </dd>
            </div>
          ))}
        </dl>
      ) : (
        <p className="text-body-sm text-muted">Mothership status unavailable.</p>
      )}
    </Pane>
  );
}
