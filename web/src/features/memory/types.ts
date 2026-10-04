// ---------------------------------------------------------------------------
// Shared memory (§6.2–6.3)
// ---------------------------------------------------------------------------

/** `key` is "" for global, the org for `org`, and `owner/repo` for `repo`. */
export type MemoryScope = "global" | "org" | "repo";

/** `origin` is where in the colony a note came from ("orchestrator", "subagent", "background", …).
 * Older proposals omit it; treat those as "orchestrator". */
export type MemorySource = { session_id: string; repo: string; origin?: string } | { user: true };

export interface MemoryNote {
  id: string;
  scope: MemoryScope;
  key: string;
  title: string;
  content: string;
  tags: string[];
  created_at: string;
  source: MemorySource;
}

export interface MemoryProposal extends MemoryNote {
  status: "pending";
}

export interface MemoryListing {
  scope: MemoryScope;
  key: string;
  /** Where approved notes live: `files` on the Mothership, or `mem0`. */
  provider?: string;
  notes: MemoryNote[];
  proposals: MemoryProposal[];
}

/** Whether a mem0 key is set and where from. The API never returns the key. */
export interface Mem0Status {
  has_key: boolean;
  source: "saved" | "MEM0_API_KEY" | null;
  /** mem0 is the memory module's saved provider. */
  active: boolean;
}

/** The voice module's active speech-to-text service (GET /api/voice). Never the key. */
export interface VoiceStatus {
  /** `browser` is the browser's own recogniser; anything else is a service the Mothership calls. */
  provider: string;
  name: string;
  model: string;
  /** ISO-639-1, or empty for auto-detect. */
  language: string;
  /** The service can be used now: a key is set (or not needed) and a base URL is known. Always true for `browser`. */
  configured: boolean;
  has_key: boolean;
  /** `saved`, the env var's name, or `provider:<id>` when a model provider's key is reused. */
  source: string | null;
  key_optional: boolean;
  max_seconds: number;
  max_bytes: number;
}

export interface Mem0Check {
  ok: boolean;
  error?: string;
}

export interface NewNoteRequest {
  scope: MemoryScope;
  key: string;
  title: string;
  content: string;
}

// ---------------------------------------------------------------------------
// Saved secrets (GET /api/secrets): where each key lives, never its value
// ---------------------------------------------------------------------------

export type SecretLocation = "keychain" | "file" | "env" | "unset";
export type SecretGroup = "providers" | "connections" | "integrations" | "colonies";

/** What a colony gets of a secret: nothing, the gateway's use of it, or a per-host substitution. */
export interface SecretColonyAccess {
  kind: "gateway" | "injected" | "none";
  /** For `injected`: the hosts msb swaps the placeholder for the value on (TLS only). */
  hosts: string[];
}

/** Which colonies a colony secret is given to. */
export type ColonySecretScope = { kind: "all" } | { kind: "org"; org: string } | { kind: "repo"; repo: string };

/** POST /api/secrets/colony. `value` is required for a new secret. */
export interface ColonySecretRequest {
  env: string;
  hosts: string[];
  scope: ColonySecretScope;
  value?: string;
}

export interface SecretRow {
  /** Stable id, e.g. `provider-keys:zai`; the path segment for PUT/DELETE/move. */
  id: string;
  label: string;
  group: SecretGroup;
  used_by: string;
  icon: string;
  location: SecretLocation;
  /** The environment variable that can also supply it, if any. */
  env: string | null;
  env_set: boolean;
  updated_at: string | null;
  /** False for secrets managed elsewhere (environment-only, or read by the CLI off disk). */
  editable: boolean;
  /** How it reaches colonies; absent from an older mothership. */
  colonies?: SecretColonyAccess;
}

export interface KeychainHealth {
  available: boolean;
  /** "macOS Keychain", "Secret Service", or "none". */
  backend: string;
  reason: string | null;
  checked_at: string | null;
}

export interface SecretsListing {
  keychain: KeychainHealth;
  secrets: SecretRow[];
}
