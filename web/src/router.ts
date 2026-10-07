// The cockpit's link to the address bar (issue #1180): pushState navigation that the rest of the
// app can follow. `navigate` writes the url and tells subscribers; the browser's own back and
// forward arrive as `popstate`. Components read the path through `usePath`, so a settings page
// follows the address without Cockpit threading it down.
import { useSyncExternalStore } from "react";

const EVENT = "colonizer:navigate";
const BASE = import.meta.env.BASE_URL;

export function routerBase(): string {
  return BASE;
}

/** Where the window is now, as path + query (no hash); "/" with no window (a static render). */
export function currentLocation(): { pathname: string; search: string; hash: string } {
  if (typeof window === "undefined") return { pathname: "/", search: "", hash: "" };
  return { pathname: window.location.pathname, search: window.location.search, hash: window.location.hash };
}

/**
 * Goes to `url` (a path with optional query and hash). The same address is a no-op, so a sync that
 * already agrees adds no history entry. `replace` swaps the current entry instead of adding one;
 * `silent` skips telling subscribers, for a caller that is itself reacting to the state.
 */
export function navigate(url: string, options: { replace?: boolean; silent?: boolean } = {}): void {
  if (typeof window === "undefined") return;
  const here = window.location.pathname + window.location.search + window.location.hash;
  if (url === here) return;
  try {
    window.history[options.replace ? "replaceState" : "pushState"](null, "", url);
  } catch {
    return;
  }
  if (!options.silent) window.dispatchEvent(new Event(EVENT));
}

/** Goes to a path, keeping the query as it is (`?org=`, `?mock=1`): how a pane moves between pages. */
export function go(path: string, options: { replace?: boolean } = {}): void {
  navigate(path + currentLocation().search, options);
}

/** Runs `listener` on every navigation: ours (`navigate`) and the browser's back and forward. */
export function subscribe(listener: () => void): () => void {
  window.addEventListener("popstate", listener);
  window.addEventListener(EVENT, listener);
  return () => {
    window.removeEventListener("popstate", listener);
    window.removeEventListener(EVENT, listener);
  };
}

/** The current pathname, re-rendering on every navigation. */
export function usePath(): string {
  return useSyncExternalStore(subscribe, () => currentLocation().pathname, () => "/");
}

/** The current hash (without `#`), re-rendering on navigation. */
export function useHash(): string {
  return useSyncExternalStore(subscribe, () => currentLocation().hash.replace(/^#/, ""), () => "");
}
