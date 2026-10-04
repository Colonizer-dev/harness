import { PROVIDER_CATALOG } from "../../providerCatalog";
import type { ModelProvider, ProviderAuth, ProviderLimits, ProviderPreset, ProviderPricing, ProviderWire, SaveProviderRequest } from "../../types";

// ---------------------------------------------------------------------------
// Model providers (§6.3): the presets and catalogue the form is built from, the
// limit/pricing parsing, and the pure PUT-body builder the form saves with.
// ---------------------------------------------------------------------------

export type ProviderDraft = ProviderLimits & {
  id: string;
  name: string;
  base_url: string;
  auth: ProviderAuth;
  wire: ProviderWire;
  models: string[];
  /** Prefilled from a catalogue entry's verified rates, if it has any; a built-in preset never sets this. */
  pricing?: ProviderPricing;
};

export const DEFAULT_TIMEOUT = 600;
const DEFAULT_LIMITS: ProviderLimits = { timeout_secs: DEFAULT_TIMEOUT, max_concurrent: null, queue_timeout_secs: null, context_tokens: null, fallback_model: null };

const PRESETS: Record<ProviderPreset, ProviderDraft> = {
  deepseek: {
    id: "deepseek",
    name: "DeepSeek",
    base_url: "https://api.deepseek.com/anthropic",
    auth: "x-api-key",
    wire: "anthropic",
    models: ["deepseek-flash", "deepseek-v4-pro"],
    ...DEFAULT_LIMITS,
  },
  // OpenAI is not Anthropic-compatible: the gateway translates this one in both directions.
  openai: {
    id: "openai",
    name: "OpenAI",
    base_url: "https://api.openai.com",
    auth: "bearer",
    wire: "openai",
    models: ["gpt-5.6", "gpt-5.5"],
    ...DEFAULT_LIMITS,
    // Conservative: Claude Code compacts against this, and overshooting the real limit costs a failed turn.
    context_tokens: 272_000,
  },
  // Z.AI's coding plan speaks the Anthropic protocol; its key goes in an Authorization header.
  zai: {
    id: "zai",
    name: "Z.AI",
    base_url: "https://api.z.ai/api/anthropic",
    auth: "bearer",
    wire: "anthropic",
    models: ["glm-5.3", "glm-5.3-flash"],
    ...DEFAULT_LIMITS,
  },
  // Alibaba bills coding plans and token plans through different hosts; this is the token plan's.
  alibaba: {
    id: "alibaba",
    name: "Alibaba (Qwen)",
    base_url: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic",
    auth: "bearer",
    wire: "anthropic",
    models: ["qwen3.7-plus", "qwen3.8-flash"],
    ...DEFAULT_LIMITS,
  },
  // Local servers are slow and usually serve one or two requests at a time.
  local: { id: "local", name: "Local", base_url: "http://127.0.0.1:8080", auth: "none", wire: "anthropic", models: [], ...DEFAULT_LIMITS, timeout_secs: 900, max_concurrent: 1 },
  custom: { id: "", name: "", base_url: "", auth: "x-api-key", wire: "anthropic", models: [], ...DEFAULT_LIMITS },
};

/** Integer fields of the Advanced group, with the ranges the Mothership accepts. */
const LIMIT_RANGE = {
  timeout_secs: [30, 3600],
  max_concurrent: [1, 64],
  queue_timeout_secs: [1, 3600],
  context_tokens: [1024, 2_000_000],
} as const;

export type LimitKey = keyof typeof LIMIT_RANGE;

export function parseLimit(key: LimitKey, raw: string): { value: number | null; error: string | null } {
  const text = raw.trim().replace(/[_,\s]/g, "");
  if (!text) return { value: null, error: null };
  const [min, max] = LIMIT_RANGE[key];
  const value = Number(text);
  if (!Number.isInteger(value) || value < min || value > max) {
    return { value: null, error: `A whole number from ${min.toLocaleString()} to ${max.toLocaleString()}` };
  }
  return { value, error: null };
}

export const limitText = (value: number | null | undefined) => (value == null ? "" : String(value));

/** The four pricing rates, as they sit in the form's text fields. */
export type PricingDraft = Record<keyof ProviderPricing, string>;

const PRICING_KEYS = ["input_per_mtok", "output_per_mtok", "cache_read_per_mtok", "cache_write_per_mtok", "thinking_per_mtok"] as const;

const emptyPricingDraft = (): PricingDraft => ({
  input_per_mtok: "",
  output_per_mtok: "",
  cache_read_per_mtok: "",
  cache_write_per_mtok: "",
  thinking_per_mtok: "",
});

export const pricingDraftOf = (pricing: ProviderPricing | null | undefined): PricingDraft =>
  pricing ? Object.fromEntries(PRICING_KEYS.map((key) => [key, String(pricing[key] ?? "")])) as PricingDraft : emptyPricingDraft();

/** A rate is a dollar amount per million tokens: finite, never negative. Blank leaves the rate unset. */
export function parsePrice(raw: string): { value: number | null; error: string | null } {
  const text = raw.trim().replace(/[_,\s]/g, "");
  if (!text) return { value: null, error: null };
  const value = Number(text);
  if (!Number.isFinite(value) || value < 0) return { value: null, error: "A dollar amount, 0 or more" };
  return { value, error: null };
}

/** The collapsed Pricing summary: the rates currently in the fields, at 0 decimals or as given. */
export function pricingSummaryOf(pricing: Record<keyof ProviderPricing, { value: number | null }>): string[] {
  return PRICING_KEYS.map((key) => [key.replace(/_per_mtok$/, "").replace("_", " "), pricing[key].value] as const)
    .filter((entry): entry is [string, number] => entry[1] != null)
    .map(([label, value]) => `${label} $${value}`);
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${+(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

/** Short labels for non-default gateway settings, for rows and the collapsed Advanced summary. */
export function limitLabels(limits: Partial<ProviderLimits>): string[] {
  const labels: string[] = [];
  if (limits.max_concurrent != null) labels.push(`max ${limits.max_concurrent}`);
  if (limits.timeout_secs != null && limits.timeout_secs !== DEFAULT_TIMEOUT) labels.push(`timeout ${limits.timeout_secs} s`);
  if (limits.queue_timeout_secs != null) labels.push(`queue ${limits.queue_timeout_secs} s`);
  if (limits.context_tokens != null) labels.push(`context ${formatTokens(limits.context_tokens)}`);
  if (limits.fallback_model) labels.push(`fallback ${limits.fallback_model}`);
  return labels;
}

const PRESET_LABEL: Record<ProviderPreset, string> = {
  deepseek: "DeepSeek",
  openai: "OpenAI",
  zai: "Z.AI",
  alibaba: "Alibaba",
  local: "Local",
  custom: "Custom",
};


/** Shown while adding a provider, where the base URL is the thing people get wrong. */
export const PRESET_HINT: Partial<Record<ProviderPreset, string>> = {
  zai: "Uses your Z.AI coding plan key as a bearer token.",
  alibaba: "This is the token plan's host. A coding plan key needs coding-intl.dashscope.aliyuncs.com instead — the two are not interchangeable.",
};

export const WIRE_LABEL: Record<ProviderWire, string> = { anthropic: "Anthropic API", openai: "OpenAI API" };

export const AUTH_LABEL: Record<ProviderAuth, string> = {
  "x-api-key": "API key (x-api-key header)",
  bearer: "Bearer token (Authorization header)",
  none: "No authentication",
};

export type Editing = { mode: "new"; preset: ProviderPreset } | { mode: "edit"; id: string } | null;

/** Add buttons, in the order the presets are listed. */
export const CATALOG_BY_ID = new Map(PROVIDER_CATALOG.map((entry) => [entry.id, entry]));

/** The starting values for a new provider: a built-in preset, a catalogue entry, or bare Custom. */
export function presetDraft(preset: ProviderPreset): ProviderDraft {
  const built = PRESETS[preset];
  if (built) return built;
  const entry = CATALOG_BY_ID.get(preset);
  if (!entry) return PRESETS.custom;
  return {
    ...PRESETS.custom,
    id: entry.id,
    name: entry.name,
    base_url: entry.base_url,
    auth: entry.auth,
    wire: entry.wire,
    context_tokens: entry.context_tokens ?? PRESETS.custom.context_tokens,
    models: entry.models ?? PRESETS.custom.models,
    max_concurrent: entry.max_concurrent ?? PRESETS.custom.max_concurrent,
    pricing: entry.pricing ?? PRESETS.custom.pricing,
  };
}

/** A second (third, …) instance of a preset gets a free id and a suffixed name. */
export function uniqueDraft(preset: ProviderPreset, takenIds: string[]): ProviderDraft {
  const base = presetDraft(preset);
  if (!takenIds.includes(base.id)) return base;
  let n = 2;
  while (takenIds.includes(`${base.id}-${n}`)) n++;
  return { ...base, id: `${base.id}-${n}`, name: `${base.name} (${n})` };
}

/** A catalogue entry's label, for the form header and the mark's fallback initials. */
export function presetLabel(preset: ProviderPreset): string {
  return PRESET_LABEL[preset] ?? CATALOG_BY_ID.get(preset)?.name ?? "Provider";
}

export const ADD_PRESETS: { preset: ProviderPreset; label: string }[] = [
  { preset: "deepseek", label: "DeepSeek" },
  { preset: "openai", label: "OpenAI" },
  { preset: "zai", label: "Z.AI" },
  { preset: "alibaba", label: "Alibaba" },
  { preset: "local", label: "Local server" },
  { preset: "custom", label: "Custom" },
];

/**
 * The fallback picker's non-Claude choices (issue #767): every model on another provider that speaks
 * the same wire, as `<provider>/<model>` — the gateway retries a quota-exhausted request there
 * itself. A cross-wire provider is left out; the Mothership refuses it too.
 */
export function sameWireFallbacks(ownId: string, wire: ProviderWire, peers: ModelProvider[]): string[] {
  return peers
    .filter((p) => p.id !== ownId && p.wire === wire)
    .flatMap((p) => p.models.map((m) => `${p.id}/${m}`));
}

/** One row of the canonical → wire model map editor (#295): the name picked and the name sent. */
export interface ModelMapRow {
  canonical: string;
  wire: string;
}

/** The canonical names the rows would save: trimmed, blanks dropped. */
export function modelMapCanonicals(rows: ModelMapRow[]): string[] {
  return rows.map((row) => row.canonical.trim()).filter(Boolean);
}

/** Names set on more than one row. A saved map collapses duplicates last-wins, so the form refuses them. */
export function duplicateModelMapCanonicals(rows: ModelMapRow[]): string[] {
  const seen = new Set<string>();
  const duplicates = new Set<string>();
  for (const name of modelMapCanonicals(rows)) {
    if (seen.has(name)) duplicates.add(name);
    seen.add(name);
  }
  return [...duplicates];
}

/** The form's fields that decide the PUT body, as the form holds them — untrimmed, blanks included. */
export interface ProviderSaveInput {
  name: string;
  base_url: string;
  auth: ProviderAuth;
  wire: ProviderWire;
  models: string[];
  preset: ProviderPreset;
  api_key?: string;
  pricing?: ProviderPricing;
  quota: { url: string; pointer: string };
  timeout_secs: number | null;
  max_concurrent: number | null;
  queue_timeout_secs: number | null;
  context_tokens: number | null;
  fallback_model: string | null;
  /** The connection policy (#295, #472): trusted, the model map and the disabled tools. */
  trusted: boolean;
  model_map: ModelMapRow[];
  disabled_tools: string[];
}

/**
 * The PUT /api/providers/{id} body the form saves (#605). Pure, so the round-trip of the connection
 * policy — trusted, the model map and the disabled tools — is testable without a DOM. A row is dropped
 * only when its canonical name is blank; a blank wire name is kept as `""`, which the gateway reads as
 * "send the canonical name as it is" and still counts as an entry in the map's allowlist. An editor
 * left empty sends `{}`, which clears the saved map, like the key and pricing. `ChipsInput` has already
 * trimmed and deduped the disabled tools.
 */
export function providerSaveBody(input: ProviderSaveInput): SaveProviderRequest {
  return {
    name: input.name.trim(),
    base_url: input.base_url.trim(),
    auth: input.auth,
    wire: input.wire,
    models: input.models,
    preset: input.preset,
    api_key: input.api_key,
    pricing: input.pricing,
    quota: { url: input.quota.url.trim(), pointer: input.quota.pointer.trim() },
    timeout_secs: input.timeout_secs,
    max_concurrent: input.max_concurrent,
    queue_timeout_secs: input.queue_timeout_secs,
    context_tokens: input.context_tokens,
    fallback_model: input.fallback_model,
    trusted: input.trusted,
    model_map: Object.fromEntries(
      input.model_map
        .map((row) => [row.canonical.trim(), row.wire.trim()] as const)
        .filter(([canonical]) => canonical),
    ),
    disabled_tools: input.disabled_tools,
  };
}
