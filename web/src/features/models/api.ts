// The header model switcher's API methods (issue #1051). The root `Api` interface composes `ModelsApi`.
import { del, enc, post, put, request } from "../../http";
import type { ModelAssignments, ModelPlans, ModelProfile, ModelProfileBody, ModelSwitchReply, ModelSwitchRequest } from "./types";

export interface ModelsApi {
  /** GET /api/models/assignments: the effective model per role, install-wide and per org, with sources, modules and models. */
  modelAssignments(): Promise<ModelAssignments>;
  /** POST /api/models/switch: validated as a whole, then saved; `apply: "running"` also restarts the scope's colonies. */
  switchModels(body: ModelSwitchRequest): Promise<ModelSwitchReply>;
  /** GET /api/models/plans: each plan in use, with its limit state, request counts and plan balance. */
  modelPlans(): Promise<ModelPlans>;
  /** GET /api/models/profiles: the saved profiles, then the starters the install can run. */
  modelProfiles(): Promise<{ profiles: ModelProfile[] }>;
  /** POST /api/models/profiles: saves a profile in the install config. */
  createModelProfile(body: Required<Pick<ModelProfileBody, "name" | "roles">> & Pick<ModelProfileBody, "module">): Promise<ModelProfile>;
  /** PUT /api/models/profiles/{id}: renames a profile or replaces its roles. */
  updateModelProfile(id: string, body: ModelProfileBody): Promise<ModelProfile>;
  /** DELETE /api/models/profiles/{id}. */
  deleteModelProfile(id: string): Promise<{ deleted: string }>;
}

export const modelsHttp: ModelsApi = {
  modelAssignments: () => request("/api/models/assignments"),
  switchModels: (body) => post("/api/models/switch", body),
  modelPlans: () => request("/api/models/plans"),
  modelProfiles: () => request("/api/models/profiles"),
  createModelProfile: (body) => post("/api/models/profiles", body),
  updateModelProfile: (id, body) => put(`/api/models/profiles/${enc(id)}`, body),
  deleteModelProfile: (id) => del(`/api/models/profiles/${enc(id)}`),
};
