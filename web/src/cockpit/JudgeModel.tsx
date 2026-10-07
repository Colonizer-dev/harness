// The judge row of the header's model switcher (issue #1201): the autonomy module's `model`, the one
// that answers colonies' questions when nobody does, set from the navbar instead of deep in Settings.
// It saves on pick, through the same PUT /api/modules/autonomy the Settings field uses, with the
// module's other settings merged in — never replaced. The words, the option list and the request
// body are pure functions, so the tests pin them without a DOM.
import type { ReactElement } from "react";

import { cx } from "../components/ui";
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

const DOT: Record<HealthTone, string> = { ok: "bg-ok", warn: "bg-warn", err: "bg-err", unknown: "bg-faint" };

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
  /** The same class the role selects use, so the row matches the rest of the dropdown. */
  selectClass: string;
}

export function JudgeSection(p: JudgeSectionProps): ReactElement | null {
  if (!p.module) return null;
  const current = currentJudgeModel(p.module);
  const options = judgeOptions(p.providers);
  const tone = judgeTone(p.status);
  const known = !current || options.some((o) => o.id === current);
  const groups = [...new Set(options.map((o) => o.group))];
  const alternative = judgeFailing(p.status) ? judgeAlternative(options, current, p.models) : null;
  return (
    <div role="group" aria-label="judge model" data-judge className="mt-3 border-t border-border pt-2">
      <div className="mb-0.5 flex items-center justify-between gap-2 text-meta-lg">
        <span className="min-w-0 truncate text-muted">Judge</span>
        <span className="flex shrink-0 items-center gap-1 text-meta-sm text-faint">
          <span aria-hidden="true" data-health={tone} className={cx("size-1.5 rounded-full", DOT[tone])} />
          {judgeToneWords(tone)}
        </span>
      </div>
      <p className="m-0 mb-1 text-meta text-faint">Answers colonies&apos; questions for you</p>
      <select
        aria-label="Judge model"
        value={current}
        disabled={p.saving}
        onChange={(e) => p.onPick(e.target.value)}
        className={p.selectClass}
      >
        {!current && <option value="">No model set</option>}
        {!known && <option value={current}>{current}</option>}
        {groups.map((g) => (
          <optgroup key={g} label={g}>
            {options
              .filter((o) => o.group === g)
              .map((o) => (
                <option key={o.id} value={o.id}>
                  {o.label}
                </option>
              ))}
          </optgroup>
        ))}
      </select>
      {alternative && (
        <p data-judge-suggestion className="m-0 mt-1 text-small-lg text-warn">
          The judge is failing.{" "}
          <button
            type="button"
            disabled={p.saving}
            onClick={() => p.onPick(alternative.id)}
            className="cursor-pointer border-0 bg-transparent p-0 font-medium text-accent underline-offset-2 hover:underline disabled:opacity-50"
          >
            Switch to {alternative.id}
          </button>
        </p>
      )}
      {p.error && (
        <p role="alert" className="m-0 mt-1 whitespace-pre-line text-small text-err">
          {p.error}{" "}
          {p.onSaveAnyway && (
            <button type="button" onClick={p.onSaveAnyway} className="cursor-pointer border-0 bg-transparent p-0 font-medium text-accent underline-offset-2 hover:underline">
              Save anyway
            </button>
          )}
        </p>
      )}
    </div>
  );
}
