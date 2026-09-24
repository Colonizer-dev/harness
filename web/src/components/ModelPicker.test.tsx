import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import type { ModelOption } from "../types";
import { ModelPicker, formatModel, parseModel } from "./ModelPicker";

const api = { providers: () => new Promise(() => {}) } as unknown as Api;
const wrap = (node: React.ReactNode) => renderToStaticMarkup(<ApiContext.Provider value={api}>{node}</ApiContext.Provider>);

const MODELS: ModelOption[] = [
  { id: "opus", label: "Claude Opus (latest)", provider: "anthropic" },
  { id: "bailian/qwen3.8-max", label: "qwen3.8-max · Alibaba Bailian (Token Plan)", provider: "bailian" },
];

describe("parseModel / formatModel", () => {
  it("reads an alias as Anthropic and a prefix naming a provider as that provider", () => {
    expect(parseModel("opus", ["bailian"])).toEqual({ provider: "anthropic", model: "opus", known: true });
    expect(parseModel("bailian/qwen3.8-max", ["bailian"])).toEqual({ provider: "bailian", model: "qwen3.8-max", known: true });
    expect(parseModel("", ["bailian"])).toEqual({ provider: "anthropic", model: "", known: true });
  });

  it("keeps a prefix naming no configured provider as unknown instead of reading it as Anthropic", () => {
    expect(parseModel("gone/m1", ["bailian"])).toEqual({ provider: "gone", model: "m1", known: false });
  });

  it("writes back the same strings the settings always stored", () => {
    expect(formatModel("anthropic", "opus")).toBe("opus");
    expect(formatModel("bailian", "qwen3.8-max")).toBe("bailian/qwen3.8-max");
    expect(formatModel("bailian", "")).toBe(""); // an empty model is the field's default
  });
});

describe("ModelPicker", () => {
  it("shows a provider menu and a model menu with the model's label", () => {
    const html = wrap(<ModelPicker value="opus" onChange={() => {}} models={MODELS} ariaLabel="Orchestrator" />);
    expect(html).toContain('aria-label="Orchestrator: provider"');
    expect(html).toContain('aria-label="Orchestrator: model"');
    expect(html).toContain("Anthropic");
    expect(html).toContain("Claude Opus (latest)");
  });

  it("shows the field's default when the value is empty", () => {
    expect(wrap(<ModelPicker value="" onChange={() => {}} models={MODELS} emptyLabel="Same as orchestrator" />)).toContain("Same as orchestrator");
  });

  it("does not call a prefixed value unknown before the provider list has loaded", () => {
    const html = wrap(<ModelPicker value="bailian/qwen3.8-max" onChange={() => {}} models={MODELS} />);
    expect(html).not.toContain("Unknown:");
    expect(html).toContain("qwen3.8-max");
  });
});
