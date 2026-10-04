// The `modules` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { clone, sleep } from "../../mockShared";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { ModulesApi } from "./api";

export function modulesMock(ms: MockState): ModulesApi {
  return {
    modules: () => ms.later(() => ms.modules),
    saveModule: async (kind, body) => {
      await sleep(250);
      const module = ms.modules.find((m) => m.kind === kind);
      if (!module) throw new ApiError("unknown module kind", 404);
      if (!module.providers.some((p) => p.id === body.provider)) throw new ApiError("unknown provider", 400);
      Object.assign(module, { provider: body.provider, enabled: body.enabled, settings: body.settings });
      return clone(module);
    }
  };
}
