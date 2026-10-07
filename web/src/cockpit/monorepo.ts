// A monorepo's packages in the workspace dashboard: GET /api/repos/{owner}/{repo}/packages says
// where each package lives, and each colony's `changed_paths` (the files its pull request changed)
// place it in the packages it touched. Pure, so the table's figures are testable without a server.
import { sumCosts, sessionCost } from "../spend";
import type { RepoPackages, Session } from "../types";

/** The key of the row for changes outside every package (root files, docs, CI). */
export const OUTSIDE = "__outside__";
/** The key of the row for colonies whose changed files are not known yet. */
export const UNKNOWN = "__unknown__";

/** The package path a changed file belongs to: the longest package path that prefixes it. */
export function packageFor(path: string, packages: RepoPackages["packages"]): string | null {
  let best: string | null = null;
  for (const p of packages) {
    if ((path === p.path || path.startsWith(`${p.path}/`)) && (best === null || p.path.length > best.length)) best = p.path;
  }
  return best;
}

/** The row keys a colony counts toward: each package its files touch, OUTSIDE for files in none,
 *  UNKNOWN when its file list has not been read. */
export function packagesTouched(session: Session, packages: RepoPackages["packages"]): Set<string> {
  const paths = session.changed_paths ?? [];
  if (paths.length === 0) return new Set([UNKNOWN]);
  const keys = new Set<string>();
  for (const path of paths) keys.add(packageFor(path, packages) ?? OUTSIDE);
  return keys;
}

export interface PackageRow {
  key: string;
  name: string;
  path: string | null;
  colonies: number;
  merged: number;
  failed: number;
  rate: number;
  spend: number | null;
}

/** One row per package with colonies, then outside-packages and not-yet-read rows when they have
 *  any. A colony that touched several packages counts in each. */
export function packageRows(sessions: Session[], detection: RepoPackages): PackageRow[] {
  const by = new Map<string, Session[]>();
  for (const s of sessions) {
    for (const key of packagesTouched(s, detection.packages)) {
      const list = by.get(key) ?? [];
      list.push(s);
      by.set(key, list);
    }
  }
  const row = (key: string, name: string, path: string | null): PackageRow | null => {
    const list = by.get(key);
    if (!list || list.length === 0) return null;
    const merged = list.filter((s) => s.status === "merged").length;
    const failed = list.filter((s) => s.status === "failed").length;
    return { key, name, path, colonies: list.length, merged, failed, rate: merged / list.length, spend: sumCosts(list.map(sessionCost)) };
  };
  const packages = detection.packages
    .map((p) => row(p.path, p.name, p.path))
    .filter((r): r is PackageRow => r !== null)
    .sort((a, b) => b.colonies - a.colonies || a.name.localeCompare(b.name));
  const tail = [row(OUTSIDE, "(outside packages)", null), row(UNKNOWN, "(files not read yet)", null)].filter(
    (r): r is PackageRow => r !== null,
  );
  return [...packages, ...tail];
}

/** Whether a colony belongs to the package row `key`, for the dashboard's package filter. */
export function inPackage(session: Session, key: string, detection: RepoPackages): boolean {
  return packagesTouched(session, detection.packages).has(key);
}
