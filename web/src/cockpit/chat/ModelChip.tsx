// The model chip: which model answers, opening a searchable picker grouped by provider, with each
// provider's mark, whether its key is stored, its price per million tokens and rough tags.
import { useMemo, useRef, useState, type ReactElement } from "react";
import { ProviderMark } from "../../components/providerMark";
import { IconChevronDown } from "../../components/icons";
import { cx } from "../../components/ui";
import type { ChatModels, ProviderPreset } from "../../types";
import { Popover, SearchList, type ListItem } from "./Popover";
import { modelEntries, modelTags, pricingOf, providerOf } from "./logic";

const TAG_TONE = { fast: "text-ok", cheap: "text-info", strong: "text-accent" } as const;

export function markFor(model: string, models: ChatModels | null): { preset?: ProviderPreset | "anthropic"; name: string } {
  const p = providerOf(model, models);
  if (p) return { preset: p.preset as ProviderPreset | undefined, name: p.name };
  return { preset: "anthropic", name: "Anthropic" };
}

export function shortModel(model: string): string {
  return model.includes("/") ? model.slice(model.indexOf("/") + 1) : model;
}

function price(model: string, models: ChatModels | null): string | null {
  const p = pricingOf(model, models);
  if (!p) return null;
  const f = (n: number) => (n >= 10 ? n.toFixed(0) : n >= 1 ? n.toFixed(1) : n.toFixed(2));
  return `$${f(p.input_per_mtok)} / $${f(p.output_per_mtok)}`;
}

export function ModelPopoverList({
  value,
  models,
  claudeIds,
  onPick,
  exclude,
}: {
  value: string;
  models: ChatModels | null;
  claudeIds: readonly { id: string; label: string }[];
  onPick: (model: string) => void;
  exclude?: string | null;
}): ReactElement {
  const items = useMemo<ListItem[]>(
    () =>
      modelEntries(models, claudeIds)
        .filter((e) => e.id !== exclude)
        .map((e) => {
          const cost = price(e.id, models);
          const tags = modelTags(e.id);
          return {
            id: e.id,
            label: e.label,
            group: e.provider,
            keywords: `${e.provider} ${tags.join(" ")}`,
            disabled: e.disabled,
            leading: <ProviderMark preset={e.preset as ProviderPreset | "anthropic" | undefined} name={e.provider} size="button" />,
            hint: cost ? `${cost} per 1M tokens` : undefined,
            trailing: (
              <span className="flex shrink-0 items-center gap-1.5">
                {tags.map((t) => (
                  <span key={t} className={cx("text-[10.5px] font-medium uppercase tracking-wide", TAG_TONE[t])}>
                    {t}
                  </span>
                ))}
                <span
                  title={e.hasKey ? "Key stored" : "No key stored for this provider"}
                  aria-label={e.hasKey ? "key stored" : "no key stored"}
                  className={cx("size-1.5 rounded-full", e.hasKey ? "bg-ok" : "bg-warn")}
                />
              </span>
            ),
          };
        }),
    [models, claudeIds, exclude],
  );
  return (
    <SearchList
      items={items}
      selected={value}
      placeholder="Search models…"
      loading={!models}
      onPick={(i) => onPick(i.id)}
      footer={
        <div className="border-t border-border px-3 py-2 text-[11px] leading-snug text-faint">
          Claude models need an Anthropic API key or provider — never the subscription login colonies use.
        </div>
      }
    />
  );
}

export function ModelChip({
  value,
  models,
  claudeIds,
  onChange,
  label = "model",
  exclude,
  compact,
  openRef,
}: {
  value: string;
  models: ChatModels | null;
  claudeIds: readonly { id: string; label: string }[];
  onChange: (model: string) => void;
  label?: string;
  exclude?: string | null;
  compact?: boolean;
  /** Lets a slash command open this chip's popover. */
  openRef?: { current: (() => void) | null };
}): ReactElement {
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  if (openRef) openRef.current = () => setOpen(true);
  const mark = markFor(value, models);
  return (
    <>
      <button
        ref={button}
        type="button"
        aria-label={`${label}: ${value || "default"}`}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className={cx(
          "inline-flex min-w-0 max-w-[240px] cursor-pointer items-center gap-1.5 rounded-full border border-border bg-panel-2/60 py-1 pl-1 pr-2 text-[12.5px] text-text hover:border-border-strong focus-visible:border-accent focus-visible:outline-none",
          compact && "max-w-[180px]",
        )}
      >
        <ProviderMark preset={mark.preset} name={mark.name} size="button" />
        <span className="truncate">{value ? shortModel(value) : "Pick a model"}</span>
        <IconChevronDown size={13} className="shrink-0 text-faint" />
      </button>
      <Popover open={open} onClose={() => setOpen(false)} anchor={button} placement="top-start" width={380} label={`Choose the ${label}`}>
        <ModelPopoverList
          value={value}
          models={models}
          claudeIds={claudeIds}
          exclude={exclude}
          onPick={(m) => {
            onChange(m);
            setOpen(false);
            button.current?.focus();
          }}
        />
      </Popover>
    </>
  );
}
