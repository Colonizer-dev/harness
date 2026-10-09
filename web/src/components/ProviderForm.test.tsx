// The provider editor's connection policy (#605): Trusted, the model map and the disabled tools,
// rendered to static markup like the cockpit's other tests, and the pure save body pinned directly.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import type { ModelProvider } from "../types";
import { toggleModel } from "./settings/providerOverview";
import { ProviderForm, duplicateModelMapCanonicals, modelMapCanonicals, providerSaveBody, type ProviderSaveInput } from "./SettingsDialog";

const wrap = (node: React.ReactNode) => renderToStaticMarkup(<ApiContext.Provider value={{} as Api}>{node}</ApiContext.Provider>);

const provider = (overrides: Partial<ModelProvider> = {}): ModelProvider => ({
  id: "deepseek",
  name: "DeepSeek",
  base_url: "https://api.deepseek.com/anthropic",
  auth: "x-api-key",
  wire: "anthropic",
  has_key: true,
  models: ["deepseek-flash"],
  preset: "deepseek",
  trusted: false,
  timeout_secs: 600,
  max_concurrent: null,
  queue_timeout_secs: null,
  context_tokens: null,
  fallback_model: null,
  in_flight: 0,
  queued: 0,
  ...overrides,
});

const form = (initial: ModelProvider) =>
  wrap(<ProviderForm initial={initial} preset={initial.preset} takenIds={[]} onCancel={() => {}} onSaved={() => {}} />);

const bodyInput = (overrides: Partial<ProviderSaveInput> = {}): ProviderSaveInput => ({
  name: "  DeepSeek  ",
  base_url: "  https://api.deepseek.com/anthropic  ",
  auth: "x-api-key",
  wire: "anthropic",
  models: ["deepseek-flash"],
  preset: "deepseek",
  quota: { url: "  https://api.deepseek.com/plan ", pointer: " /data/remaining " },
  timeout_secs: 600,
  max_concurrent: null,
  queue_timeout_secs: null,
  context_tokens: null,
  fallback_model: null,
  trusted: true,
  model_map: [{ canonical: " sonnet ", wire: " claude-wire-sonnet " }],
  disabled_tools: ["WebSearch"],
  ...overrides,
});

describe("provider connection policy", () => {
  it("shows the Trusted switch on for a trusted provider", () => {
    const html = form(provider({ trusted: true }));
    expect(html).toContain('role="switch"');
    expect(html).toContain('aria-checked="true"');
    expect(html).toContain("Trusted");
  });

  it("shows it off for an untrusted provider", () => {
    expect(form(provider({ trusted: false }))).toContain('aria-checked="false"');
  });

  it("prefills the model map rows and the disabled-tool chips", () => {
    const html = form(provider({ model_map: { sonnet: "claude-wire-sonnet" }, disabled_tools: ["WebSearch", "Bash"] }));
    expect(html).toContain('value="sonnet"');
    expect(html).toContain('value="claude-wire-sonnet"');
    expect(html).toContain("WebSearch");
    expect(html).toContain("Bash");
    // A map with rows loses the empty-map hint.
    expect(html).not.toContain("No mappings");
  });

  it("round-trips the three fields in the save body", () => {
    const body = providerSaveBody(bodyInput());
    expect(body.trusted).toBe(true);
    expect(body.model_map).toEqual({ sonnet: "claude-wire-sonnet" });
    expect(body.disabled_tools).toEqual(["WebSearch"]);
    expect(body.name).toBe("DeepSeek");
    expect(body.base_url).toBe("https://api.deepseek.com/anthropic");
    expect(body.quota).toEqual({ url: "https://api.deepseek.com/plan", pointer: "/data/remaining" });
    // The limit pointer round-trips trimmed too, and stays off the body when blank.
    expect(providerSaveBody(bodyInput({ quota: { url: "https://api.deepseek.com/plan", pointer: "/data/remaining", limit_pointer: " /data/total " } })).quota).toEqual({
      url: "https://api.deepseek.com/plan",
      pointer: "/data/remaining",
      limit_pointer: "/data/total",
    });
  });

  it("keeps a blank wire name, dropping only rows whose canonical is blank", () => {
    const body = providerSaveBody(
      bodyInput({
        model_map: [
          { canonical: "qwen3", wire: "" },
          { canonical: "  ", wire: "x" },
          { canonical: " sonnet ", wire: " claude-wire-sonnet " },
        ],
      }),
    );
    // A blank wire is a real entry: the gateway sends the canonical name and counts it in the allowlist.
    expect(body.model_map).toEqual({ qwen3: "", sonnet: "claude-wire-sonnet" });
  });

  it("round-trips a prefilled {qwen3: ''} unchanged, and an all-blank editor as an empty map", () => {
    const prefilled = Object.entries({ qwen3: "" }).map(([canonical, wire]) => ({ canonical, wire }));
    expect(providerSaveBody(bodyInput({ model_map: prefilled })).model_map).toEqual({ qwen3: "" });
    expect(providerSaveBody(bodyInput({ model_map: [{ canonical: "", wire: "" }] })).model_map).toEqual({});
  });

  it("flags duplicate canonical names, which a saved map would collapse last-wins", () => {
    expect(duplicateModelMapCanonicals([{ canonical: "sonnet", wire: "a" }, { canonical: " sonnet ", wire: "b" }])).toEqual(["sonnet"]);
    expect(duplicateModelMapCanonicals([{ canonical: "sonnet", wire: "a" }, { canonical: "", wire: "" }, { canonical: "", wire: "" }])).toEqual([]);
    expect(modelMapCanonicals([{ canonical: " sonnet ", wire: "" }, { canonical: "  ", wire: "x" }])).toEqual(["sonnet"]);
  });
});

// Issue #1223: a catalogue entry's plan-balance preset prefills the Plan balance fields on create.
describe("provider plan-balance preset", () => {
  it("prefills the MiniMax international quota fields on create, and none without a preset", () => {
    const add = (preset: string) => wrap(<ProviderForm preset={preset} takenIds={[]} onCancel={() => {}} onSaved={() => {}} />);
    expect(add("minimax-en")).toContain('value="https://api.minimax.io/v1/api/openplatform/coding_plan/remains"');
    expect(add("minimax-en")).toContain('value="/data/model_remains/0/current_interval_usage_count"');
    expect(add("minimax-en")).toContain('value="/data/model_remains/0/current_interval_total_count"');
    expect(add("minimax")).not.toContain("coding_plan/remains");
  });

  it("keeps a saved probe when editing, so its pointers survive the save", () => {
    const html = form(
      provider({
        quota: {
          url: "https://api.minimax.io/v1/api/openplatform/coding_plan/remains",
          pointer: "/data/model_remains/0/current_interval_usage_count",
          limit_pointer: "/data/model_remains/0/current_interval_total_count",
        },
      }),
    );
    expect(html).toContain('value="https://api.minimax.io/v1/api/openplatform/coding_plan/remains"');
    expect(html).toContain('value="/data/model_remains/0/current_interval_usage_count"');
  });
});

// Issue #1167: the edit form lists the endpoint's models as checkboxes beside the free-typed chips —
// "new" just appeared upstream, "gone" is enabled though the endpoint no longer names it, and
// unticking is the only way a model leaves the save.
describe("provider model checklist", () => {
  const discovered = provider({
    models: ["deepseek-flash", "deepseek-vintage"],
    discovered_models: ["deepseek-flash", "deepseek-v4-pro"],
    new_models: ["deepseek-v4-pro"],
    discovered_at: "2026-10-09T10:00:00Z",
  });

  it("renders the endpoint's models as checkboxes, flagged new and gone, with when they were found", () => {
    const html = form(discovered);
    expect(html.split('type="checkbox"').length - 1).toBe(3);
    expect(html).toContain(">deepseek-v4-pro</span>");
    expect(html).toContain(">new</span>");
    expect(html).toContain(">gone</span>");
    expect(html).toMatch(/type="checkbox"[^>]*checked/);
    expect(html).toContain("Available on the endpoint, discovered");
  });

  it("stays hidden while the endpoint and the catalogue have nothing to offer", () => {
    expect(form(provider({ preset: "custom", models: [], discovered_models: [] }))).not.toContain('type="checkbox"');
  });

  it("flips a checkbox in and out of the saved models, never duplicating one already there", () => {
    expect(toggleModel(["deepseek-flash"], "deepseek-v4-pro", true)).toEqual(["deepseek-flash", "deepseek-v4-pro"]);
    expect(toggleModel(["deepseek-flash", "deepseek-vintage"], "deepseek-vintage", false)).toEqual(["deepseek-flash"]);
  });
});
