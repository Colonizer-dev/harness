// The pure half of RepoMultiSelect (issue #1212): what a repository-list setting's value means,
// and how a click edits it. A value is a list of entries: `*` (every repository of every visible
// org), `owner` (every repository of that org) or `owner/name`. Hidden orgs (issue #1213) are
// left out of the lists and out of "All" here, as the server leaves them out of `*`.
import type { OrgGroup } from "../cockpit/RepoPicker";
import type { OrgInfo } from "../types";

/** The wildcard every repository-list setting stores for "all repositories". */
export const ALL = "*";

const part = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

/** An entry the server accepts: `*`, `owner` or `owner/name`. `null` when it is fine, else why not. */
export function entryError(raw: string): string | null {
  const entry = raw.trim();
  if (!entry) return "Name a repository (owner/name), an org (owner) or * for all.";
  if (entry === ALL) return null;
  const parts = entry.split("/");
  if (parts.length > 2 || !parts.every((p) => part.test(p) && p !== "." && p !== "..")) {
    return `"${entry}" is not an owner or owner/name.`;
  }
  return null;
}

const same = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();
const ownerOf = (repo: string) => repo.split("/")[0];
const has = (value: readonly string[], entry: string) => value.some((v) => same(v, entry));
const without = (value: readonly string[], entry: string) => value.filter((v) => !same(v, entry));

/** What one chip says: "All repositories", "All in acme", or the repository. */
export function chipLabel(entry: string): string {
  if (entry === ALL) return "All repositories";
  return entry.includes("/") ? entry : `All in ${entry}`;
}

/** Whether the value names the org outright, or every repository. */
export function coversOrg(value: readonly string[], org: string): boolean {
  return has(value, ALL) || has(value, org);
}

/** Whether the value covers a repository: itself, its org, or everything. */
export function coversRepo(value: readonly string[], repo: string): boolean {
  return has(value, ALL) || has(value, ownerOf(repo)) || has(value, repo);
}

/** "All repositories" on or off. On replaces every narrower entry, which it already covers. */
export function toggleAll(value: readonly string[]): string[] {
  return has(value, ALL) ? without(value, ALL) : [ALL];
}

/** "All in <org>" on or off; on drops the org's single repositories, which it covers, and the wildcard. */
export function toggleOrg(value: readonly string[], org: string): string[] {
  if (has(value, org)) return without(value, org);
  return [...without(value, ALL).filter((v) => !same(ownerOf(v), org) || !v.includes("/")), org];
}

/** One repository on or off. Choosing one under "All" narrows the wildcard away to just that one. */
export function toggleRepo(value: readonly string[], repo: string): string[] {
  if (has(value, repo)) return without(value, repo);
  const base = has(value, ALL) || has(value, ownerOf(repo)) ? [] : [...value];
  return [...base, repo];
}

/** A typed entry added; `error` says why it was not. Duplicates (any case) are quietly one. */
export function addEntry(value: readonly string[], raw: string): { value: string[]; error: string | null } {
  const error = entryError(raw);
  if (error) return { value: [...value], error };
  const entry = raw.trim();
  if (entry === ALL) return { value: [ALL], error: null };
  if (coversRepo(value, entry) && has(value, entry)) return { value: [...value], error: null };
  return { value: [...without(value, entry), entry], error: null };
}

/** An entry removed (a chip's ✕). */
export function removeEntry(value: readonly string[], entry: string): string[] {
  return without(value, entry);
}

/** Orgs the operator did not hide. Absent or false means shown. */
export function visibleOrgs(orgs: readonly OrgInfo[]): OrgInfo[] {
  return orgs.filter((o) => o.settings?.hidden !== true);
}

/** The repositories whose org is not hidden. A repository whose org the list does not know is kept. */
export function visibleRepos(repos: readonly string[], orgs: readonly OrgInfo[]): string[] {
  const hidden = new Set(orgs.filter((o) => o.settings?.hidden === true).map((o) => o.org.toLowerCase()));
  return repos.filter((r) => !hidden.has(ownerOf(r).toLowerCase()));
}

/** The chips to show closed: the first `max`, and how many more are folded into "+n". */
export function chipsFor(value: readonly string[], max: number): { shown: string[]; more: number } {
  return { shown: value.slice(0, max), more: Math.max(0, value.length - max) };
}

/** A textarea-style list ("a, b c") as entries: split on commas, spaces and newlines, deduplicated. */
export function parseEntries(text: string): string[] {
  const out: string[] = [];
  for (const entry of text.split(/[\s,]+/).map((e) => e.trim()).filter(Boolean)) {
    if (!has(out, entry)) out.push(entry);
  }
  return out;
}

/** One line of the dropdown. */
export type Row =
  | { kind: "all" }
  | { kind: "org"; org: string; count: number; info: OrgInfo | undefined }
  | { kind: "repo"; repo: string; description: string | null }
  | { kind: "add"; text: string };

/**
 * The dropdown's lines for the (already filtered) groups: "All repositories" first, then each org
 * with its repositories, and "Add <text>…" last for a typed entry the lists do not already show.
 */
export function buildRows(groups: readonly OrgGroup[], query: string, allowAll: boolean, describe: (repo: string) => string | null): Row[] {
  const typed = query.trim();
  const rows: Row[] = [];
  if (allowAll && (!typed || "all repositories".includes(typed.toLowerCase()))) rows.push({ kind: "all" });
  for (const g of groups) {
    rows.push({ kind: "org", org: g.org, count: g.repos.length, info: g.info });
    for (const repo of g.repos) rows.push({ kind: "repo", repo, description: describe(repo) });
  }
  const known = rows.some((r) => (r.kind === "repo" && r.repo.toLowerCase() === typed.toLowerCase()) || (r.kind === "org" && r.org.toLowerCase() === typed.toLowerCase()));
  if (typed && !known) rows.push({ kind: "add", text: typed });
  return rows;
}

/** What picking a row does to the value; a typed entry reports why it was refused. */
export function pickRow(value: readonly string[], row: Row): { value: string[]; error: string | null } {
  if (row.kind === "all") return { value: toggleAll(value), error: null };
  if (row.kind === "org") return { value: toggleOrg(value, row.org), error: null };
  if (row.kind === "repo") return { value: toggleRepo(value, row.repo), error: null };
  return addEntry(value, row.text);
}
