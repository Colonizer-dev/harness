// The judge row of the header's model switcher (issue #1201): the autonomy module's `model`, the one
// that answers colonies' questions when nobody does, set from the navbar instead of deep in Settings.
// It saves on pick, through the same PUT /api/modules/autonomy the Settings field uses, with the
// module's other settings merged in — never replaced. The words, the option list and the request
// body are pure functions, so the tests pin them without a DOM.
import type { AutonomyStatus, ModelProvider, ModuleInfo, SwitchableModel } from "../types";
import { JUDGE_ALERT_AFTER } from "./Header";
import { modelHealth, type HealthTone } from "./ModelSwitcher";

/** The Claude aliases the judge may use, offered only while an Anthropic API connection with a key exists. */
export const ANTHROPIC_ALIASES = ["fable", "opus"] as const;

export interface JudgeOption {
  id: string;
  label: string;
  /** The group the option sits under: a provider's name, or Anthropic. */
  group: string;
}

/** Whether a configured provider reaches Anthropic's own API with a key: what `fable` / `opus` need. */
export function hasAnthropicApi(providers: readonly ModelProvider[]): boolean {
  return providers.some((p) => p.has_key && p.base_url.includes("api.anthropic.com"));
}

/** Every `provider/model` from Model providers, plus `fable` / `opus` when an Anthropic API provider has a key. */
export function judgeOptions(providers: readonly ModelProvider[]): JudgeOption[] {
  const own = providers.flatMap((p) => p.models.map((m) => ({ id: `${p.id}/${m}`, label: m, group: p.name })));
  const aliases = hasAnthropicApi(providers) ? ANTHROPIC_ALIASES.map((id) => ({ id, label: id, group: "Anthropic" })) : [];
  return [...aliases, ...own];
}

/** The judge's health dot: red once the header's warning would show, amber after a failure, green otherwise. */
export function judgeTone(status: AutonomyStatus | null | undefined): HealthTone {
  if (!status) return "unknown";
  if (status.consecutive_failures >= JUDGE_ALERT_AFTER) return "err";
  return status.consecutive_failures > 0 ? "warn" : "ok";
}

export const judgeFailing = (status: AutonomyStatus | null | undefined): boolean => judgeTone(status) === "err";

const TONE_WORDS: Record<HealthTone, string> = { ok: "answering", warn: "degraded", err: "failing", unknown: "no data yet" };
export const judgeToneWords = (tone: HealthTone): string => TONE_WORDS[tone];

/** The judge model now: the saved `model` setting, or "" when none is set. */
export function currentJudgeModel(module: ModuleInfo | null | undefined): string {
  const value = module?.settings.model;
  return typeof value === "string" ? value : "";
}

/**
 * The request body for picking `model`: the module's provider and enabled flag as they are, and its
 * saved settings with only `model` changed — the settings map is merged, never replaced.
 */
export function judgeSaveBody(module: ModuleInfo, model: string): { provider: string; enabled: boolean; settings: Record<string, unknown> } {
  return { provider: module.provider, enabled: module.enabled, settings: { ...module.settings, model } };
}

/** A healthy model to suggest while the judge is failing: not the failing one, not out of quota or degraded. */
export function judgeAlternative(options: readonly JudgeOption[], current: string, models: readonly SwitchableModel[]): JudgeOption | null {
  return options.find((o) => o.id !== current && models.some((m) => m.id === o.id) && modelHealth(o.id, models) === "ok") ?? null;
}

export interface JudgeSectionProps {
  /** The autonomy module; null until loaded (or when the mothership has none). */
  module: ModuleInfo | null;
  providers: readonly ModelProvider[];
  status: AutonomyStatus | null;
  /** The switcher's model list, for the health of a suggested alternative. */
  models: readonly SwitchableModel[];
  saving?: boolean;
  error?: string | null;
  onPick: (model: string) => void;
  /** Offered beside a refusal from the judge's test call. */
  onSaveAnyway?: () => void;
}
