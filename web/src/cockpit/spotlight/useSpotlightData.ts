// What Spotlight searches beyond the colonies the cockpit already holds: recent conversations,
// loops, the settings index (pages, fields, every module setting) and the open issues of the
// workspace's busiest repositories. Loaded when the box first opens and kept a minute, so the
// second open is instant and a page load never pays for it.
import { useEffect, useMemo, useRef, useState } from "react";

import { useApi } from "../../context";
import { FIXED_PAGES, buildSearchIndex, groupInfo, groupOf, type SearchEntry } from "../../components/settings/nav";
import { kindInfo } from "../../components/settings/moduleFields";
import type { SectionId } from "../../components/settings/ui";
import type { ChatMeta, Loop, ModuleInfo, Repo } from "../../types";
import type { RepoIssue } from "../issuesList";

const TTL_MS = 60_000;
/** Repositories whose issues are fetched, busiest first. */
const ISSUE_REPOS = 8;
const CONCURRENCY = 3;

interface Cached<T> {
  at: number;
  value: T;
}

export interface SpotlightLoaded {
  chats: ChatMeta[];
  loops: Loop[];
  modules: ModuleInfo[];
  issues: RepoIssue[];
  loadingIssues: boolean;
  /** Pages and fields of Settings, ready to search. */
  index: SearchEntry[];
  crumbsOf: (entry: { section: SectionId; label: string; field: boolean }) => string[];
}

/** The repositories worth listing issues for: this scope's, with open issues, most recently pushed first. */
export function issueRepos(repos: readonly Repo[], org: string | null, limit = ISSUE_REPOS): string[] {
  return repos
    .filter((r) => !r.archived && r.has_issues !== false && r.open_issues_count > 0 && (!org || r.full_name.toLowerCase().startsWith(`${org.toLowerCase()}/`)))
    .sort((a, b) => (b.pushed_at ?? "").localeCompare(a.pushed_at ?? "") || a.full_name.localeCompare(b.full_name))
    .slice(0, limit)
    .map((r) => r.full_name);
}

export function useSpotlightData(open: boolean, repos: readonly Repo[], org: string | null, orgs: readonly string[]): SpotlightLoaded {
  const api = useApi();
  const [chats, setChats] = useState<ChatMeta[]>([]);
  const [loops, setLoops] = useState<Loop[]>([]);
  const [modules, setModules] = useState<ModuleInfo[]>([]);
  const [issues, setIssues] = useState<RepoIssue[]>([]);
  const [loadingIssues, setLoadingIssues] = useState(false);
  const issueCache = useRef(new Map<string, Cached<RepoIssue[]>>());
  const light = useRef<Cached<null> | null>(null);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    if (!light.current || Date.now() - light.current.at > TTL_MS) {
      light.current = { at: Date.now(), value: null };
      api.chats().then((r) => !cancelled && setChats(r.chats.slice(0, 40)), () => {});
      api.loops().then((r) => !cancelled && setLoops(r), () => {});
      api.modules().then((r) => !cancelled && setModules(r), () => {});
    }
    const wanted = issueRepos(repos, org);
    const stale = wanted.filter((r) => {
      const hit = issueCache.current.get(r);
      return !hit || Date.now() - hit.at > TTL_MS;
    });
    const gather = () => wanted.flatMap((r) => issueCache.current.get(r)?.value ?? []);
    setIssues(gather());
    if (stale.length === 0) return;
    setLoadingIssues(true);
    const queue = [...stale];
    const worker = async () => {
      for (let name = queue.shift(); name !== undefined; name = queue.shift()) {
        const repo = name;
        try {
          const list = await api.issues(repo);
          issueCache.current.set(repo, { at: Date.now(), value: list.map((i) => ({ ...i, repo })) });
        } catch {
          issueCache.current.set(repo, { at: Date.now(), value: [] });
        }
        if (!cancelled) setIssues(gather());
      }
    };
    void Promise.all(Array.from({ length: Math.min(CONCURRENCY, stale.length) }, worker)).then(() => !cancelled && setLoadingIssues(false));
    return () => {
      cancelled = true;
    };
  }, [api, open, repos, org]);

  const pages = useMemo(
    () => [
      ...FIXED_PAGES.map((p) => ({ id: p.id, label: p.label, hint: p.hint })),
      ...modules.map((m) => ({ id: `module:${m.kind}` as SectionId, label: kindInfo(m.kind).title, hint: kindInfo(m.kind).description })),
      ...orgs.map((o) => ({ id: `org:${o}` as SectionId, label: o, hint: `Settings for the ${o} workspace` })),
    ],
    [modules, orgs],
  );
  const index = useMemo(
    () =>
      buildSearchIndex({
        pages,
        modules: modules.map((m) => ({ kind: m.kind, title: kindInfo(m.kind).title, schema: m.schema })),
        orgs,
      }),
    [pages, modules, orgs],
  );
  const crumbsOf = useMemo(
    () => (entry: { section: SectionId; label: string; field: boolean }) => {
      const page = pages.find((p) => p.id === entry.section);
      const crumbs = [groupInfo(groupOf(entry.section)).label, page?.label ?? entry.label];
      return entry.field && page?.label !== entry.label ? [...crumbs, entry.label] : crumbs;
    },
    [pages],
  );
  return { chats, loops, modules, issues, loadingIssues, index, crumbsOf };
}
