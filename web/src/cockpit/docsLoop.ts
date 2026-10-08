// The built-in Docs & README loop (docs_loop.rs, docs/loops.md#docs--readme): its settings, its
// reports, and the small pure helpers the Loops page's card is built from.
import { entryError } from "../components/repoSelect";

export interface DocsLoopSettings {
  /** Repositories (`owner/name`), orgs (`owner`) or `*` (every shown org) the loop runs on; empty means off. */
  allow: string[];
  /** Hours between runs: 24 (daily) by default, down to 1 (hourly). */
  interval_hours: number;
  /** Hours after a dispatch before the same repository may get another docs colony. */
  cooldown_hours: number;
}

export type DocsFindingKind = "undocumented_change" | "broken_link" | "broken_anchor" | "missing_command" | "routes_drift" | "changelog" | "docs_map";

export interface DocsFinding {
  kind: DocsFindingKind;
  file?: string;
  line?: number;
  change?: string;
  message: string;
  /** Reported, but not enough on its own to dispatch a colony. */
  advisory?: boolean;
}

export type DocsAction = "clean" | "dispatched" | "skipped" | "report_only" | "error";

export interface DocsRepoReport {
  repo: string;
  head: string | null;
  since: string | null;
  findings: DocsFinding[];
  more: number;
  action: DocsAction;
  reason: string;
  colony: string | null;
}

export interface DocsReport {
  id: string;
  at: string;
  trigger: "schedule" | "run_now" | "dry_run";
  dry_run: boolean;
  external_writes_blocked: boolean;
  repos: DocsRepoReport[];
}

/** GET /api/docs-loop. */
export interface DocsLoopView {
  name: string;
  settings: DocsLoopSettings;
  enabled: boolean;
  next_run_at: string | null;
  last_report: DocsReport | null;
  history: { id: string; at: string; trigger: string; summary: string }[];
  limits: { min_interval_hours: number; max_interval_hours: number; max_cooldown_hours: number };
}

export const DOCS_LOOP_ORIGIN = "docs-loop";

/** The interval choices the card offers: hourly up to weekly, daily first. */
export const DOCS_INTERVALS: { hours: number; label: string }[] = [
  { hours: 24, label: "Daily" },
  { hours: 12, label: "Every 12 hours" },
  { hours: 6, label: "Every 6 hours" },
  { hours: 1, label: "Hourly" },
  { hours: 168, label: "Weekly" },
];

export function describeInterval(hours: number): string {
  const preset = DOCS_INTERVALS.find((i) => i.hours === hours);
  if (preset) return preset.label.toLowerCase();
  return hours % 24 === 0 ? `every ${hours / 24} days` : `every ${hours} hours`;
}

/** An allowlist entry the server accepts: `*`, `owner` or `owner/name`. `null` when it is fine, else why not. */
export const allowEntryError = entryError;

export const DOCS_KIND_LABEL: Record<DocsFindingKind, string> = {
  undocumented_change: "code changed, docs did not",
  broken_link: "broken link",
  broken_anchor: "broken anchor",
  missing_command: "missing command",
  routes_drift: "routes drift",
  changelog: "changelog",
  docs_map: "docs map",
};

export const DOCS_ACTION_LABEL: Record<DocsAction, string> = {
  clean: "clean",
  dispatched: "colony dispatched",
  skipped: "skipped",
  report_only: "report only",
  error: "failed",
};

/** One line for a report: how many repositories, findings and dispatches. */
export function summarizeReport(report: DocsReport): string {
  const findings = report.repos.reduce((n, r) => n + r.findings.length + r.more, 0);
  const count = (a: DocsAction) => report.repos.filter((r) => r.action === a).length;
  const parts = [`${report.repos.length} ${report.repos.length === 1 ? "repository" : "repositories"}`, `${findings} ${findings === 1 ? "finding" : "findings"}`];
  for (const a of ["dispatched", "skipped", "report_only", "error"] as const) {
    const n = count(a);
    if (n) parts.push(`${n} ${DOCS_ACTION_LABEL[a]}`);
  }
  if (report.dry_run) parts.push("dry run");
  else if (report.external_writes_blocked) parts.push("external writes blocked");
  return parts.join(" · ");
}

/** Where a finding points: `file:line`, `file`, or nothing. */
export function findingPlace(f: DocsFinding): string {
  if (!f.file) return "";
  return f.line ? `${f.file}:${f.line}` : f.file;
}
