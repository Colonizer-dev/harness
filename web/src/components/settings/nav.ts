// The settings information architecture, as plain data: the groups, the pages inside them, the
// URL slug of each, and the search index. Pure on purpose (no React), so the route parser, the
// sidebar and the tests all read the same table.
//
// Pages are the existing section ids (`SectionId`); this file only decides where each one lives and
// what it is called in plain words. `module:<kind>` pages are placed by their kind, and `org:<name>`
// pages all sit under Workspaces.
import type { SectionId } from "./ui";

export type GroupId = "general" | "models" | "connections" | "runtime" | "devices" | "fleet" | "workspaces" | "about";

export interface GroupInfo {
  id: GroupId;
  /** The URL segment: `/settings/<slug>/<page>`. */
  slug: string;
  label: string;
  /** One line under the label, in plain words. */
  blurb: string;
  /** A name in settingsGuide's icon set. */
  icon: string;
}

/** The groups, in the order the sidebar lists them. */
export const GROUPS: readonly GroupInfo[] = [
  { id: "general", slug: "general", label: "General", blurb: "Your cockpit, setup and updates", icon: "sliders" },
  { id: "models", slug: "models", label: "Models", blurb: "Which AI models colonies may use", icon: "spark" },
  { id: "connections", slug: "connections", label: "Connections & secrets", blurb: "GitHub, Claude, keys and tokens", icon: "plug" },
  { id: "runtime", slug: "runtime", label: "Colonies & runtime", blurb: "How and where colonies run", icon: "ant" },
  { id: "devices", slug: "devices", label: "Devices & access", blurb: "Your phone, remote access, alerts", icon: "user" },
  { id: "fleet", slug: "fleet", label: "Fleet & host", blurb: "Other machines and this one", icon: "mesh" },
  { id: "workspaces", slug: "workspaces", label: "Workspaces", blurb: "Per-organisation settings", icon: "org" },
  { id: "about", slug: "about", label: "About & privacy", blurb: "What is sent, and what this is built with", icon: "shield" },
];

export interface PageInfo {
  id: SectionId;
  group: GroupId;
  /** The URL segment after the group's. */
  slug: string;
  label: string;
  /** Plain-language one-liner, shown on the phone list and in search results. */
  hint: string;
  /** Opens a cockpit view instead of a settings pane (Secrets has its own page). */
  view?: "secrets";
}

/** The fixed pages, in the order each group lists them. Modules and workspaces are added by kind. */
export const FIXED_PAGES: readonly PageInfo[] = [
  { id: "cockpit", group: "general", slug: "cockpit", label: "Your cockpit", hint: "The address to bookmark for this cockpit" },
  { id: "setup", group: "general", slug: "setup", label: "Setup", hint: "The checklist for your first colony" },
  { id: "updates", group: "general", slug: "updates", label: "Updates", hint: "Which Colonizer this is, and whether a newer one is out" },

  { id: "providers", group: "models", slug: "providers", label: "Model providers", hint: "Claude and other model endpoints, and their keys" },

  { id: "connections", group: "connections", slug: "github-claude", label: "GitHub & Claude", hint: "The two accounts colonies need" },
  { id: "secrets", group: "connections", slug: "secrets", label: "Secrets", hint: "Every key and token Colonizer holds, in one place", view: "secrets" },
  { id: "tokens", group: "connections", slug: "api-tokens", label: "API tokens", hint: "Scoped keys for CLIs, agents and CI" },

  { id: "runtime", group: "runtime", slug: "runtime", label: "Runtime", hint: "What this machine has to run colonies" },

  { id: "remote", group: "devices", slug: "remote-access", label: "Remote access", hint: "Open this cockpit from anywhere" },
  { id: "phone", group: "devices", slug: "phone", label: "Add your phone", hint: "Pair a phone with a one-use code" },
  { id: "notifications", group: "devices", slug: "notifications", label: "Notifications", hint: "How a colony that needs you gets your attention" },
  { id: "desktop", group: "devices", slug: "desktop", label: "Desktop", hint: "Install the cockpit as an app, start at login" },

  { id: "fleet", group: "fleet", slug: "fleet", label: "Fleet", hint: "Let other machines join this one" },
  { id: "live-map", group: "fleet", slug: "live-map", label: "Live map", hint: "Show this mothership as a dot on colonizer.dev" },

  { id: "orgs", group: "workspaces", slug: "all-orgs", label: "Show or hide orgs", hint: "Choose which GitHub orgs Colonizer shows and acts on" },

  { id: "usage", group: "about", slug: "usage-data", label: "Usage data", hint: "An anonymous summary you read before anything is sent" },
  { id: "built-with", group: "about", slug: "built-with", label: "Built with", hint: "What this venture is built with" },
];

/** Module kinds that belong to Models; every other kind is part of how colonies run. */
const MODEL_MODULES = new Set(["agent", "autonomy", "burn_down", "voice"]);
/** Module kinds about the fleet and the host rather than colonies. */
const HOST_MODULES = new Set(["observability"]);

export function groupInfo(id: GroupId): GroupInfo {
  return GROUPS.find((g) => g.id === id) ?? GROUPS[0];
}

/** Which group a section lives in. */
export function groupOf(id: SectionId): GroupId {
  if (id.startsWith("org:")) return "workspaces";
  if (id.startsWith("module:")) {
    const kind = id.slice("module:".length);
    return MODEL_MODULES.has(kind) ? "models" : HOST_MODULES.has(kind) ? "fleet" : "runtime";
  }
  return FIXED_PAGES.find((p) => p.id === id)?.group ?? "general";
}

/** The page's URL segment: `providers`, `module-source`, `org-acme`. */
export function pageSlug(id: SectionId): string {
  if (id.startsWith("module:")) return `module-${id.slice("module:".length)}`;
  if (id.startsWith("org:")) return `org-${id.slice("org:".length)}`;
  return FIXED_PAGES.find((p) => p.id === id)?.slug ?? id;
}

/** The section a slug names, or null. Slugs are unique across groups, so the group is only a hint. */
export function sectionFromSlug(slug: string): SectionId | null {
  if (slug.startsWith("module-") && slug.length > 7) return `module:${slug.slice(7)}`;
  if (slug.startsWith("org-") && slug.length > 4) return `org:${slug.slice(4)}`;
  return FIXED_PAGES.find((p) => p.slug === slug)?.id ?? null;
}

/** The path for a section: `/settings/models/providers`. A section with its own view goes there. */
export function settingsPath(id: SectionId): string {
  const page = FIXED_PAGES.find((p) => p.id === id);
  if (page?.view) return `/${page.view}`;
  return `/settings/${groupInfo(groupOf(id)).slug}/${encodeURIComponent(pageSlug(id)).replace(/%3A/gi, ":")}`;
}

/** The section a `/settings/<group>/<page>` path names; `null` for `/settings` itself or nonsense. */
export function sectionFromSettingsPath(pathname: string): SectionId | null {
  const parts = pathname.split("/").filter(Boolean);
  if (parts[0] !== "settings" || parts.length < 3) return null;
  let slug = parts[2];
  try {
    slug = decodeURIComponent(slug);
  } catch {
    return null;
  }
  return sectionFromSlug(slug);
}

// ---------------------------------------------------------------------------
// Status: what needs you, summed up for the sidebar's dots
// ---------------------------------------------------------------------------

/** Only two states earn a dot: amber needs you, red is broken. Everything fine shows nothing. */
export type Attention = "warn" | "err";

export function worstAttention(list: readonly (Attention | null | undefined)[]): Attention | null {
  if (list.includes("err")) return "err";
  if (list.includes("warn")) return "warn";
  return null;
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

export interface SearchEntry {
  /** The page this lands on. */
  section: SectionId;
  /** What it is called: a page title or a field's label. For a field, also what the page highlights. */
  label: string;
  /** One plain sentence. */
  help: string;
  /** Extra words a person might type, beyond the label. */
  keywords: readonly string[];
  /** A field inside a page, rather than the page itself. */
  field: boolean;
}

export interface SearchHit {
  entry: SearchEntry;
  score: number;
  /** `Models › Model providers › MiniMax` */
  crumbs: string[];
}

/** Words people use for the same thing: typing any of them finds entries tagged with the others. */
const SYNONYMS: readonly (readonly string[])[] = [
  ["key", "token", "secret", "api", "password", "credential", "credentials"],
  ["phone", "mobile", "iphone", "android", "pair", "qr"],
  ["notify", "notification", "notifications", "alert", "alerts", "sound", "push"],
  ["model", "models", "llm", "provider", "providers", "claude", "anthropic"],
  ["remote", "tunnel", "relay", "anywhere", "tailnet"],
  ["update", "updates", "upgrade", "version", "release"],
  ["privacy", "telemetry", "usage", "analytics"],
  ["sandbox", "microvm", "vm", "container", "runtime"],
  ["github", "git", "repo", "repository", "issues"],
];

function expand(word: string): string[] {
  const group = SYNONYMS.find((g) => g.includes(word));
  return group ? [...group] : [word];
}

/** Whether every character of `needle` appears in `text`, in order — a typo-tolerant last resort. */
function subsequence(needle: string, text: string): boolean {
  if (needle.length < 3) return false;
  let at = 0;
  for (const ch of text) if (ch === needle[at] && ++at === needle.length) return true;
  return false;
}

function wordStart(text: string, word: string): boolean {
  return text.startsWith(word) || text.includes(` ${word}`);
}

/**
 * Entries matching the query, best first. Every word of the query must match somewhere: the label
 * counts most (a word that starts it most of all), then the extra keywords, then the help text, then
 * the breadcrumb. A word may match through a synonym ("token" finds the key entries) or, for the
 * label alone, as a subsequence ("provders"). An empty query matches nothing.
 */
export function searchSettings(entries: readonly SearchEntry[], crumbsOf: (e: SearchEntry) => string[], query: string, limit = 12): SearchHit[] {
  const words = query.toLowerCase().split(/[\s,]+/).filter(Boolean);
  if (words.length === 0) return [];
  const hits: SearchHit[] = [];
  for (const entry of entries) {
    const crumbs = crumbsOf(entry);
    const label = entry.label.toLowerCase();
    const keywords = entry.keywords.join(" ").toLowerCase();
    const help = entry.help.toLowerCase();
    const trail = crumbs.join(" ").toLowerCase();
    let score = 0;
    let all = true;
    for (const word of words) {
      let best = 0;
      for (const alt of expand(word)) {
        const exact = alt === word;
        const weight = exact ? 1 : 0.6;
        if (label === alt) best = Math.max(best, 10 * weight);
        else if (wordStart(label, alt)) best = Math.max(best, 8 * weight);
        else if (label.includes(alt)) best = Math.max(best, 5 * weight);
        if (wordStart(keywords, alt)) best = Math.max(best, 4 * weight);
        else if (keywords.includes(alt)) best = Math.max(best, 3 * weight);
        if (help.includes(alt)) best = Math.max(best, 2 * weight);
        if (trail.includes(alt)) best = Math.max(best, 1 * weight);
      }
      if (best === 0 && subsequence(word, label)) best = 1.5;
      if (best === 0) {
        all = false;
        break;
      }
      score += best;
    }
    // A page ranks just above its own fields, so "providers" lands on the page first.
    if (all) hits.push({ entry, score: score + (entry.field ? 0 : 0.5), crumbs });
  }
  return hits.sort((a, b) => b.score - a.score || a.crumbs.join().localeCompare(b.crumbs.join())).slice(0, limit);
}

const page = (section: SectionId, label: string, help: string, keywords: string[] = []): SearchEntry => ({ section, label, help, keywords, field: false });
const field = (section: SectionId, label: string, help: string, keywords: string[] = []): SearchEntry => ({ section, label, help, keywords, field: true });

/** Fields worth jumping to on the fixed pages. Each label is the text that appears on the page. */
export const FIELD_ENTRIES: readonly SearchEntry[] = [
  field("connections", "GitHub", "Sign in with the GitHub CLI, or give a token", ["token", "login", "gh", "account"]),
  field("connections", "Claude", "Log in with your Claude subscription, or give an API key", ["token", "login", "api key", "subscription", "anthropic"]),
  field("remote", "Allow remote access", "Open this cockpit from your phone or another computer through the relay", ["tunnel", "relay", "https", "anywhere", "switch"]),
  field("remote", "Ask for GitHub sign-in first", "Visitors must sign in with GitHub before the pairing screen", ["login", "auth", "security"]),
  field("notifications", "Play a sound when a colony asks a question", "A short chime when a colony needs an answer", ["audio", "chime", "alert"]),
  field("notifications", "Browser notifications while the tab is not in front", "A desktop notification when a colony needs you", ["push", "desktop", "alert"]),
  field("notifications", "Quiet hours", "Hold notifications overnight", ["night", "mute", "do not disturb"]),
  field("notifications", "A colony asks a question", "Choose which events notify you", ["events", "webhook"]),
  field("notifications", "A colony fails", "Choose which events notify you", ["events", "error"]),
  field("notifications", "A colony opens a pull request", "Choose which events notify you", ["events", "pr"]),
  field("desktop", "Start Colonizer at login", "Have the mothership already running when you sign in to your computer", ["autostart", "boot", "launch agent"]),
  field("live-map", "Show this mothership on the live map", "One anonymous dot on colonizer.dev/live", ["telemetry", "privacy", "heartbeat"]),
  field("usage", "Allow an anonymous usage batch", "Sent at most once a day, only to an endpoint you name", ["telemetry", "privacy", "analytics", "endpoint"]),
  field("updates", "Check for new releases", "Look for a newer Colonizer in the background", ["automatic", "auto update", "version"]),
  field("tokens", "Create a token", "A scoped key for a CLI, an agent or CI, instead of the owner token", ["api key", "scope", "ci", "cli", "revoke"]),
  field("phone", "Pair a phone", "Scan a one-use code, then confirm it here", ["qr", "code", "revoke", "device"]),
  field("fleet", "Members", "Machines that joined this one", ["peer", "host", "invite", "pairing code"]),
  field("providers", "Add a provider", "Another Anthropic-compatible endpoint", ["endpoint", "base url", "key", "gateway", "minimax", "openrouter"]),
];

/** The pages' own entries, plus the fields, plus a field per setting of every loaded module. */
export function buildSearchIndex(input: {
  pages: readonly { id: SectionId; label: string; hint: string }[];
  modules?: readonly { kind: string; title: string; schema: { properties?: Record<string, { title?: string; description?: string }> } | null }[];
  providers?: readonly { name: string; models: readonly string[]; missingKey: boolean }[];
  orgs?: readonly string[];
}): SearchEntry[] {
  const entries: SearchEntry[] = input.pages.map((p) => page(p.id, p.label, p.hint, PAGE_KEYWORDS[p.id] ?? []));
  const present = new Set(input.pages.map((p) => p.id));
  for (const f of FIELD_ENTRIES) if (present.has(f.section)) entries.push(f);
  for (const m of input.modules ?? []) {
    const id = `module:${m.kind}` as SectionId;
    if (!present.has(id)) continue;
    for (const [key, spec] of Object.entries(m.schema?.properties ?? {})) {
      entries.push(field(id, spec.title ?? key, spec.description ?? `A setting of ${m.title}`, [key.replace(/_/g, " ")]));
    }
  }
  for (const p of input.providers ?? []) {
    entries.push(field("providers", p.name, p.missingKey ? "A model provider that still needs its key" : `A model provider: ${p.models.slice(0, 3).join(", ")}`, ["provider", "key", ...p.models]));
  }
  for (const org of input.orgs ?? []) {
    const id = `org:${org}` as SectionId;
    if (present.has(id)) entries.push(page(id, org, `Settings for the ${org} workspace`, ["workspace", "organisation", "organization"]));
  }
  return entries;
}

/** Words that find a page that its title alone would not. */
const PAGE_KEYWORDS: Partial<Record<SectionId, string[]>> = {
  cockpit: ["address", "url", "bookmark", "qr", "link"],
  setup: ["checklist", "first colony", "onboarding", "start"],
  connections: ["github", "claude", "login", "token", "api key", "account"],
  providers: ["models", "anthropic", "minimax", "key", "endpoint", "gateway", "fallback"],
  tokens: ["api key", "secret", "scope", "ci", "cli", "agent"],
  secrets: ["keys", "token", "password", "vault", "credential", "api key"],
  runtime: ["sandbox", "microsandbox", "microvm", "mesh", "disk", "claude binary"],
  remote: ["tunnel", "relay", "https", "tailscale", "anywhere"],
  phone: ["mobile", "pair", "qr", "ios", "android", "install"],
  notifications: ["alerts", "sound", "push", "webhook", "quiet hours"],
  desktop: ["app", "login", "autostart", "install"],
  fleet: ["peers", "machines", "join", "invite", "hosts"],
  "live-map": ["telemetry", "dot", "privacy", "colonizer.dev"],
  usage: ["telemetry", "privacy", "analytics", "anonymous"],
  updates: ["version", "release", "upgrade", "restart"],
  orgs: ["hide", "hidden", "show", "organisations", "organizations", "workspaces", "archive", "all repositories"],
  "built-with": ["stack", "licences", "licenses", "credits", "about"],
};
