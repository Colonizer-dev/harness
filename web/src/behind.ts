// useBehind: how far a colony branch lags origin/{base} (issue #173). `behind` is
// undefined while loading, a number once known, and null when there is nothing to show
// (no base, or the fetch failed — staleness display must never nag, so errors hide the line).
import { useCallback, useEffect, useState } from "react";
import { useApi } from "./context";
import type { Session } from "./types";

export function useBehind(session: Session | null): {
  behind: number | null | undefined;
  base: string | null;
  refresh: () => void;
} {
  const api = useApi();
  const [behind, setBehind] = useState<number | null | undefined>(undefined);
  const [nonce, setNonce] = useState(0);
  const id = session?.id ?? null;
  const base = session?.base ?? null;
  const branch = session?.branch ?? null;
  useEffect(() => {
    if (!id || !base || !branch) {
      setBehind(null);
      return;
    }
    let active = true;
    setBehind(undefined);
    api.behindSession(id).then(
      (info) => {
        if (active) setBehind(info.behind_by);
      },
      () => {
        if (active) setBehind(null);
      },
    );
    return () => {
      active = false;
    };
  }, [api, id, base, branch, nonce]);
  const refresh = useCallback(() => setNonce((n) => n + 1), []);
  return { behind, base, refresh };
}
