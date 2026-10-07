// A provider's mark: the vendor's CC0 logo where one exists, else a lettermark. Shared by the
// provider settings and the model picker.
import type { ComponentType } from "react";
import type { ProviderPreset } from "../types";
import {
  BrandAlibabaCloud,
  BrandClaude,
  BrandDeepSeek,
  BrandGitHubCopilot,
  BrandKimi,
  BrandMiniMax,
  BrandModelScope,
  BrandNvidia,
  BrandBytePlus,
  BrandMeta,
  BrandOpenRouter,
  BrandXai,
  BrandXiaomi,
  BrandZai,
  IconPlug,
  IconServer,
  type IconProps,
} from "./icons";
import { effectivePreset } from "./settings/providerCatalog";
import { cx } from "./ui";

/**
 * What sits in a provider's tile. Vendors with a CC0 mark in the icon set get it;
 * `local` and `custom` are not brands and keep a plain glyph. Z.AI, Meta and BytePlus use
 * MIT-licensed artwork (see icons.tsx). Anything else, including OpenAI (no permissively
 * licensed artwork exists for it) and every preset added later, falls through to a lettermark
 * built from the provider's name.
 */
const PRESET_MARK: Partial<Record<ProviderPreset | "anthropic", ComponentType<IconProps>>> = {
  anthropic: BrandClaude,
  deepseek: BrandDeepSeek,
  alibaba: BrandAlibabaCloud,
  local: IconServer,
  custom: IconPlug,
  // Catalogue vendors whose mark exists under CC0; the rest fall back to initials.
  kimi: BrandKimi,
  "kimi-for-coding": BrandKimi,
  minimax: BrandMiniMax,
  modelscope: BrandModelScope,
  openrouter: BrandOpenRouter,
  "github-copilot": BrandGitHubCopilot,
  "xai-grok": BrandXai,
  nvidia: BrandNvidia,
  xiaomi: BrandXiaomi,
  zai: BrandZai,
  "zhipu-glm": BrandZai,
  "zhipu-glm-en": BrandZai,
  meta: BrandMeta,
  byteplus: BrandBytePlus,
};

/** Alibaba's endpoints are many (`qwencloud`, `qwencloud-token-plan`, `alibaba`, `qianwen-…`); they share one mark. */
const ALIBABA_PREFIX = /^(qwencloud|alibaba|qianwen)(-|$)/;

export function markOf(preset: string | undefined): ComponentType<IconProps> | undefined {
  if (!preset) return undefined;
  return PRESET_MARK[preset as ProviderPreset] ?? (ALIBABA_PREFIX.test(preset) ? BrandAlibabaCloud : undefined);
}

/** One or two initials: the capitals of the name ("OpenAI" gives OA, "Z.AI" gives ZA), else the first letters of its words. */
function initialsOf(name: string): string {
  const capitals = name.replace(/[^A-Z]/g, "");
  if (capitals.length >= 2) return capitals.slice(0, 2);
  const words = name.split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  const letters = words.map((w) => w[0]).join("").slice(0, 2).toUpperCase();
  return letters || capitals || "?";
}

/**
 * The tile at the start of a provider row or add button. Decorative: the vendor's
 * name is always beside it as text, so the tile is hidden from assistive tech.
 */
export function ProviderMark({
  preset,
  name,
  baseUrl,
  size = "row",
}: {
  preset?: ProviderPreset | "anthropic";
  name: string;
  /** Lets a provider saved as `custom` at a known vendor's URL wear that vendor's mark. */
  baseUrl?: string;
  size?: "row" | "button" | "tile";
}) {
  const Mark = markOf(effectivePreset(preset, baseUrl));
  const box = { tile: "size-11 rounded-xl", row: "size-8 rounded-lg", button: "size-[18px] rounded-[5px]" }[size];
  const glyph = { tile: 24, row: 18, button: 12 }[size];
  // Initials carry the whole tile when a vendor has no mark, so they scale with it.
  const initials = { tile: "text-lead font-semibold tracking-tight", row: "text-meta-lg font-semibold tracking-tight", button: "text-micro-xs font-bold" }[size];
  return (
    <span aria-hidden="true" className={cx("grid shrink-0 select-none place-items-center bg-panel-2 text-text", box, !Mark && initials)}>
      {Mark ? <Mark size={glyph} strokeWidth={size === "button" ? 2 : 1.75} /> : initialsOf(name)}
    </span>
  );
}
