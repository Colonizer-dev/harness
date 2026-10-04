// Modules API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { enc, put, request } from "../../http";
import type { ModuleInfo } from "./types";

export interface SaveModuleRequest {
  provider: string;
  enabled: boolean;
  settings: Record<string, unknown>;
}

export interface ModulesApi {
  modules(): Promise<ModuleInfo[]>;
  saveModule(kind: string, body: SaveModuleRequest): Promise<ModuleInfo>;
}

export const modulesHttp: ModulesApi = {
  modules: () => request("/api/modules"),
  saveModule: (kind, body) => put(`/api/modules/${enc(kind)}`, body),
};
