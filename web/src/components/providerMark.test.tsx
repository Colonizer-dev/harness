// Which mark a provider wears (#1166): the vendor behind a `custom` row's base URL, the Alibaba
// family, and the three marks added from @lobehub/icons. The Rust table in providers.rs is pinned
// against the catalogue here too, since the web UI and the Mothership infer the vendor separately.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import rust from "../../../crates/colonizer/src/providers.rs?raw";
import { PROVIDER_CATALOG } from "../providerCatalog";
import { ProviderMark } from "./providerMark";
import { effectivePreset, normalizeBaseUrl } from "./settings/providerCatalog";

const html = (props: Parameters<typeof ProviderMark>[0]) => renderToStaticMarkup(<ProviderMark {...props} />);
const plug = html({ preset: "custom", name: "Mine" });

describe("ProviderMark", () => {
  it("shows the DeepSeek mark for a custom provider at DeepSeek's base URL", () => {
    const custom = html({ preset: "custom", name: "DeepSeek", baseUrl: "https://api.deepseek.com/anthropic" });
    expect(custom).toBe(html({ preset: "deepseek", name: "DeepSeek" }));
    expect(custom).not.toBe(plug);
  });

  it("matches the URL after normalising case, a trailing slash and a default port", () => {
    expect(html({ preset: "custom", name: "x", baseUrl: "HTTPS://API.DeepSeek.com:443/anthropic/" })).toBe(html({ preset: "deepseek", name: "x" }));
  });

  it("keeps the plug for an unknown URL and never overrides a chosen preset", () => {
    expect(html({ preset: "custom", name: "Mine", baseUrl: "https://llm.example.com" })).toBe(plug);
    expect(html({ preset: "zai", name: "Zed", baseUrl: "https://api.deepseek.com/anthropic" })).toBe(html({ preset: "zai", name: "Zed" }));
  });

  it("gives a catalogue-only vendor its own mark when saved as custom", () => {
    const mm = html({ preset: "custom", name: "MiniMax Coding Plan", baseUrl: "https://api.minimaxi.com/anthropic" });
    expect(mm).toBe(html({ preset: "minimax", name: "MiniMax" }));
    expect(html({ preset: "custom", name: "BytePlus Coding Plan", baseUrl: "https://ark.ap-southeast.bytepluses.com/api/coding" })).toBe(
      html({ preset: "byteplus", name: "BytePlus" }),
    );
  });

  it("shows the Alibaba mark for every qwencloud and alibaba preset", () => {
    const alibaba = html({ preset: "alibaba", name: "Alibaba" });
    expect(alibaba).toContain("<svg");
    for (const preset of ["qwencloud", "qwencloud-token-plan", "qwencloud-for-coding", "alibaba-other"]) {
      expect(html({ preset, name: "Whatever" })).toBe(alibaba);
    }
  });

  it("has artwork, not initials, for Z.AI, Zhipu, Meta and BytePlus", () => {
    for (const preset of ["zai", "zhipu-glm", "zhipu-glm-en", "meta", "byteplus"]) {
      const markup = html({ preset, name: "Zhipu AI" });
      expect(markup, preset).toContain("<svg");
      expect(markup, preset).not.toContain(">ZA<");
    }
    expect(html({ preset: "zai", name: "Z.AI" })).not.toBe(html({ preset: "meta", name: "Z.AI" }));
  });

  it("still falls back to initials for a vendor with no mark", () => {
    expect(html({ preset: "openai", name: "OpenAI" })).toContain(">OA<");
  });
});

describe("effectivePreset", () => {
  it("resolves only custom or unset presets", () => {
    expect(effectivePreset("custom", "https://api.deepseek.com/anthropic")).toBe("deepseek");
    expect(effectivePreset(undefined, "https://api.deepseek.com/anthropic")).toBe("deepseek");
    expect(effectivePreset("custom", "https://llm.example.com")).toBe("custom");
    expect(effectivePreset("zai", "https://api.deepseek.com/anthropic")).toBe("zai");
  });

  it("prefers a built-in preset over a catalogue twin and names nothing for a shared URL", () => {
    expect(effectivePreset("custom", "https://api.z.ai/api/anthropic")).toBe("zai");
    expect(effectivePreset("custom", "https://tokenhub.tencentmaas.com/plan/anthropic")).toBe("custom");
  });
});

describe("the Mothership's vendor table", () => {
  const table = [...rust.slice(rust.indexOf("const KNOWN_PRESETS")).split("];")[0].matchAll(/\(\s*"([^"]+)",\s*"([^"]+)",?\s*\)/g)].map((m) => [m[1], m[2]] as const);

  it("names the same vendor as the web UI for every URL it lists, and lists every catalogue URL the web UI resolves", () => {
    expect(table.length).toBeGreaterThan(50);
    for (const [id, url] of table) expect(effectivePreset("custom", url), url).toBe(id);
    const listed = new Set(table.map(([, url]) => normalizeBaseUrl(url)));
    for (const entry of PROVIDER_CATALOG) {
      if (effectivePreset("custom", entry.base_url) !== "custom") expect(listed.has(normalizeBaseUrl(entry.base_url)), entry.id).toBe(true);
    }
  });
});
