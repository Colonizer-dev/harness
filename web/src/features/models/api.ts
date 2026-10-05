// The header model switcher's API methods (issue #1051). The root `Api` interface composes `ModelsApi`.
import { post, request } from "../../http";
import type { ModelAssignments, ModelSwitchReply, ModelSwitchRequest } from "./types";

export interface ModelsApi {
  /** GET /api/models/assignments: the effective model per role, install-wide and per org, with sources, modules and models. */
  modelAssignments(): Promise<ModelAssignments>;
  /** POST /api/models/switch: validated as a whole, then saved; `apply: "running"` also restarts the scope's colonies. */
  switchModels(body: ModelSwitchRequest): Promise<ModelSwitchReply>;
}

export const modelsHttp: ModelsApi = {
  modelAssignments: () => request("/api/models/assignments"),
  switchModels: (body) => post("/api/models/switch", body),
};
