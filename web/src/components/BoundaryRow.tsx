// A boundary event (issue #609) in the colony timeline: a control refused something. Muted on
// purpose — one refusal is a wall the colony works around, not news; the watchdog's control-defeat
// flag is what says when a pattern of them needs a person, and it shows the same rows as evidence.
import type { BoundaryKind, BoundaryRecord } from "../types";
import { IconKey } from "./icons";
import { cx } from "./ui";

const KIND_WORDS: Record<BoundaryKind, string> = {
  exec_policy_deny: "Exec policy refused",
  exec_policy_ask_bypass_attempt: "Asked again after a refusal",
  path_policy_denied: "Path policy refused",
  path_policy_unbound: "Path policy not applied",
  egress_denied: "Network refused",
  publish_rewrite_refused: "Publish rewrote colony output",
  sandbox_denied: "Sandbox refused",
};

/** The row's lead words: what kind of control decided. An unknown kind reads plainly. */
export function boundaryLabel(record: Pick<BoundaryRecord, "kind">): string {
  return KIND_WORDS[record.kind] ?? "Control refused";
}

/** What the control refused, for the row and the evidence list: the target when named, else the detail. */
export function boundarySubject(record: Pick<BoundaryRecord, "target" | "detail">): string {
  return record.target?.trim() || record.detail;
}

/**
 * One boundary event. In the chat it sits indented and faint under the message it followed; as
 * `evidence` (the control-defeat attention item) it drops the indent and takes its banner's colour.
 */
export function BoundaryRow({ record, evidence = false }: { record: BoundaryRecord; evidence?: boolean }) {
  return (
    <div
      data-boundary={record.kind}
      title={record.detail}
      className={cx(
        "flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-0.5 text-small",
        evidence ? "my-0.5" : "my-1 ml-10 text-faint",
      )}
    >
      <IconKey size={12} className="shrink-0 opacity-70" />
      <span className="font-medium">{boundaryLabel(record)}</span>
      <code className="min-w-0 rounded bg-panel-2 px-1 font-mono text-[0.95em] [overflow-wrap:anywhere]">
        {boundarySubject(record)}
      </code>
      <span className="opacity-75">· {record.control}</span>
    </div>
  );
}
