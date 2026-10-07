import { useEffect, useState } from "react";
import { useApi } from "./context";
import type { ModelOption } from "./types";

/** Model suggestions for pickers (GET /api/models). Empty until loaded or when the call fails. */
export function useModels(reloadKey: unknown = 0): ModelOption[] {
  const api = useApi();
  const [models, setModels] = useState<ModelOption[]>([]);
  useEffect(() => {
    let cancelled = false;
    api
      .models()
      .then((list) => !cancelled && setModels(list))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [api, reloadKey]);
  return models;
}
