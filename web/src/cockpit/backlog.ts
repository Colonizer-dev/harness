// The Nest's frontier badge (issue #1144): the open issues waiting in the repositories the operator
// colonizes. The mothership counts them (GET /api/status `backlog`, issues only, Colonizer's own
// orgs, cached about ten minutes); this turns that into what the badge shows and says.
import type { Backlog } from "../features/host/types";

export interface BacklogBadge {
  /** The number on the badge; null while the mothership has not counted yet (or is an older build). */
  count: number | null;
  /** The tooltip. */
  title: string;
}

/** `HH:MM` on the viewer's clock, zero-padded. */
export function clockTime(date: Date): string {
  const two = (n: number) => String(n).padStart(2, "0");
  return `${two(date.getHours())}:${two(date.getMinutes())}`;
}

/** "N open issues in M repositories you colonize · as of HH:MM", with the singular where it is one. */
export function backlogTooltip(issues: number, repos: number, asOf: Date | null): string {
  const text = `${issues} open ${issues === 1 ? "issue" : "issues"} in ${repos} ${repos === 1 ? "repository" : "repositories"} you colonize`;
  return asOf && !Number.isNaN(asOf.getTime()) ? `${text} · as of ${clockTime(asOf)}` : text;
}

/** The badge for the whole install, or for the selected workspace (matched case-insensitively). */
export function backlogBadge(backlog: Backlog | null | undefined, org: string | null): BacklogBadge {
  if (!backlog) return { count: null, title: "frontier · counting open issues" };
  const own = org ? Object.entries(backlog.by_org ?? {}).find(([name]) => name.toLowerCase() === org.toLowerCase())?.[1] : undefined;
  const { issues, repos } = org ? (own ?? { issues: 0, repos: 0 }) : backlog;
  return { count: issues, title: backlogTooltip(issues, repos, new Date(backlog.as_of)) };
}
