// Memory, secrets & voice API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, enc, post, put, request } from "../../http";
import type { ColonySecretRequest, Mem0Check, Mem0Status, MemoryListing, MemoryNote, MemoryProposal, MemoryScope, NewNoteRequest, SecretRow, SecretsListing, VaultProposalListing, VoiceStatus } from "./types";

export interface MemoryApi {
  /** GET /api/secrets: every saved secret and where it lives; values never leave the mothership. */
  secrets(): Promise<SecretsListing>;
  /** PUT /api/secrets/{id}: sets or replaces a secret; `location` also moves it there. */
  saveSecret(id: string, value: string, location?: "keychain" | "file"): Promise<SecretRow>;
  /** DELETE /api/secrets/{id}; a colony secret answers `{id, removed}` since its row is gone. */
  deleteSecret(id: string): Promise<SecretRow | { id: string; removed: true }>;
  /** POST /api/secrets/colony: adds a colony secret or changes its hosts, scope or value. */
  saveColonySecret(body: ColonySecretRequest): Promise<{ id: string }>;
  /** POST /api/secrets/{id}/move: between the system keychain and the 0600 file. */
  moveSecret(id: string, to: "keychain" | "file"): Promise<SecretRow>;
  memory(scope: MemoryScope, key: string): Promise<MemoryListing>;
  memoryProposals(): Promise<MemoryProposal[]>;
  approveProposal(id: string, edits?: { title?: string; content?: string }): Promise<MemoryNote>;
  rejectProposal(id: string): Promise<unknown>;
  createNote(body: NewNoteRequest): Promise<MemoryNote>;
  deleteNote(note: Pick<MemoryNote, "id" | "scope" | "key">): Promise<unknown>;
  /** GET /api/vault/proposals: notes colonies proposed for the operator vault (issue #777). */
  vaultProposals(): Promise<VaultProposalListing>;
  /** Writes the note into the vault's inbox folder as a new file; never overwrites one. */
  acceptVaultProposal(id: string): Promise<{ ok: true; path: string }>;
  rejectVaultProposal(id: string): Promise<unknown>;
  mem0Status(): Promise<Mem0Status>;
  /** Saves the key on the Mothership; an empty string removes it. */
  saveMem0Key(apiKey: string): Promise<Mem0Status>;
  /** Tries the saved key against the configured endpoint. */
  checkMem0(): Promise<Mem0Check>;
  /** The voice module's active speech-to-text service. */
  voice(): Promise<VoiceStatus>;
  /** Saves a voice service's key on the Mothership; an empty string removes it. */
  saveVoiceKey(provider: string, apiKey: string): Promise<VoiceStatus>;
  /** Sends a recorded clip to the connected service; the Mothership adds the key. */
  transcribe(audio: Blob): Promise<{ text: string; provider: string }>;
}

export const memoryHttp: MemoryApi = {
  secrets: () => request("/api/secrets"),
  saveSecret: (id, value, location) => put(`/api/secrets/${enc(id)}`, location ? { value, location } : { value }),
  deleteSecret: (id) => del(`/api/secrets/${enc(id)}`),
  saveColonySecret: (body) => post("/api/secrets/colony", body),
  moveSecret: (id, to) => post(`/api/secrets/${enc(id)}/move`, { to }),
  memory: (scope, key) => request(`/api/memory?scope=${enc(scope)}&key=${enc(key)}`),
  memoryProposals: () => request("/api/memory/proposals"),
  approveProposal: (id, edits) => post(`/api/memory/proposals/${enc(id)}/approve`, edits ?? {}),
  rejectProposal: (id) => post(`/api/memory/proposals/${enc(id)}/reject`),
  createNote: (body) => post("/api/memory/notes", body),
  deleteNote: ({ id, scope, key }) => del(`/api/memory/notes/${enc(id)}?scope=${enc(scope)}&key=${enc(key)}`),
  vaultProposals: () => request("/api/vault/proposals"),
  acceptVaultProposal: (id) => post(`/api/vault/proposals/${enc(id)}/accept`),
  rejectVaultProposal: (id) => post(`/api/vault/proposals/${enc(id)}/reject`),
  mem0Status: () => request("/api/memory/mem0"),
  saveMem0Key: (apiKey) => put("/api/memory/mem0", { api_key: apiKey }),
  checkMem0: () => post("/api/memory/mem0/check"),
  voice: () => request("/api/voice"),
  saveVoiceKey: (provider, apiKey) => put("/api/voice/key", { provider, api_key: apiKey }),
  transcribe: (audio) =>
    // The raw clip as the body, typed by what MediaRecorder produced (audio/webm;codecs=opus in Chrome).
    request("/api/voice/transcribe", { method: "POST", body: audio, headers: { "content-type": audio.type || "audio/webm" } }),
};
