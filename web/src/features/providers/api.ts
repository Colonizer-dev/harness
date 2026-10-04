// Model providers, plugins & quota API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, enc, post, put, request } from "../../http";
import type { QuotaActionReply, QuotaActionRequest, QuotaCard } from "../sessions/types";
import type { DownloadableSkillset, ModelOption, ModelProvider, PluginListing, ProviderHealth, SaveProviderRequest } from "./types";

export interface ProvidersApi {
  providers(): Promise<ModelProvider[]>;
  saveProvider(id: string, body: SaveProviderRequest): Promise<ModelProvider>;
  deleteProvider(id: string): Promise<unknown>;
  /** Probes the provider from the Mothership; can take ~5 s. */
  providerHealth(id: string): Promise<ProviderHealth>;
  /** GET /api/attention: what needs the maintainer beyond a colony's own question — the provider-out-of-quota cards (issue #767). */
  attention(): Promise<{ quota_cards: QuotaCard[] }>;
  /** POST /api/providers/{id}/quota-action: answer a provider's out-of-quota card (switch, wait or stop). */
  quotaAction(provider: string, body: QuotaActionRequest): Promise<QuotaActionReply>;
  models(): Promise<ModelOption[]>;
  plugins(): Promise<PluginListing>;
  /** GET /api/plugins/graft */
  graftSkillset(): Promise<DownloadableSkillset>;
  /** POST /api/plugins/graft/download: start (or join) the download; poll graftSkillset for progress. */
  graftDownload(): Promise<DownloadableSkillset>;
}

export const providersHttp: ProvidersApi = {
  providers: () => request("/api/providers"),
  saveProvider: (id, body) => put(`/api/providers/${enc(id)}`, body),
  deleteProvider: (id) => del(`/api/providers/${enc(id)}`),
  providerHealth: (id) => request(`/api/providers/${enc(id)}/health`),
  attention: () => request("/api/attention"),
  quotaAction: (provider, body) => post(`/api/providers/${enc(provider)}/quota-action`, body),
  models: () => request("/api/models"),
  plugins: () => request("/api/plugins"),
  graftSkillset: () => request("/api/plugins/graft"),
  graftDownload: () => post("/api/plugins/graft/download"),
};
