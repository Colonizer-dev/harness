// The `memory` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { clone, now, sleep } from "../../mockShared";
import type { MemoryNote } from "../../types";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { MemoryApi } from "./api";

export const mockKeychain = { available: true, backend: "macOS Keychain", reason: null, checked_at: new Date().toISOString() };
export const mockSecrets: import("./types").SecretRow[] = [
  { id: "github-token", label: "GitHub token", group: "connections", used_by: "Issues, pushes and pull requests", icon: "github", location: "file", env: null, env_set: false, updated_at: "2026-09-20T10:00:00Z", editable: true, colonies: { kind: "none", hosts: [] } },
  { id: "claude-token", label: "Claude token", group: "connections", used_by: "Every Claude colony", icon: "claude", location: "file", env: null, env_set: false, updated_at: "2026-09-22T08:00:00Z", editable: false, colonies: { kind: "injected", hosts: ["api.anthropic.com"] } },
  { id: "api-token", label: "Cockpit API token", group: "connections", used_by: "The cockpit sign-in and the colonizer CLI", icon: "key", location: "file", env: null, env_set: false, updated_at: null, editable: false, colonies: { kind: "none", hosts: [] } },
  { id: "provider-keys:zai", label: "Z.AI", group: "providers", used_by: "Models routed to zai", icon: "plug", location: "file", env: null, env_set: false, updated_at: "2026-09-17T04:00:00Z", editable: true, colonies: { kind: "gateway", hosts: [] } },
  { id: "provider-keys:bailian", label: "Alibaba Bailian", group: "providers", used_by: "Models routed to bailian", icon: "plug", location: "unset", env: null, env_set: false, updated_at: null, editable: true, colonies: { kind: "gateway", hosts: [] } },
  { id: "voice-keys:openai", label: "OpenAI (voice)", group: "integrations", used_by: "Speech to text in Colonize", icon: "mic", location: "env", env: "OPENAI_API_KEY", env_set: true, updated_at: null, editable: true, colonies: { kind: "none", hosts: [] } },
  { id: "colony:STRIPE_TEST_KEY", label: "STRIPE_TEST_KEY", group: "colonies", used_by: "Colonies on acme/web", icon: "key", location: "keychain", env: null, env_set: false, updated_at: "2026-09-23T09:00:00Z", editable: true, colonies: { kind: "injected", hosts: ["api.stripe.com"] } },
  { id: "jev", label: "TypeSafe (Jev)", group: "integrations", used_by: "Jev compaction, routing and recovery", icon: "spark", location: "unset", env: "JEV_API_KEY", env_set: false, updated_at: null, editable: true, colonies: { kind: "injected", hosts: ["api.typesafe.ai"] } },
];
export function mockSecret(id: string): import("./types").SecretRow {
  const row = mockSecrets.find((r) => r.id === id);
  if (!row) throw new Error("no such secret");
  return row;
}

export function memoryMock(ms: MockState): MemoryApi {
  return {
    secrets: async () => {
      await sleep(150);
      return { keychain: mockKeychain, secrets: mockSecrets.map((r) => ({ ...r })) };
    },
    saveSecret: async (id, _value, location) => {
      await sleep(200);
      const row = mockSecret(id);
      row.location = location ?? (row.location === "unset" || row.location === "env" ? "keychain" : row.location);
      row.updated_at = new Date().toISOString();
      return { ...row };
    },
    deleteSecret: async (id) => {
      await sleep(150);
      if (id.startsWith("colony:")) {
        const i = mockSecrets.findIndex((r) => r.id === id);
        if (i >= 0) mockSecrets.splice(i, 1);
        return { id, removed: true as const };
      }
      const row = mockSecret(id);
      row.location = row.env_set ? "env" : "unset";
      row.updated_at = null;
      return { ...row };
    },
    saveColonySecret: async (body) => {
      await sleep(200);
      const id = `colony:${body.env}`;
      const existing = mockSecrets.find((r) => r.id === id);
      const usedBy = body.scope.kind === "all" ? "Every colony" : body.scope.kind === "org" ? `Colonies on ${body.scope.org} repositories` : `Colonies on ${body.scope.repo}`;
      const row = {
        id, label: body.env, group: "colonies" as const, used_by: usedBy, icon: "key", location: "keychain" as const,
        env: null, env_set: false, updated_at: new Date().toISOString(), editable: true,
        colonies: { kind: "injected" as const, hosts: body.hosts },
      };
      if (existing) Object.assign(existing, row);
      else mockSecrets.push(row);
      return { id };
    },
    moveSecret: async (id, to) => {
      await sleep(200);
      const row = mockSecret(id);
      row.location = to;
      row.updated_at = new Date().toISOString();
      return { ...row };
    },
    memory: (scope, key) =>
      ms.later(() => ({
    scope,
    key,
    provider: "files",
    notes: ms.notes.filter((n) => n.scope === scope && n.key === key).sort((a, b) => b.created_at.localeCompare(a.created_at)),
    proposals: ms.proposals.filter((p) => p.scope === scope && p.key === key),
      })),
    memoryProposals: () => ms.later(() => [...ms.proposals].sort((a, b) => b.created_at.localeCompare(a.created_at))),
    approveProposal: async (id, edits) => {
      await sleep(250);
      const index = ms.proposals.findIndex((p) => p.id === id);
      if (index < 0) throw new ApiError("no such proposal", 404);
      const [proposal] = ms.proposals.splice(index, 1);
      const { status: _status, ...rest } = proposal;
      const note: MemoryNote = {
    ...rest,
    id: `note-${Math.random().toString(16).slice(2, 8)}`,
    title: edits?.title?.trim() || proposal.title,
    content: edits?.content?.trim() || proposal.content,
    created_at: now(),
      };
      ms.notes.push(note);
      return clone(note);
    },
    rejectProposal: async (id) => {
      await sleep(200);
      const index = ms.proposals.findIndex((p) => p.id === id);
      if (index < 0) throw new ApiError("no such proposal", 404);
      ms.proposals.splice(index, 1);
      return { ok: true };
    },
    createNote: async (body) => {
      await sleep(250);
      if (!body.title.trim() || !body.content.trim()) throw new ApiError("a note needs a title and content", 400);
      if (body.scope !== "global" && !body.key) throw new ApiError("org and repo notes need a key", 400);
      const note: MemoryNote = {
    id: `note-${Math.random().toString(16).slice(2, 8)}`,
    scope: body.scope,
    key: body.scope === "global" ? "" : body.key,
    title: body.title.trim(),
    content: body.content.trim(),
    tags: [],
    created_at: now(),
    source: { user: true },
      };
      ms.notes.push(note);
      return clone(note);
    },
    deleteNote: async ({ id, scope, key }) => {
      await sleep(200);
      const index = ms.notes.findIndex((n) => n.id === id && n.scope === scope && n.key === key);
      if (index < 0) throw new ApiError("no such note", 404);
      ms.notes.splice(index, 1);
      return { ok: true };
    },
    vaultProposals: () => ms.later(() => ({ configured: true, inbox: "Inbox/colonizer", proposals: clone(ms.vaultProposals) })),
    acceptVaultProposal: async (id) => {
      await sleep(200);
      const index = ms.vaultProposals.findIndex((p) => p.id === id);
      if (index < 0) throw new ApiError("no such vault proposal", 404);
      const [proposal] = ms.vaultProposals.splice(index, 1);
      return { ok: true as const, path: `Inbox/colonizer/${proposal.path}` };
    },
    rejectVaultProposal: async (id) => {
      await sleep(200);
      const index = ms.vaultProposals.findIndex((p) => p.id === id);
      if (index < 0) throw new ApiError("no such vault proposal", 404);
      ms.vaultProposals.splice(index, 1);
      return { ok: true };
    },
    mem0Status: () => ms.later(() => ({ ...ms.mem0 })),
    saveMem0Key: async (apiKey) => {
      await sleep(250);
      ms.mem0.has_key = apiKey.trim() !== "";
      ms.mem0.source = ms.mem0.has_key ? "saved" : null;
      return { ...ms.mem0 };
    },
    voice: () => ms.later(ms.voiceStatus),
    saveVoiceKey: async (provider, apiKey) => {
      await sleep(250);
      if (apiKey.trim()) ms.voiceKeys.add(provider);
      else ms.voiceKeys.delete(provider);
      return ms.voiceStatus();
    },
    transcribe: async (audio) => {
      await sleep(900);
      const mod = ms.modules.find((m) => m.kind === "voice");
      if (!mod || mod.provider === "browser") throw new ApiError("voice is set to the browser's own recognition; connect a service in Settings → Modules → Voice", 409);
      if (!ms.voiceKeys.has(mod.provider) && mod.provider !== "openai_compatible") throw new ApiError("add the API key in Settings → Modules → Voice", 409);
      return { text: audio.size > 0 ? "Add rate limiting to the webhook endpoint and cover it with a test" : "", provider: mod.provider };
    },
    checkMem0: async () => {
      await sleep(600);
      return ms.mem0.has_key ? { ok: true } : { ok: false, error: "add a mem0 API key in Settings → Modules → Memory" };
    }
  };
}
