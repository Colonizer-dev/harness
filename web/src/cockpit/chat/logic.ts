// The Chat view's pure parts: grouping and searching conversations, slash commands, model facts
// (provider, tags, vision, price), token estimates and persona presets. No React here, so the
// tests reach all of it.
import type { ChatAttachment, ChatAttachmentNote, ChatMessage, ChatMeta, ChatModels, ChatProvider } from "../../types";

// ---------------------------------------------------------------------------
// Conversation list
// ---------------------------------------------------------------------------

export type ChatGroupLabel = "Pinned" | "Today" | "Yesterday" | "Previous 7 days" | "Older";

const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();

/** Which bucket a conversation last touched at `ts` falls in, relative to `now` (local days). */
export function dayBucket(ts: string, now: Date): Exclude<ChatGroupLabel, "Pinned"> {
  const t = new Date(ts).getTime();
  const today = startOfDay(now);
  const day = 24 * 60 * 60 * 1000;
  if (Number.isNaN(t) || t >= today) return "Today";
  if (t >= today - day) return "Yesterday";
  if (t >= today - 7 * day) return "Previous 7 days";
  return "Older";
}

/** Pinned first, then Today / Yesterday / Previous 7 days / Older, each newest first; empty groups left out. */
export function groupChats(chats: readonly ChatMeta[], now: Date = new Date()): { label: ChatGroupLabel; chats: ChatMeta[] }[] {
  const order: ChatGroupLabel[] = ["Pinned", "Today", "Yesterday", "Previous 7 days", "Older"];
  const groups = new Map<ChatGroupLabel, ChatMeta[]>(order.map((l) => [l, []]));
  const sorted = [...chats].sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  for (const c of sorted) groups.get(c.pinned ? "Pinned" : dayBucket(c.updated_at, now))!.push(c);
  return order.map((label) => ({ label, chats: groups.get(label)! })).filter((g) => g.chats.length > 0);
}

/** Whether a conversation matches the sidebar's search (title, model, persona) and workspace filter. */
export function chatMatches(c: ChatMeta, query: string, workspace: string | null): boolean {
  if (workspace && (c.workspace ?? "").toLowerCase() !== workspace.toLowerCase()) return false;
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [c.title, c.model, personaLabel(c.persona)].some((s) => s.toLowerCase().includes(q));
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

export const SLASH_COMMANDS = [
  { name: "colony", hint: "Attach a colony's summary and recent activity" },
  { name: "file", hint: "Attach a repository file" },
  { name: "loop", hint: "Turn this conversation into a scheduled loop" },
  { name: "model", hint: "Switch the model" },
  { name: "system", hint: "Edit the system prompt" },
  { name: "clear", hint: "Start a new conversation" },
] as const;

export type SlashCommand = (typeof SLASH_COMMANDS)[number]["name"];

/** The commands a draft that starts with "/" could be, filtered by what is typed; `null` when it is not a command. */
export function slashMatches(draft: string): (typeof SLASH_COMMANDS)[number][] | null {
  const m = /^\/(\w*)$/.exec(draft.trim());
  if (!m || draft.includes("\n")) return null;
  const typed = m[1].toLowerCase();
  return SLASH_COMMANDS.filter((c) => c.name.startsWith(typed));
}

/** A complete command typed and sent ("/model"), or `null`. */
export function parseSlash(draft: string): SlashCommand | null {
  const m = /^\/(\w+)\s*$/.exec(draft.trim());
  if (!m) return null;
  return SLASH_COMMANDS.find((c) => c.name === m[1].toLowerCase())?.name ?? null;
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/** The provider a `<provider>/<model>` id goes through, or `null` for a plain Claude model. */
export function providerOf(model: string, models: ChatModels | null): ChatProvider | null {
  const slash = model.indexOf("/");
  if (slash < 0 || !models) return null;
  const id = model.slice(0, slash);
  return models.providers.find((p) => p.id === id) ?? null;
}

/** Whether images can be sent to this model: plain Claude models and Anthropic-wire providers. */
export function visionCapable(model: string, models: ChatModels | null): boolean {
  if (!model.includes("/")) return true;
  return (providerOf(model, models)?.wire ?? "anthropic") === "anthropic";
}

/** Rough speed/price/strength tags from the model's name, for the picker. */
export function modelTags(model: string): ("fast" | "cheap" | "strong")[] {
  const m = model.toLowerCase();
  const tags = new Set<"fast" | "cheap" | "strong">();
  if (/(flash|haiku|mini|nano|lite|turbo|small|instant|air)/.test(m)) tags.add("fast").add("cheap");
  if (/(opus|pro\b|-pro|max|reasoner|ultra|large|sonnet|glm-5\.3$|thinking|r1)/.test(m)) tags.add("strong");
  if (/(chat|deepseek)/.test(m) && !tags.has("strong")) tags.add("cheap");
  return [...tags];
}

/** The priced rates of a model, when its provider has any. */
export function pricingOf(model: string, models: ChatModels | null): { input_per_mtok: number; output_per_mtok: number } | null {
  const p = providerOf(model, models)?.pricing;
  return p && (p.input_per_mtok > 0 || p.output_per_mtok > 0) ? p : null;
}

/** Used when /api/models has not answered: the plain Claude models most installs offer. */
const CLAUDE_FALLBACK = ["claude-opus-5-5", "claude-sonnet-5", "claude-haiku-4-5"];

export interface ModelEntry {
  id: string;
  label: string;
  provider: string;
  preset?: string;
  hasKey: boolean;
  disabled: string | null;
}

/** Every model the picker offers: each provider's, then the plain Claude ones (flagged when this install cannot reach them). */
export function modelEntries(models: ChatModels | null, claudeIds: readonly { id: string; label: string }[] = []): ModelEntry[] {
  if (!models) return [];
  const plain = claudeIds.length > 0 ? claudeIds : CLAUDE_FALLBACK.map((id) => ({ id, label: id }));
  const claude: ModelEntry[] = plain.map(({ id, label }) => ({
    id,
    label,
    provider: "Anthropic",
    preset: "anthropic",
    hasKey: models.claude.available,
    disabled: models.claude.available ? null : models.claude.reason ?? "Claude models are not available here.",
  }));
  const rest = models.providers.flatMap((p) =>
    p.models.map((m) => ({ id: `${p.id}/${m}`, label: m, provider: p.name, preset: p.preset, hasKey: p.has_key ?? true, disabled: null })),
  );
  return [...rest, ...claude];
}

// ---------------------------------------------------------------------------
// Tokens and cost
// ---------------------------------------------------------------------------

/** About how many tokens a text is: four characters each, the usual rule of thumb. */
export function estimateTokens(text: string | number): number {
  const chars = typeof text === "number" ? text : text.length;
  return Math.ceil(chars / 4);
}

/** The input cost of `tokens` at a provider's rate, or `null` when it is not priced. */
export function inputCost(tokens: number, pricing: { input_per_mtok: number } | null): number | null {
  return pricing ? (tokens * pricing.input_per_mtok) / 1_000_000 : null;
}

/** A label for an attachment chip, before the server records its own. */
export function attachmentLabel(a: ChatAttachment): string {
  switch (a.kind) {
    case "colony":
      return `colony ${a.id}`;
    case "file":
      return a.path.split("/").pop() ?? a.path;
    case "map":
      return `${a.repo.split("/")[1] ?? a.repo} map`;
    case "map_component":
      return a.component;
    case "snippet":
      return a.label || "snippet";
    case "image":
      return a.name || "image";
    case "colonies_today":
      return "today's colonies";
    case "merged_prs":
      return `merged PRs · ${a.days ?? 7}d`;
  }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/** Repository-looking paths in a message's inline code (`src/main.rs`), for "open in Code". */
export function looksLikePath(code: string): boolean {
  return /^[\w.@-]+(\/[\w.@-]+)+\.[a-zA-Z0-9]{1,8}(:\d+)?$/.test(code.trim()) || /^[\w-]+\.(rs|ts|tsx|js|jsx|py|go|md|toml|json|yml|yaml|css|html|sh)$/.test(code.trim());
}

/** The ids of messages containing every word of the query, oldest first. */
export function searchMessages(messages: readonly ChatMessage[], query: string): string[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return [];
  return messages.filter((m) => words.every((w) => m.content.toLowerCase().includes(w))).map((m) => m.id);
}

/** Compare replies grouped by the user message they answer, so they render side by side. */
export function candidatesByParent(messages: readonly ChatMessage[]): Map<string, ChatMessage[]> {
  const out = new Map<string, ChatMessage[]>();
  for (const m of messages) {
    if (!m.candidate || !m.parent_id) continue;
    const list = out.get(m.parent_id) ?? [];
    list.push(m);
    out.set(m.parent_id, list.sort((a, b) => (a.lane ?? 0) - (b.lane ?? 0)));
  }
  return out;
}

/** "1.2 s" / "340 ms". */
export function formatMs(ms: number | null | undefined): string {
  if (ms == null) return "";
  return ms >= 1000 ? `${(ms / 1000).toFixed(ms >= 10_000 ? 0 : 1)} s` : `${ms} ms`;
}

// ---------------------------------------------------------------------------
// Personas
// ---------------------------------------------------------------------------

/** Which ant a persona is drawn as (web/src/cockpit/chat/ants.tsx). */
export type AntKind = "forager" | "soldier" | "weaver" | "honeypot";

export interface Persona {
  /** Stable: preset edits on the mothership are keyed by it. */
  id: string;
  /** The role, which is what a conversation stores as its `persona` label. Kept stable so older
   *  conversations still find their ant. */
  name: string;
  system: string;
  /** The ant's own name, shown first. */
  ant: string;
  /** Caste or species, e.g. "Soldier ant". */
  species: string;
  kind: AntKind;
  /** One line on what it does, shown in the picker instead of the raw prompt. */
  blurb: string;
}

export const DEFAULT_PERSONAS: readonly Persona[] = [
  {
    id: "plain",
    name: "Plain",
    system: "",
    ant: "Pip",
    species: "Forager",
    kind: "forager",
    blurb: "Just the model, no system prompt. Fetches whatever you ask for.",
  },
  {
    id: "reviewer",
    name: "Code reviewer",
    system:
      "You are a careful senior code reviewer. Look for bugs, security problems, missing tests and unclear names, in that order. Quote the lines you mean, say why each matters and propose the fix. Skip style nits a formatter would catch.",
    ant: "Sarge",
    species: "Soldier ant",
    kind: "soldier",
    blurb: "Guards the nest: bugs, security holes and missing tests, with the fix for each.",
  },
  {
    id: "architect",
    name: "Architect",
    system:
      "You are a pragmatic software architect. Explain how the parts fit, name the trade-offs, and recommend the simplest design that works. Prefer diagrams in text and short numbered plans.",
    ant: "Silka",
    species: "Weaver ant",
    kind: "weaver",
    blurb: "Weaves the parts together, names the trade-offs and picks the simplest design.",
  },
  {
    id: "release",
    name: "Release writer",
    system:
      "You write release notes for engineers and users. Group changes under Added, Changed, Fixed and Security, one line each, linking pull requests. Lead with what users notice; leave out internal refactors.",
    ant: "Mellie",
    species: "Honeypot ant",
    kind: "honeypot",
    blurb: "Stores up every change and serves it as release notes users actually read.",
  },
];

/** The preset a conversation's stored `persona` label points at: its role (what is stored), its id
 *  or its ant's name, any case. `null` for a label no preset has (or none). */
export function personaFor<P extends Persona>(label: string | null | undefined, personas: readonly P[]): P | null {
  const l = (label ?? "").trim().toLowerCase();
  if (!l) return null;
  return personas.find((p) => p.name.toLowerCase() === l || p.id === l || p.ant.toLowerCase() === l) ?? null;
}

/** How a conversation's persona reads in the list: "Sarge · Code reviewer", or the stored label as is. */
export function personaLabel(label: string | null | undefined): string {
  const p = personaFor(label, DEFAULT_PERSONAS);
  return p ? `${p.ant} · ${p.name}` : label ?? "";
}

/** The presets with the operator's edits (kept on the mothership), defaults for anything unedited. */
export function loadPersonas(edits: Readonly<Record<string, string>>): Persona[] {
  return DEFAULT_PERSONAS.map((p) => (typeof edits[p.id] === "string" ? { ...p, system: edits[p.id] } : { ...p }));
}

/** What to store for a preset saved with `system`: `null` when it matches the built-in prompt again. */
export function personaEdit(id: string, system: string): string | null {
  const preset = DEFAULT_PERSONAS.find((p) => p.id === id);
  return preset && preset.system === system ? null : system;
}

/** Where earlier versions kept persona edits and reply notes in the browser; moved to the mothership once. */
export const LEGACY_PERSONA_KEY = "colonizer.chat.personas";
export const LEGACY_FEEDBACK_KEY = "colonizer.chat.feedback";

/** The entries of a browser-stored JSON map the mothership does not have yet (bad JSON reads as empty). */
export function legacyEntries(raw: string | null, server: Readonly<Record<string, string>>): [string, string][] {
  let local: unknown;
  try {
    local = JSON.parse(raw ?? "{}");
  } catch {
    return [];
  }
  if (!local || typeof local !== "object") return [];
  return Object.entries(local as Record<string, unknown>).filter((e): e is [string, string] => typeof e[1] === "string" && !(e[0] in server));
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

export const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"] as const;
/** The mothership's caps (docs/protocol.md, "Chat images"). */
export const IMAGE_MAX_BYTES = 10 * 1024 * 1024;
export const IMAGES_PER_MESSAGE = 8;

/** Why an image file cannot be attached, or `null`. The mothership checks the bytes again. */
export function imageProblem(file: { type: string; size: number }, alreadyAttached: number): string | null {
  if (!(IMAGE_TYPES as readonly string[]).includes(file.type)) return `${file.type || "That file"} is not a supported image (png, jpeg, gif, webp).`;
  if (file.size > IMAGE_MAX_BYTES) return "Images are limited to 10 MB.";
  if (alreadyAttached >= IMAGES_PER_MESSAGE) return `At most ${IMAGES_PER_MESSAGE} images on one message.`;
  return null;
}

/** The stored images of a message, as attachments to send again (edit and resend). */
export function storedImages(notes: readonly ChatAttachmentNote[] | undefined): { sha: string; label: string }[] {
  return (notes ?? []).filter((n) => n.kind === "image" && n.sha).map((n) => ({ sha: n.sha!, label: n.label }));
}

/** Whether any message carries a stored image (the export then packs a zip). */
export function hasImages(messages: readonly ChatMessage[]): boolean {
  return messages.some((m) => storedImages(m.attachments).length > 0);
}
