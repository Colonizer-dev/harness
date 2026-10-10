// Pure logic behind the Queues view (issue #1127): grouping, the per-row wait wording, and the
// bulk-action outcome reducer. No React, no fetch — the colocated tests run on the plain values.
import { SESSION_STATUS, attentionText, parkedLabel, type Tone } from "../../components/ui";
import type { QueuesGroup, QueuesRow } from "./types";

/** One group of rows in the view's chosen grouping, in first-appearance order (rows arrive sorted). */
export interface QueuesGroupView {
  key: string;
  label: string;
  rows: QueuesRow[];
}

/** The repository a row's filters and groups name: `org/repo`, without doubling the org when `repo` already carries it. */
export function repoKey(row: Pick<QueuesRow, "org" | "repo">): string {
  return row.repo.includes("/") ? row.repo : `${row.org}/${row.repo}`;
}

/** The group a row falls in, keyed by what the row carries; a missing host reads as "unassigned". */
function groupKey(row: QueuesRow, group: QueuesGroup): string {
  switch (group) {
    case "host":
      return row.host ?? "unassigned";
    case "reason":
      return row.reason ?? "queued";
    case "repo":
      return repoKey(row);
    default:
      return "all";
  }
}

/**
 * Rows grouped for the view: `none` is one group holding everything; `host`, `reason` and `repo`
 * group by the row's host (`host ?? "unassigned"`), its machine wait reason (`reason ?? "queued"`)
 * or its `org/repo`, in first-appearance order.
 */
export function groupRows(rows: readonly QueuesRow[], group: QueuesGroup): QueuesGroupView[] {
  if (group === "none") return [{ key: "all", label: "", rows: [...rows] }];
  const buckets = new Map<string, QueuesRow[]>();
  for (const row of rows) {
    const key = groupKey(row, group);
    const bucket = buckets.get(key);
    if (bucket) bucket.push(row);
    else buckets.set(key, [row]);
  }
  return [...buckets].map(([key, grouped]) => ({ key, label: groupLabel(key, group, grouped[0]), rows: grouped }));
}

/** What a group's header reads: the key itself, except a reason group, which speaks in the maps' words
 *  (the park wording's "· resumes …" tail is a card's business, not a header's). */
function groupLabel(key: string, group: QueuesGroup, first: QueuesRow): string {
  if (group !== "reason") return key;
  return key === "queued" ? (SESSION_STATUS.queued.label ?? key) : reasonLabel(first).split(" · ")[0];
}

/**
 * The card wording for why a row waits, reused from the cockpit's own maps — never re-worded here:
 * an attention row reads `attentionText`, a row with a wait reason reads the park-reason map (with
 * the resume time appended, as `parkedLabel` writes it), and a plain row reads its status label.
 */
export function reasonLabel(row: QueuesRow): string {
  if (row.attention) return attentionText(row.attention);
  if (row.reason) return parkedLabel({ at: row.created_at, reason: row.reason, resets_at: row.resumes_at ?? undefined, vm_kept: true });
  return SESSION_STATUS[row.status]?.label ?? row.status;
}

/** The reason badge's tone: warn for parked and attention rows, info for waiting ones, neutral otherwise. */
export function reasonTone(row: QueuesRow): Tone {
  if (row.attention || row.status === "parked") return "warn";
  if (row.status === "queued" || row.status === "waiting_for_answer") return "info";
  return "neutral";
}

/** One colony's outcome in a bulk action: it happened, or the server refused it and said why. */
export interface ActionResult {
  id: string;
  ok: boolean;
  error?: string;
}

/** What a bulk action did: the ids it moved, and the refusals with the server's own words. */
export interface ActionOutcome {
  done: string[];
  refused: { id: string; error: string }[];
}

/** Splits per-id results into the outcome the results line shows: done first, then the refusals. */
export function summarizeActions(results: readonly ActionResult[]): ActionOutcome {
  const done: string[] = [];
  const refused: { id: string; error: string }[] = [];
  for (const result of results) {
    if (result.ok) done.push(result.id);
    else refused.push({ id: result.id, error: result.error || "it could not be done" });
  }
  return { done, refused };
}
