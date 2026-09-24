// The model picker: a Provider menu and a Model menu side by side, in place of a free-text field.
// The value it reads and writes is the same string the settings always stored — a Claude alias or
// ID for Anthropic, `<provider>/<model>` for any other provider — so saved settings are unchanged,
// and a value it does not recognise is shown as "Unknown: …" rather than dropped.
import { createContext, useContext, useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useApi } from "../context";
import type { ModelOption, ModelProvider } from "../types";
import { IconCheck, IconChevronDown } from "./icons";
import { providerSecretId, useOpenSecrets } from "../secretsNav";
import { ProviderMark } from "./providerMark";
import { cx, inputClass } from "./ui";

/**
 * Opens Settings at a section, optionally at one model provider, from anywhere inside it. The
 * settings screen provides it; a picker outside one has no way there and hides its "Set key" link
 * unless it is given `onSetKey`.
 */
export const SettingsNavContext = createContext<((section: "providers", providerId?: string) => void) | null>(null);

export const ANTHROPIC = "anthropic";

/** The configured providers (GET /api/providers), for names, marks and key status. */
function useProviders(): ModelProvider[] | null {
  const api = useApi();
  const [providers, setProviders] = useState<ModelProvider[] | null>(null);
  useEffect(() => {
    let cancelled = false;
    api
      .providers()
      .then((list) => !cancelled && setProviders(list))
      .catch(() => !cancelled && setProviders([]));
    return () => {
      cancelled = true;
    };
  }, [api]);
  return providers;
}

export type ParsedModel = { provider: string; model: string; known: boolean };

/**
 * Splits a stored model string: `<provider>/<model>` when the prefix names a configured provider,
 * otherwise an Anthropic alias or ID. A prefix naming no configured provider is unknown, never
 * silently read as Anthropic.
 */
export function parseModel(value: string, providerIds: string[]): ParsedModel {
  if (!value) return { provider: ANTHROPIC, model: "", known: true };
  const slash = value.indexOf("/");
  if (slash > 0) {
    const provider = value.slice(0, slash);
    const model = value.slice(slash + 1);
    return { provider, model, known: providerIds.includes(provider) };
  }
  return { provider: ANTHROPIC, model: value, known: true };
}

/** The stored string for a provider and model; the empty model is the field's default. */
export function formatModel(provider: string, model: string): string {
  if (!model) return "";
  return provider === ANTHROPIC ? model : `${provider}/${model}`;
}

type Option = { value: string; label: ReactNode; text: string; hint?: string };

/** A button that opens an opaque listbox; arrows, Home/End, Enter and Escape work as in a select. */
function Menu({
  id,
  label,
  value,
  options,
  onPick,
  button,
  className,
}: {
  id?: string;
  label: string;
  value: string;
  options: Option[];
  onPick: (value: string) => void;
  button: ReactNode;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const listId = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLUListElement>(null);

  const show = () => {
    const at = options.findIndex((o) => o.value === value);
    setActive(at < 0 ? 0 : at);
    setOpen(true);
  };
  const close = (refocus: boolean) => {
    setOpen(false);
    if (refocus) trigger.current?.focus();
  };
  const pick = (v: string) => {
    onPick(v);
    close(true);
  };

  useEffect(() => {
    if (open) list.current?.focus();
  }, [open]);
  useEffect(() => {
    if (!open) return;
    list.current?.querySelector<HTMLElement>(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [open, active]);

  const onListKey = (e: KeyboardEvent<HTMLUListElement>) => {
    const last = options.length - 1;
    if (e.key === "ArrowDown") setActive((i) => Math.min(last, i + 1));
    else if (e.key === "ArrowUp") setActive((i) => Math.max(0, i - 1));
    else if (e.key === "Home") setActive(0);
    else if (e.key === "End") setActive(last);
    else if (e.key === "Enter" || e.key === " ") {
      if (options[active]) pick(options[active].value);
    } else if (e.key === "Escape") close(true);
    else if (e.key === "Tab") close(false);
    else return;
    e.preventDefault();
  };

  return (
    <div className={cx("relative min-w-0", className)}>
      <button
        ref={trigger}
        id={id}
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        aria-label={label}
        onClick={() => (open ? close(false) : show())}
        onKeyDown={(e) => {
          if ((e.key === "ArrowDown" || e.key === "ArrowUp") && !open) {
            e.preventDefault();
            show();
          }
        }}
        className={cx(inputClass, "flex w-full cursor-pointer items-center gap-2 text-left")}
      >
        <span className="flex min-w-0 flex-1 items-center gap-2">{button}</span>
        <IconChevronDown size={14} className="shrink-0 text-faint" />
      </button>
      {open && (
        <>
          <div aria-hidden="true" className="fixed inset-0 z-40" onClick={() => close(false)} />
          <ul
            ref={list}
            id={listId}
            role="listbox"
            aria-label={label}
            tabIndex={-1}
            aria-activedescendant={`${listId}-${active}`}
            onKeyDown={onListKey}
            className="scroll-thin absolute left-0 right-0 top-[calc(100%+4px)] z-50 max-h-[280px] min-w-[220px] overflow-y-auto rounded-lg border border-border-strong bg-panel p-1 shadow-[0_12px_32px_rgb(0_0_0/0.35)] focus:outline-none"
          >
            {options.map((option, i) => {
              const selected = option.value === value;
              return (
                <li
                  key={option.value}
                  id={`${listId}-${i}`}
                  data-index={i}
                  role="option"
                  aria-selected={selected}
                  onMouseEnter={() => setActive(i)}
                  onClick={() => pick(option.value)}
                  className={cx(
                    "flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-[13px]",
                    i === active ? "bg-panel-2 text-text" : "text-muted",
                  )}
                >
                  <span className="flex min-w-0 flex-1 items-center gap-2">{option.label}</span>
                  {option.hint && <span className="shrink-0 text-[11.5px] text-faint">{option.hint}</span>}
                  <span className="w-3.5 shrink-0 text-accent">{selected && <IconCheck size={14} />}</span>
                </li>
              );
            })}
          </ul>
        </>
      )}
    </div>
  );
}

const CUSTOM = "\u0000custom";

export function ModelPicker({
  value,
  onChange,
  models,
  emptyLabel = "Default",
  ariaLabel,
  id,
  onSetKey,
}: {
  value: string;
  onChange: (value: string) => void;
  /** GET /api/models: every model the pickers know, tagged with its provider. */
  models: ModelOption[];
  /** What the empty value means for this field, e.g. "Same as orchestrator". */
  emptyLabel?: string;
  ariaLabel?: string;
  id?: string;
  /** Where "Set key" goes; inside the settings screen it defaults to the key's row on the Secrets page. */
  onSetKey?: (providerId: string) => void;
}) {
  const providers = useProviders();
  const nav = useContext(SettingsNavContext);
  const openSecrets = useOpenSecrets();
  const setKey =
    onSetKey ??
    (nav && openSecrets
      ? (providerId: string) => openSecrets(providerSecretId(providerId))
      : nav
        ? (providerId: string) => nav("providers", providerId)
        : null);
  const [custom, setCustom] = useState(false);
  const name = ariaLabel ?? "Model";

  const ids = (providers ?? []).map((p) => p.id);
  const parsed = parseModel(value, ids);
  // Before the provider list arrives, a prefixed value is not yet known to be unknown.
  const unknownProvider = providers !== null && !parsed.known;
  const provider = providers?.find((p) => p.id === parsed.provider) ?? null;
  const providerName = parsed.provider === ANTHROPIC ? "Anthropic" : (provider?.name ?? parsed.provider);
  const needsKey = provider !== null && !provider.has_key;

  const modelsFor = (pid: string): string[] => {
    const listed = models.filter((m) => m.provider === pid).map((m) => (pid === ANTHROPIC ? m.id : m.id.replace(`${pid}/`, "")));
    const own = pid === ANTHROPIC ? [] : (providers?.find((p) => p.id === pid)?.models ?? []);
    return [...new Set([...listed, ...own])];
  };
  // A non-Anthropic label repeats the provider ("qwen3.8-max · Alibaba Bailian"); the menu beside it
  // already says which provider, so only the model part is kept.
  const labelFor = (pid: string, model: string): string => {
    const label = models.find((m) => m.id === formatModel(pid, model))?.label ?? model;
    return pid === ANTHROPIC ? label : label.split(" · ")[0];
  };

  const providerOptions: Option[] = [
    { value: ANTHROPIC, text: "Anthropic", label: <Mark id={ANTHROPIC} name="Anthropic" preset="anthropic" /> },
    ...(providers ?? []).map((p) => ({
      value: p.id,
      text: p.name,
      label: <Mark id={p.id} name={p.name} preset={p.preset} keyed={p.has_key} />,
      hint: p.has_key ? undefined : "no key",
    })),
    ...(unknownProvider ? [{ value: parsed.provider, text: parsed.provider, label: <span className="truncate">Unknown: {parsed.provider}</span> }] : []),
  ];

  const choices = modelsFor(parsed.provider);
  const modelOptions: Option[] = [
    { value: "", text: emptyLabel, label: <span className="truncate italic">{emptyLabel}</span> },
    ...choices.map((m) => ({ value: m, text: m, label: <span className="truncate">{labelFor(parsed.provider, m)}</span>, hint: labelFor(parsed.provider, m) !== m ? m : undefined })),
    ...(parsed.model && !choices.includes(parsed.model)
      ? [{ value: parsed.model, text: parsed.model, label: <span className="truncate font-mono text-[12.5px]">Unknown: {parsed.model}</span> }]
      : []),
    { value: CUSTOM, text: "Custom…", label: <span className="truncate text-muted">Custom…</span> },
  ];

  const pickProvider = (pid: string) => {
    if (pid === parsed.provider) return;
    setCustom(false);
    // A provider's first model, so the stored value stays valid; the empty default is Anthropic's.
    onChange(pid === ANTHROPIC ? "" : formatModel(pid, modelsFor(pid)[0] ?? ""));
  };
  const pickModel = (m: string) => {
    if (m === CUSTOM) {
      setCustom(true);
      return;
    }
    setCustom(false);
    onChange(formatModel(parsed.provider, m));
  };

  const currentModel = modelOptions.find((o) => o.value === parsed.model);

  return (
    <div className="flex w-full min-w-0 flex-col gap-1.5">
      <div className="flex min-w-0 gap-1.5">
        <Menu
          label={`${name}: provider`}
          value={parsed.provider}
          options={providerOptions}
          onPick={pickProvider}
          className="w-[46%] shrink-0"
          button={
            unknownProvider ? (
              <span className="truncate text-warn">Unknown: {parsed.provider}</span>
            ) : (
              <Mark id={parsed.provider} name={providerName} preset={parsed.provider === ANTHROPIC ? "anthropic" : provider?.preset} keyed={provider ? provider.has_key : undefined} />
            )
          }
        />
        <Menu
          id={id}
          label={`${name}: model`}
          value={custom ? CUSTOM : parsed.model}
          options={modelOptions}
          onPick={pickModel}
          className="flex-1"
          button={currentModel ? currentModel.label : <span className="truncate italic">{emptyLabel}</span>}
        />
      </div>
      {custom && (
        <input
          autoFocus
          defaultValue={parsed.model}
          onChange={(e) => onChange(formatModel(parsed.provider, e.target.value.trim()))}
          onKeyDown={(e) => e.key === "Enter" && setCustom(false)}
          placeholder={parsed.provider === ANTHROPIC ? "claude-… model ID" : `${providerName} model ID`}
          aria-label={`${name}: custom model ID`}
          spellCheck={false}
          autoComplete="off"
          className={cx(inputClass, "font-mono text-[12.5px]")}
        />
      )}
      {needsKey && (
        <p className="flex items-center gap-1.5 text-[12px] text-warn">
          <span aria-hidden="true" className="size-1.5 rounded-full bg-warn" />
          {providerName} has no key yet.
          {setKey && (
            <button type="button" onClick={() => setKey(parsed.provider)} className="cursor-pointer font-medium text-accent underline-offset-2 hover:underline">
              Set key
            </button>
          )}
        </p>
      )}
    </div>
  );
}

/** A provider's mark and name, with a key dot when its key status is known. */
function Mark({ name, preset, keyed }: { id: string; name: string; preset?: string; keyed?: boolean }) {
  return (
    <>
      <ProviderMark preset={preset} name={name} size="button" />
      <span className="truncate">{name}</span>
      {keyed !== undefined && (
        <span
          role="img"
          aria-label={keyed ? "key set" : "no key"}
          className={cx("size-1.5 shrink-0 rounded-full", keyed ? "bg-ok" : "bg-warn")}
        />
      )}
    </>
  );
}
