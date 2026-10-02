// A colony's changed files, for the inspector's pull request card (issue #611). The card draws only
// the per-file +/- counts, so the unified diff text (capped at 200 KiB) is dropped here and never
// kept. Recomputing the whole diff is work the mothership pays per call, and a card is opened far
// more often than a colony commits, so the file list is cached per session and stamped with the
// `updated_at` it was fetched at: re-opening an unchanged card never refetches, and a colony that
// pushes more refetches once the list poll moves its `updated_at` on (the shape the commits fetch
// beside it already uses). A failed fetch shows no rows rather than a toast — the branch and PR link
// still carry the card.
import { useEffect, useState } from "react";
import type { Api } from "./api";
import { useApi } from "./context";
import type { Session, SessionDiffFile } from "./types";

/** A colony's files and the `updated_at` they were fetched at, so a newer session refetches. */
interface CacheEntry {
  updated_at: string;
  files: SessionDiffFile[];
}

/** The last fetch per session id, keyed on the `updated_at` it covered (see the file header). */
const cache = new Map<string, CacheEntry>();
/** Fetches already in flight, keyed by `id|updated_at`, so two opens of one card share a request. */
const pending = new Map<string, Promise<SessionDiffFile[]>>();

/**
 * The card's changed files for a colony: the cache entry when it is current for `updated_at`, else
 * the route, then cached. An entry a version stale is not returned — the caller keeps drawing it
 * while this promise is in flight.
 */
export async function loadSessionDiff(api: Api, id: string, updated_at: string): Promise<SessionDiffFile[]> {
  const cached = cache.get(id);
  if (cached && cached.updated_at === updated_at) return cached.files;
  const key = `${id}|${updated_at}`;
  const inFlight = pending.get(key);
  if (inFlight) return inFlight;
  const request = api
    .sessionDiff(id)
    .then((body) => {
      cache.set(id, { updated_at, files: body.files });
      return body.files;
    })
    .finally(() => pending.delete(key));
  pending.set(key, request);
  return request;
}

/** State carrying the colony its files belong to, so a switch cannot paint the wrong card. */
interface TaggedFiles {
  id: string;
  files: SessionDiffFile[];
}

/**
 * The files to draw for `id`, or null when the tagged state belongs to another colony — the guard
 * that stops colony A's files leaking into colony B's first render before the effect runs for B.
 */
export function currentFiles(state: TaggedFiles | null, id: string | null): SessionDiffFile[] | null {
  return state && state.id === id ? state.files : null;
}

/**
 * The files a colony changed, for its pull request card. Null while nothing is known: no PR to show
 * the card, no cached files yet, or the fetch failed. Refetches when the session's `updated_at`
 * moves on, keeping the last counts on screen until the new ones land.
 */
export function useSessionDiff(session: Session | null): SessionDiffFile[] | null {
  const api = useApi();
  // A colony without a pull request has no card, so there is no diff worth asking for.
  const id = session?.pr_url ? session.id : null;
  const updatedAt = session?.updated_at ?? "";
  const [state, setState] = useState<TaggedFiles | null>(() => {
    const hit = id ? cache.get(id)?.files : undefined;
    return id && hit ? { id, files: hit } : null;
  });
  useEffect(() => {
    if (!id) {
      setState(null);
      return;
    }
    // Keep the last counts (possibly a commit stale) while the refresh is in flight, rather than
    // blanking the card to a spinner-less gap.
    const cached = cache.get(id);
    setState(cached ? { id, files: cached.files } : null);
    let active = true;
    loadSessionDiff(api, id, updatedAt).then(
      (loaded) => {
        if (active) setState({ id, files: loaded });
      },
      () => {
        /* a courtesy read: a failed fetch leaves the card without file rows, never a toast */
      },
    );
    return () => {
      active = false;
    };
  }, [api, id, updatedAt]);
  return currentFiles(state, id);
}

/** How many changed files the card lists before it folds the rest behind a toggle. */
export const PR_FILES_SHOWN = 5;

/** The rows to draw for the card: the first few, or all when expanded, and how many the fold hides. */
export function prFileRows<T>(files: T[], expanded: boolean): { shown: T[]; hidden: number } {
  if (expanded || files.length <= PR_FILES_SHOWN) return { shown: files, hidden: 0 };
  return { shown: files.slice(0, PR_FILES_SHOWN), hidden: files.length - PR_FILES_SHOWN };
}
