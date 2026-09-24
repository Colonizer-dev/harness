import { describe, expect, it } from "vitest";
import type { ChatMessage, ChatMeta, ChatModels } from "../../types";
import { filterItems } from "./Popover";
import {
  candidatesByParent,
  chatMatches,
  dayBucket,
  estimateTokens,
  groupChats,
  hasImages,
  IMAGE_MAX_BYTES,
  IMAGES_PER_MESSAGE,
  imageProblem,
  inputCost,
  legacyEntries,
  loadPersonas,
  looksLikePath,
  modelEntries,
  modelTags,
  parseSlash,
  personaEdit,
  pricingOf,
  searchMessages,
  slashMatches,
  storedImages,
  visionCapable,
} from "./logic";

const meta = (id: string, updated_at: string, extra: Partial<ChatMeta> = {}): ChatMeta => ({
  id,
  title: `chat ${id}`,
  model: "zai/glm-5.3-flash",
  max_tokens: 4096,
  created_at: updated_at,
  updated_at,
  ...extra,
});

const msg = (id: string, role: "user" | "assistant", content: string, extra: Partial<ChatMessage> = {}): ChatMessage => ({
  id,
  role,
  content,
  ts: "2026-09-24T00:00:00Z",
  input_tokens: 0,
  output_tokens: 0,
  stopped: false,
  ...extra,
});

const models: ChatModels = {
  default: "zai/glm-5.3-flash",
  claude: { available: false, reason: "needs a key" },
  providers: [
    { id: "zai", name: "Z.AI", models: ["glm-5.3-flash"], wire: "anthropic", has_key: true, pricing: { input_per_mtok: 0.6, output_per_mtok: 2.2 } },
    { id: "ds", name: "DeepSeek", models: ["deepseek-chat"], wire: "openai", has_key: false, pricing: null },
  ],
};

describe("conversation list", () => {
  const now = new Date(2026, 8, 24, 15, 0);
  it("buckets by local day", () => {
    expect(dayBucket(new Date(2026, 8, 24, 1).toISOString(), now)).toBe("Today");
    expect(dayBucket(new Date(2026, 8, 23, 23).toISOString(), now)).toBe("Yesterday");
    expect(dayBucket(new Date(2026, 8, 19).toISOString(), now)).toBe("Previous 7 days");
    expect(dayBucket(new Date(2026, 7, 1).toISOString(), now)).toBe("Older");
  });

  it("puts pinned first, then dated groups newest first, dropping empty groups", () => {
    const groups = groupChats(
      [
        meta("a", new Date(2026, 7, 1).toISOString()),
        meta("b", new Date(2026, 8, 24, 9).toISOString()),
        meta("c", new Date(2026, 8, 24, 12).toISOString()),
        meta("d", new Date(2026, 7, 2).toISOString(), { pinned: true }),
      ],
      now,
    );
    expect(groups.map((g) => g.label)).toEqual(["Pinned", "Today", "Older"]);
    expect(groups[1].chats.map((c) => c.id)).toEqual(["c", "b"]);
  });

  it("searches title, model and persona, and filters by workspace", () => {
    const c = meta("x", "2026-09-24T00:00:00Z", { title: "Flaky test", persona: "Code reviewer", workspace: "acme" });
    expect(chatMatches(c, "flaky", null)).toBe(true);
    expect(chatMatches(c, "REVIEWER", null)).toBe(true);
    expect(chatMatches(c, "glm", null)).toBe(true);
    expect(chatMatches(c, "", "ACME")).toBe(true);
    expect(chatMatches(c, "", "octo")).toBe(false);
    expect(chatMatches(c, "nothing", null)).toBe(false);
  });
});

describe("slash commands", () => {
  it("offers commands matching what is typed, only for a lone /word", () => {
    expect(slashMatches("/")?.length).toBe(6);
    expect(slashMatches("/c")?.map((c) => c.name)).toEqual(["colony", "clear"]);
    expect(slashMatches("/mo")?.map((c) => c.name)).toEqual(["model"]);
    expect(slashMatches("hello /model")).toBeNull();
    expect(slashMatches("/model now")).toBeNull();
  });
  it("parses a complete command", () => {
    expect(parseSlash(" /Model ")).toBe("model");
    expect(parseSlash("/nope")).toBeNull();
  });
});

describe("models", () => {
  it("knows which models can see images", () => {
    expect(visionCapable("claude-sonnet-5", models)).toBe(true);
    expect(visionCapable("zai/glm-5.3-flash", models)).toBe(true);
    expect(visionCapable("ds/deepseek-chat", models)).toBe(false);
  });

  it("lists provider models with key status, and Claude ones flagged when unreachable", () => {
    const entries = modelEntries(models, [{ id: "claude-haiku-4-5", label: "claude-haiku-4-5" }]);
    expect(entries.map((e) => e.id)).toEqual(["zai/glm-5.3-flash", "ds/deepseek-chat", "claude-haiku-4-5"]);
    expect(entries[1].hasKey).toBe(false);
    expect(entries[2].disabled).toBe("needs a key");
    expect(modelEntries(null)).toEqual([]);
  });

  it("prices only priced providers", () => {
    expect(pricingOf("zai/glm-5.3-flash", models)?.input_per_mtok).toBe(0.6);
    expect(pricingOf("ds/deepseek-chat", models)).toBeNull();
    expect(inputCost(1_000_000, { input_per_mtok: 0.6 })).toBeCloseTo(0.6);
    expect(inputCost(10, null)).toBeNull();
    expect(estimateTokens("abcdefgh")).toBe(2);
  });

  it("tags models by their names", () => {
    expect(modelTags("zai/glm-5.3-flash")).toEqual(expect.arrayContaining(["fast", "cheap"]));
    expect(modelTags("claude-opus-5-5")).toContain("strong");
  });
});

describe("messages", () => {
  it("recognises repository paths in inline code", () => {
    expect(looksLikePath("src/main.rs")).toBe(true);
    expect(looksLikePath("crates/colonizer/src/chat.rs:42")).toBe(true);
    expect(looksLikePath("Cargo.toml")).toBe(true);
    expect(looksLikePath("README")).toBe(false);
    expect(looksLikePath("package.json")).toBe(true);
    expect(looksLikePath("let x = 1")).toBe(false);
    expect(looksLikePath("a/b")).toBe(false);
  });

  it("finds messages containing every word", () => {
    const ms = [msg("1", "user", "The login test is flaky"), msg("2", "assistant", "It races on login"), msg("3", "user", "thanks")];
    expect(searchMessages(ms, "login")).toEqual(["1", "2"]);
    expect(searchMessages(ms, "flaky LOGIN")).toEqual(["1"]);
    expect(searchMessages(ms, "  ")).toEqual([]);
  });

  it("groups compare candidates under their question, by lane", () => {
    const ms = [
      msg("u", "user", "q"),
      msg("b", "assistant", "B", { candidate: true, lane: 1, parent_id: "u" }),
      msg("a", "assistant", "A", { candidate: true, lane: 0, parent_id: "u" }),
      msg("x", "assistant", "kept", { parent_id: "u" }),
    ];
    expect(candidatesByParent(ms).get("u")?.map((m) => m.id)).toEqual(["a", "b"]);
  });
});

describe("personas", () => {
  it("applies the mothership's edits and forgets one set back to its default", () => {
    expect(loadPersonas({}).map((p) => p.name)).toEqual(["Plain", "Code reviewer", "Architect", "Release writer"]);
    expect(loadPersonas({ reviewer: "Be terse." }).find((p) => p.id === "reviewer")?.system).toBe("Be terse.");
    const original = loadPersonas({}).find((p) => p.id === "reviewer")!.system;
    expect(personaEdit("reviewer", original)).toBeNull();
    expect(personaEdit("reviewer", "Be terse.")).toBe("Be terse.");
  });

  it("moves browser-kept edits up once, never over the mothership's", () => {
    expect(legacyEntries('{"reviewer":"old","architect":"mine","x":3}', { reviewer: "server" })).toEqual([["architect", "mine"]]);
    expect(legacyEntries("not json", {})).toEqual([]);
    expect(legacyEntries(null, {})).toEqual([]);
  });
});

describe("images", () => {
  it("checks type, size and count before uploading", () => {
    expect(imageProblem({ type: "image/png", size: 1000 }, 0)).toBeNull();
    expect(imageProblem({ type: "image/svg+xml", size: 10 }, 0)).toMatch(/not a supported image/);
    expect(imageProblem({ type: "image/jpeg", size: IMAGE_MAX_BYTES + 1 }, 0)).toMatch(/10 MB/);
    expect(imageProblem({ type: "image/webp", size: 10 }, IMAGES_PER_MESSAGE)).toMatch(/At most 8/);
  });

  it("finds a message's stored images to send again", () => {
    const notes = [
      { kind: "image", label: "cat.png", sha: "a".repeat(64) },
      { kind: "image", label: "inline, before images were stored" },
      { kind: "file", label: "x.rs" },
    ];
    expect(storedImages(notes)).toEqual([{ sha: "a".repeat(64), label: "cat.png" }]);
    expect(hasImages([msg("u", "user", "q", { attachments: notes })])).toBe(true);
    expect(hasImages([msg("u", "user", "q")])).toBe(false);
  });
});

describe("popover search", () => {
  it("matches every word across label, group and keywords", () => {
    const items = [
      { id: "zai/glm", label: "glm-5.3-flash", group: "Z.AI", keywords: "fast cheap" },
      { id: "claude", label: "claude-opus-5-5", group: "Anthropic", keywords: "strong" },
    ];
    expect(filterItems(items, "z.ai fast").map((i) => i.id)).toEqual(["zai/glm"]);
    expect(filterItems(items, "anthropic").map((i) => i.id)).toEqual(["claude"]);
    expect(filterItems(items, "")).toHaveLength(2);
  });
});
