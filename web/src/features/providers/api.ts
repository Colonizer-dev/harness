// Model providers, plugins & quota API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, enc, post, put, request } from "../../http";
import type { FindingsCard, QuotaActionReply, QuotaActionRequest, QuotaCard } from "../sessions/types";
import type { DownloadableSkillset, ModelOption, ModelProvider, PluginListing, ProviderHealth, ProviderTestResult, ProviderUsageReport, SaveProviderRequest } from "./types";

export interface ProvidersApi {
  providers(): Promise<ModelProvider[]>;
  saveProvider(id: string, body: SaveProviderRequest): Promise<ModelProvider>;
  deleteProvider(id: string): Promise<unknown>;
  /** Probes the provider from the Mothership; can take ~5 s. */
  providerHealth(id: string): Promise<ProviderHealth>;
  /** POST /api/providers/{id}/test: one token through the colony's own route; names the URL and status (issue #1018). */
  testProvider(id: string): Promise<ProviderTestResult>;
  /** GET /api/providers/{id}/usage?days=: per-day requests, failures and latency, balance readings and plan events (issue #1204). */
  providerUsage(id: string, days?: number): Promise<ProviderUsageReport>;
  /** GET /api/attention: what needs the maintainer beyond a colony's own question — the provider-out-of-quota cards (issue #767) and the findings-and-judge card (issue #1154); older motherships omit the latter. */
  attention(): Promise<{ quota_cards: QuotaCard[]; findings_cards?: FindingsCard[] }>;
  /** POST /api/providers/{id}/quota-action: answer a provider's out-of-quota card (switch, wait or stop). */
  quotaAction(provider: string, body: QuotaActionRequest): Promise<QuotaActionReply>;
  models(): Promise<ModelOption[]>;
  plugins(): Promise<PluginListing>;
  /** GET /api/plugins/{name}: one downloadable skillset's download status. */
  skillset(name: string): Promise<DownloadableSkillset>;
  /** POST /api/plugins/{name}/download: start (or join) the download; poll skillset for progress. */
  skillsetDownload(name: string): Promise<DownloadableSkillset>;
}

export const providersHttp: ProvidersApi = {
  providers: () => request("/api/providers"),
  saveProvider: (id, body) => put(`/api/providers/${enc(id)}`, body),
  deleteProvider: (id) => del(`/api/providers/${enc(id)}`),
  providerHealth: (id) => request(`/api/providers/${enc(id)}/health`),
  testProvider: (id) => post(`/api/providers/${enc(id)}/test`),
  providerUsage: (id, days = 7) => request(`/api/providers/${enc(id)}/usage?days=${days}`),
  attention: () => request("/api/attention"),
  quotaAction: (provider, body) => post(`/api/providers/${enc(provider)}/quota-action`, body),
  models: () => request("/api/models"),
  plugins: () => request("/api/plugins"),
  skillset: (name) => request(`/api/plugins/${enc(name)}`),
  skillsetDownload: (name) => post(`/api/plugins/${enc(name)}/download`),
};
