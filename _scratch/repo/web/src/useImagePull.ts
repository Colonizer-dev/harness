import { useCallback, useEffect, useState } from "react";
import { errorMessage, useApi } from "./context";
import type { PullStatus } from "./types";

/** What `useImagePull` hands back; threaded from App so one poller serves the whole app. */
export type ImagePull = ReturnType<typeof useImagePull>;

/**
 * The colony image's download, kept off the launch path.
 *
 * A cold pull of the default image measured 108 s. Started when a stack is picked, it happens
 * while someone is looking at Setup or Settings instead of while their first colony sits on a
 * spinner. Polls only while a pull is running. App runs the one instance; Setup, Settings and
 * the sidebar all read it, so the download stays visible after Setup closes.
 */
export function useImagePull(active: boolean) {
  const api = useApi();
  const [status, setStatus] = useState<PullStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!active) return;
    let stop = false;
    api
      .sandboxPullStatus()
      .then((s) => !stop && setStatus(s))
      .catch(() => {});
    return () => {
      stop = true;
    };
  }, [active, api]);

  useEffect(() => {
    if (!active || status?.state !== "pulling") return;
    const timer = setInterval(() => {
      api
        .sandboxPullStatus()
        .then(setStatus)
        .catch(() => {});
    }, 1000);
    return () => clearInterval(timer);
  }, [active, api, status?.state]);

  const start = useCallback(async () => {
    setError(null);
    try {
      setStatus(await api.sandboxPull());
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api]);

  return { status, error, start };
}
