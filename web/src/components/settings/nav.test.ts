// The settings menu's data (issue #1180): where each page lives, and the search that finds a field.
import { describe, expect, it } from "vitest";

import { FIXED_PAGES, GROUPS, buildSearchIndex, groupInfo, groupOf, searchSettings, sectionFromSlug, settingsPath, worstAttention, type SearchEntry } from "./nav";
import { providerMissingKey, providerNeedsKey } from "./providerCatalog";
import { slugify } from "./flashField";

const modules = [
  { kind: "source", title: "Source", schema: { properties: { include_labels: { title: "Include labels", description: "Only issues with these labels" } } } },
  { kind: "autonomy", title: "Autonomy", schema: null },
  { kind: "observability", title: "Observability", schema: null },
];
const providers = [
  { name: "MiniMax", models: ["minimax-m2"], missingKey: true },
  { name: "DeepSeek", models: ["deepseek-v4"], missingKey: false },
];

function index(): SearchEntry[] {
  const pages = [
    ...FIXED_PAGES.map((p) => ({ id: p.id, label: p.label, hint: p.hint })),
    ...modules.map((m) => ({ id: `module:${m.kind}` as const, label: m.title, hint: "" })),
    { id: "org:acme" as const, label: "acme", hint: "" },
  ];
  return buildSearchIndex({ pages, modules, providers, orgs: ["acme"] });
}
const crumbs = (e: SearchEntry) => [groupInfo(groupOf(e.section)).label, e.label];
const find = (query: string) => searchSettings(index(), crumbs, query);

describe("the groups", () => {
  it("are 6 to 8, each with its own slug, and every fixed page belongs to one", () => {
    expect(GROUPS.length).toBeGreaterThanOrEqual(6);
    expect(GROUPS.length).toBeLessThanOrEqual(8);
    expect(new Set(GROUPS.map((g) => g.slug)).size).toBe(GROUPS.length);
    for (const page of FIXED_PAGES) expect(GROUPS.some((g) => g.id === page.group), page.id).toBe(true);
  });

  it("place modules by what they do, and workspaces together", () => {
    expect(groupOf("module:agent")).toBe("models");
    expect(groupOf("module:autonomy")).toBe("models");
    expect(groupOf("module:source")).toBe("runtime");
    expect(groupOf("module:sandbox")).toBe("runtime");
    expect(groupOf("module:observability")).toBe("fleet");
    expect(groupOf("org:acme")).toBe("workspaces");
  });
});

describe("worstAttention", () => {
  it("is red over amber over nothing", () => {
    expect(worstAttention([])).toBeNull();
    expect(worstAttention([null, undefined])).toBeNull();
    expect(worstAttention(["warn", null])).toBe("warn");
    expect(worstAttention(["warn", "err"])).toBe("err");
  });
});

describe("searchSettings", () => {
  it("finds a field by its label", () => {
    const hit = find("quiet hours")[0];
    expect(hit.entry.label).toBe("Quiet hours");
    expect(hit.entry.section).toBe("notifications");
    expect(hit.entry.field).toBe(true);
  });

  it("finds a field by its help text and by words it was not named with", () => {
    expect(find("chime")[0].entry.section).toBe("notifications");
    expect(find("autostart").map((h) => h.entry.label)).toContain("Start Colonizer at login");
  });

  it("treats key, token, secret and api as the same thing", () => {
    for (const word of ["key", "token", "secret", "api"]) {
      const sections = find(word).map((h) => h.entry.section);
      expect(sections, word).toContain("tokens");
      expect(sections, word).toContain("secrets");
      expect(sections, word).toContain("providers");
    }
  });

  it("finds a provider by name and reports it as Models > Model providers > MiniMax", () => {
    const hit = find("minimax")[0];
    expect(hit.entry.section).toBe("providers");
    expect(hit.entry.label).toBe("MiniMax");
    expect(hit.entry.field).toBe(true);
  });

  it("finds a module's own settings", () => {
    const hit = find("include labels")[0];
    expect(hit.entry).toMatchObject({ section: "module:source", label: "Include labels", field: true });
  });

  it("ranks a page above its fields, and a word at the start of a label above one inside it", () => {
    expect(find("notifications")[0].entry).toMatchObject({ section: "notifications", field: false });
    const labels = find("sound").map((h) => h.entry.label);
    expect(labels[0]).toBe("Play a sound when a colony asks a question");
  });

  it("needs every word to match, forgives a typo in a label, and matches nothing for nothing", () => {
    expect(find("quiet unicorn")).toEqual([]);
    expect(find("provders").map((h) => h.entry.section)).toContain("providers");
    expect(find("")).toEqual([]);
    expect(find("   ")).toEqual([]);
  });

  it("finds a workspace by name", () => {
    expect(find("acme")[0].entry.section).toBe("org:acme");
  });

  it("returns the breadcrumb it was asked for", () => {
    expect(find("phone")[0].crumbs[0]).toBe("Devices & access");
  });
});

describe("every AI account lives under Models (issue #1211)", () => {
  it("renames the connections page to GitHub and keeps the old link working", () => {
    expect(FIXED_PAGES.find((p) => p.id === "connections")?.label).toBe("GitHub");
    expect(settingsPath("connections")).toBe("/settings/connections/github");
    expect(sectionFromSlug("github-claude")).toBe("connections");
    expect(sectionFromSlug("github")).toBe("connections");
  });

  it("finds the Subscriptions rows by claude, codex and login", () => {
    for (const word of ["claude", "codex", "login"]) {
      const hits = find(word);
      expect(hits.some((h) => h.entry.section === "providers"), word).toBe(true);
    }
    expect(find("claude")[0].entry.section).toBe("providers");
    expect(find("codex")[0].entry.label).toBe("Codex");
    expect(find("grok")[0].entry.section).toBe("providers");
  });

  it("no longer lists Claude as a field of the GitHub page", () => {
    expect(find("claude").some((h) => h.entry.section === "connections")).toBe(false);
  });
});

describe("slugify", () => {
  it("makes dashed lower-case words", () => {
    expect(slugify("Ask for GitHub sign-in first")).toBe("ask-for-github-sign-in-first");
    expect(slugify("  Quiet hours! ")).toBe("quiet-hours");
  });
});

describe("which providers need a key", () => {
  const p = (auth: "x-api-key" | "bearer" | "none", preset: string, has_key: boolean) => ({ auth, preset, has_key });
  it("is the rule the provider row's badge uses: not for no-auth or local providers", () => {
    expect(providerMissingKey(p("x-api-key", "minimax", false))).toBe(true);
    expect(providerMissingKey(p("bearer", "custom", false))).toBe(true);
    expect(providerMissingKey(p("x-api-key", "minimax", true))).toBe(false);
    expect(providerMissingKey(p("none", "custom", false))).toBe(false);
    expect(providerMissingKey(p("bearer", "local", false))).toBe(false);
    expect(providerNeedsKey(p("bearer", "local", false))).toBe(false);
  });
});
