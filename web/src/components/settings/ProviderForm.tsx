import { useId, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import { fillTemplate } from "../../providerCatalog";
import { providerTestToast } from "../../providerHealth";
import type { ModelProvider, ProviderAuth, ProviderPricing, ProviderPreset } from "../../types";
import { useModels } from "../../useModels";
import { Badge, Button, InfoButton, Spinner, Switch, cx, inputClass } from "../ui";
import { IconCheck, IconChevron, IconX } from "../icons";
import { Row, Code } from "./ui";
import {
  AUTH_LABEL,
  CATALOG_BY_ID,
  DEFAULT_TIMEOUT,
  PRESET_HINT,
  WIRE_LABEL,
  duplicateModelMapCanonicals,
  limitLabels,
  limitText,
  modelMapCanonicals,
  parseLimit,
  parsePrice,
  presetDraft,
  presetLabel,
  pricingDraftOf,
  pricingSummaryOf,
  providerSaveBody,
  sameWireFallbacks,
  uniqueDraft,
  type LimitKey,
  type ModelMapRow,
  type PricingDraft,
  providerNeedsKey,
} from "./providerCatalog";

// ---------------------------------------------------------------------------
// The add/edit provider form and its smaller fields.
// ---------------------------------------------------------------------------

const PROVIDER_ID = /^[a-z0-9][a-z0-9-]{0,31}$/;

/** The saved-key state badge, shown on a provider row and on the form header. */
export function KeyBadge({ provider }: { provider: ModelProvider }) {
  if (!providerNeedsKey(provider)) return <Badge>No key needed</Badge>;
  return provider.has_key ? <Badge tone="ok">Key saved</Badge> : <Badge tone="warn">No key</Badge>;
}

/** A labelled field in the provider form: label and "i" above, control below, error under. */

function FormField({
  id,
  label,
  info,
  error,
  hint,
  className,
  children,
}: {
  id: string;
  label: string;
  info?: ReactNode;
  error?: string | null;
  hint?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div className={cx("min-w-0 space-y-1", className)}>
      <div className="flex items-center gap-1">
        <label htmlFor={id} className="text-small-lg font-medium text-muted">
          {label}
        </label>
        {info && <InfoButton label={label}>{info}</InfoButton>}
      </div>
      {children}
      {error ? <span className="block text-small text-err">{error}</span> : hint ? <span className="block text-small text-faint">{hint}</span> : null}
    </div>
  );
}

/** The scheme, host and port of a base URL — what the credential's destination is pinned to. Unparsable input has none, which counts as a change. */
function originOf(url: string): string {
  try {
    return new URL(url.trim()).origin;
  } catch {
    return "";
  }
}

export function ProviderForm({
  initial,
  preset,
  peers = [],
  takenIds,
  onCancel,
  onSaved,
  onDeleted,
}: {
  initial?: ModelProvider;
  preset: ProviderPreset;
  /** The providers on file, for the same-wire fallback choices. */
  peers?: ModelProvider[];
  takenIds: string[];
  onCancel: () => void;
  onSaved: (provider: ModelProvider) => void;
  onDeleted?: (id: string) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const start = initial ?? uniqueDraft(preset, takenIds);
  const wire = initial?.wire ?? presetDraft(preset).wire;
  const [id, setId] = useState(start.id);
  const [name, setName] = useState(start.name);
  const [baseUrl, setBaseUrl] = useState(start.base_url);
  const [auth, setAuth] = useState<ProviderAuth>(start.auth);
  const [models, setModels] = useState<string[]>(start.models);
  const [keyMode, setKeyMode] = useState<"keep" | "replace" | "remove">(initial?.has_key ? "keep" : "replace");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<"save" | "delete" | null>(null);
  const [limitDraft, setLimitDraft] = useState<Record<LimitKey, string>>({
    timeout_secs: limitText(start.timeout_secs),
    max_concurrent: limitText(start.max_concurrent),
    queue_timeout_secs: limitText(start.queue_timeout_secs),
    context_tokens: limitText(start.context_tokens),
  });
  const [fallback, setFallback] = useState(start.fallback_model ?? "");
  // The saved rates sit in the fields; `pricing` only goes on the save once they differ from them, the
  // same convention as the key: omitted keeps what is saved, so a save never silently rezeros a rate.
  const [pricingDraft, setPricingDraft] = useState<PricingDraft>(() => pricingDraftOf(start.pricing));
  // The quota probe (issue #199) always goes on the save: an empty URL clears it, the way an empty
  // key string removes the key, so no keep/clear dance is needed for two plain text fields.
  const [quotaUrl, setQuotaUrl] = useState(initial?.quota?.url ?? "");
  const [quotaPointer, setQuotaPointer] = useState(initial?.quota?.pointer ?? "");
  // The connection policy (#295, #472): all three prefill from GET /api/providers and go on the save
  // as given; the model map's blank rows are dropped by `providerSaveBody`.
  const [trusted, setTrusted] = useState(initial?.trusted ?? false);
  const [modelMapRows, setModelMapRows] = useState<ModelMapRow[]>(() =>
    Object.entries(initial?.model_map ?? {}).map(([canonical, wire]) => ({ canonical, wire })),
  );
  const [disabledTools, setDisabledTools] = useState<string[]>(initial?.disabled_tools ?? []);
  // A catalogue entry whose base URL has ${…} holes: ask for them, and the URL follows.
  const template = initial ? [] : (CATALOG_BY_ID.get(preset)?.variables ?? []);
  const [vars, setVars] = useState<Record<string, string>>(() =>
    Object.fromEntries(template.map((v) => [v.name, v.default ?? ""])),
  );
  // A new Local provider opens Advanced so the prefilled limits are visible.
  const [advancedOpen, setAdvancedOpen] = useState(!initial && preset === "local");
  const anthropicModels = useModels().filter((m) => m.provider === "anthropic");
  const sameWire = sameWireFallbacks(initial?.id ?? "", wire, peers);
  const ids = {
    id: useId(),
    name: useId(),
    url: useId(),
    auth: useId(),
    key: useId(),
    models: useId(),
    fallback: useId(),
    quotaUrl: useId(),
    quotaPointer: useId(),
    trusted: useId(),
    modelMap: useId(),
    disabledTools: useId(),
  };
  const limits = {
    timeout_secs: parseLimit("timeout_secs", limitDraft.timeout_secs),
    max_concurrent: parseLimit("max_concurrent", limitDraft.max_concurrent),
    queue_timeout_secs: parseLimit("queue_timeout_secs", limitDraft.queue_timeout_secs),
    context_tokens: parseLimit("context_tokens", limitDraft.context_tokens),
  };
  const limitsInvalid = Object.values(limits).some((l) => l.error);
  const setLimit = (k: LimitKey, value: string) => setLimitDraft((d) => ({ ...d, [k]: value }));
  const pricing = {
    input_per_mtok: parsePrice(pricingDraft.input_per_mtok),
    output_per_mtok: parsePrice(pricingDraft.output_per_mtok),
    cache_read_per_mtok: parsePrice(pricingDraft.cache_read_per_mtok),
    cache_write_per_mtok: parsePrice(pricingDraft.cache_write_per_mtok),
    thinking_per_mtok: parsePrice(pricingDraft.thinking_per_mtok),
  };
  const pricingInvalid = Object.values(pricing).some((rate) => rate.error);
  const setPricing = (k: keyof ProviderPricing, value: string) => setPricingDraft((d) => ({ ...d, [k]: value }));
  const savedPricing = initial?.pricing;
  const pricingChanged =
    (pricing.input_per_mtok.value ?? 0) !== (savedPricing?.input_per_mtok ?? 0) ||
    (pricing.output_per_mtok.value ?? 0) !== (savedPricing?.output_per_mtok ?? 0) ||
    (pricing.cache_read_per_mtok.value ?? 0) !== (savedPricing?.cache_read_per_mtok ?? 0) ||
    (pricing.cache_write_per_mtok.value ?? 0) !== (savedPricing?.cache_write_per_mtok ?? 0);
  const advancedSummary = limitLabels({
    timeout_secs: limits.timeout_secs.value ?? undefined,
    max_concurrent: limits.max_concurrent.value,
    queue_timeout_secs: limits.queue_timeout_secs.value,
    context_tokens: limits.context_tokens.value,
    fallback_model: fallback || null,
  });
  const pricingSummary = pricingSummaryOf(pricing);
  const setModelMapRow = (index: number, patch: Partial<ModelMapRow>) =>
    setModelMapRows((rows) => rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
  const addModelMapRow = () => setModelMapRows((rows) => [...rows, { canonical: "", wire: "" }]);
  const removeModelMapRow = (index: number) => setModelMapRows((rows) => rows.filter((_, i) => i !== index));
  const mappingCount = new Set(modelMapCanonicals(modelMapRows)).size;
  const duplicateCanonicals = duplicateModelMapCanonicals(modelMapRows);
  // A saved map collapses duplicate canonical names last-wins, silently dropping the earlier wire name.
  const mapError = duplicateCanonicals.length
    ? `Duplicate canonical name${duplicateCanonicals.length === 1 ? "" : "s"}: ${duplicateCanonicals.join(", ")}`
    : null;
  const policySummary =
    [
      trusted ? "Trusted" : null,
      mappingCount ? `${mappingCount} model mapping${mappingCount === 1 ? "" : "s"}` : null,
      disabledTools.length ? `${disabledTools.length} disabled tool${disabledTools.length === 1 ? "" : "s"}` : null,
    ]
      .filter(Boolean)
      .join(" · ") || "Trusted routing, model map, disabled tools";

  const isNew = !initial;
  const idError = !isNew
    ? null
    : !PROVIDER_ID.test(id)
      ? "Lowercase letters, digits and dashes"
      : id === "anthropic"
        ? "“anthropic” is reserved"
        : takenIds.includes(id)
          ? "Already in use"
          : null;
  // What actually gets saved and validated: the template with its holes filled.
  const url = template.length ? fillTemplate(baseUrl, vars) : baseUrl;
  const unfilled = template.filter((v) => !vars[v.name]?.trim());
  const urlError = unfilled.length
    ? `Fill in ${unfilled.map((v) => v.label).join(" and ")}`
    : /^https?:\/\/[^\s/]+(\/\S*)?$/.test(url.trim())
      ? null
      : "An http(s) URL";
  const keyError = auth !== "none" && keyMode === "replace" && initial?.has_key && !key.trim() ? "Paste the new key" : null;
  // The saved key rides the base URL, and the server refuses a save that moves the provider to
  // another origin without re-entering it: say so here rather than after the save comes back.
  // "Keep" leaves api_key unset, and so does auth "none" — both trip the rule.
  const originMoved = Boolean(initial?.has_key && originOf(url) !== originOf(initial.base_url));
  const originKeyError =
    originMoved && (keyMode === "keep" || auth === "none")
      ? "Changing the base URL to another origin requires entering the API key again — or removing the saved key"
      : null;
  const invalid = Boolean(idError || urlError || keyError || originKeyError || limitsInvalid || pricingInvalid || mapError || !name.trim());
  const loopback = /^https?:\/\/(127\.|localhost|\[::1\])/.test(url.trim());

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (invalid) return;
    setBusy("save");
    let api_key: string | undefined;
    if (auth !== "none") {
      if (keyMode === "remove") api_key = "";
      else if (keyMode === "replace" && key.trim()) api_key = key.trim();
    }
    try {
      const saved = await api.saveProvider(
        id,
        providerSaveBody({
          name,
          base_url: url,
          auth,
          wire,
          models,
          preset: initial?.preset ?? preset,
          api_key,
          pricing: pricingChanged
            ? {
                input_per_mtok: pricing.input_per_mtok.value ?? 0,
                output_per_mtok: pricing.output_per_mtok.value ?? 0,
                cache_read_per_mtok: pricing.cache_read_per_mtok.value ?? 0,
                cache_write_per_mtok: pricing.cache_write_per_mtok.value ?? 0,
              }
            : undefined,
          quota: { url: quotaUrl, pointer: quotaPointer },
          timeout_secs: limits.timeout_secs.value,
          max_concurrent: limits.max_concurrent.value,
          queue_timeout_secs: limits.queue_timeout_secs.value,
          context_tokens: limits.context_tokens.value,
          fallback_model: fallback || null,
          trusted,
          model_map: modelMapRows,
          disabled_tools: disabledTools,
        }),
      );
      toast(`${saved.name} saved`);
      // A new provider, or one whose route or key changed, gets a one-token test through the colony's
      // own route, and the toast names the URL and status it got: a wrong base path shows here, at
      // setup, instead of in a run of failed colony turns (issue #1018).
      if (isNew || url !== initial?.base_url || wire !== initial?.wire || api_key !== undefined) {
        api
          .testProvider(saved.id)
          .then((result) => toast(providerTestToast(saved.name, result)))
          .catch((e) => toast({ title: `${saved.name}: test request failed`, body: errorMessage(e), kind: "warn" }));
      }
      onSaved(saved);
    } catch (e) {
      toast(errorMessage(e), "error");
      setBusy(null);
    }
  };

  const remove = async () => {
    if (!initial || !window.confirm(`Remove ${initial.name}? Colonies using ${initial.id}/… models will fall back to the default model.`)) return;
    setBusy("delete");
    try {
      await api.deleteProvider(initial.id);
      toast(`${initial.name} removed`);
      onDeleted?.(initial.id);
    } catch (e) {
      toast(errorMessage(e), "error");
      setBusy(null);
    }
  };

  return (
    <form onSubmit={save} className="space-y-3 rounded-xl border border-accent/40 bg-panel p-3.5">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-body font-semibold">{isNew ? `New ${presetLabel(preset)} provider` : `Edit ${initial.name}`}</span>
        {!isNew && <KeyBadge provider={initial} />}
        {wire === "openai" && (
          <Badge tone="info" title="Speaks the OpenAI protocol; the Mothership gateway translates">
            {WIRE_LABEL.openai}
          </Badge>
        )}
      </div>
      {isNew && PRESET_HINT[preset] && <p className="text-small-lg text-muted">{PRESET_HINT[preset]}</p>}
      <div className="grid gap-3 sm:grid-cols-2">
        <FormField
          id={ids.id}
          label="ID"
          error={idError && id ? idError : null}
          hint={isNew ? `Models are picked as ${id || "id"}/model` : "Can't be changed"}
          info={isNew ? <p>Lowercase letters, digits and dashes. Fixed once saved.</p> : undefined}
        >
          <input
            id={ids.id}
            value={id}
            onChange={(e) => setId(e.target.value.toLowerCase())}
            disabled={!isNew}
            placeholder="my-provider"
            spellCheck={false}
            aria-invalid={Boolean(idError && id)}
            className={cx(inputClass, "font-mono text-body-sm disabled:opacity-60")}
          />
        </FormField>
        <FormField id={ids.name} label="Name">
          <input id={ids.name} value={name} onChange={(e) => setName(e.target.value)} placeholder="My provider" className={inputClass} />
        </FormField>
        {template.map((variable) => (
          <FormField
            key={variable.name}
            id={`${ids.url}-${variable.name}`}
            label={variable.label}
            info={<p>Part of this provider's address, so the URL below is only complete once it is filled in.</p>}
          >
            <input
              id={`${ids.url}-${variable.name}`}
              value={vars[variable.name] ?? ""}
              onChange={(e) => setVars((v) => ({ ...v, [variable.name]: e.target.value }))}
              placeholder={variable.placeholder}
              spellCheck={false}
              className={cx(inputClass, "font-mono text-body-sm")}
            />
          </FormField>
        ))}
        <FormField
          id={ids.url}
          label="Base URL"
          className="sm:col-span-2"
          error={originKeyError ?? (urlError && baseUrl && !unfilled.length ? urlError : null)}
          hint={
            unfilled.length
              ? urlError ?? undefined
              : loopback
                ? "The Mothership connects to this address, so localhost is the Mothership itself."
                : undefined
          }
        >
          <input
            id={ids.url}
            value={template.length ? url : baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            readOnly={template.length > 0}
            placeholder={wire === "openai" ? "https://api.openai.com" : "https://api.example.com/anthropic"}
            spellCheck={false}
            aria-invalid={Boolean(originKeyError || (urlError && baseUrl && !unfilled.length))}
            className={cx(inputClass, "font-mono text-body-sm", template.length > 0 && "text-muted")}
          />
        </FormField>
        <FormField id={ids.auth} label="Authentication">
          <select id={ids.auth} value={auth} onChange={(e) => setAuth(e.target.value as ProviderAuth)} className={inputClass}>
            {(Object.keys(AUTH_LABEL) as ProviderAuth[]).map((mode) => (
              <option key={mode} value={mode}>
                {AUTH_LABEL[mode]}
              </option>
            ))}
          </select>
        </FormField>
        <FormField id={ids.key} label={auth === "bearer" ? "Token" : "API key"} error={keyError}>
          {auth === "none" ? (
            <p id={ids.key} className="flex h-9 items-center text-body-sm text-faint">
              Not needed
            </p>
          ) : keyMode === "keep" ? (
            <div id={ids.key} className="flex flex-wrap items-center gap-2">
              <span className="inline-flex h-9 items-center gap-1.5 text-body-sm text-ok">
                <IconCheck size={14} /> Saved
              </span>
              <Button size="sm" onClick={() => setKeyMode("replace")}>
                Replace
              </Button>
              <Button size="sm" variant="danger" onClick={() => setKeyMode("remove")}>
                Remove
              </Button>
            </div>
          ) : keyMode === "remove" ? (
            <div id={ids.key} className="flex flex-wrap items-center gap-2">
              <span className="inline-flex h-9 items-center text-body-sm text-warn">Removed on save</span>
              <Button size="sm" variant="ghost" onClick={() => setKeyMode("keep")}>
                Undo
              </Button>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              <input
                id={ids.key}
                type="password"
                autoComplete="off"
                value={key}
                onChange={(e) => setKey(e.target.value)}
                placeholder={initial?.has_key ? "New key" : "sk-…"}
                aria-invalid={Boolean(keyError)}
                className={cx(inputClass, "font-mono text-body-sm")}
              />
              {initial?.has_key && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    setKey("");
                    setKeyMode("keep");
                  }}
                >
                  Keep saved
                </Button>
              )}
            </div>
          )}
        </FormField>
        <FormField
          id={ids.models}
          label="Models"
          className="sm:col-span-2"
          info={<p>Model IDs as the endpoint expects them. Enter or a comma adds one; leave empty to type IDs where you pick a model.</p>}
        >
          <ChipsInput id={ids.models} values={models} onChange={setModels} placeholder={models.length ? "Add another" : "deepseek-flash, qwen3-coder, …"} />
        </FormField>
        <details
          open={advancedOpen}
          onToggle={(e) => setAdvancedOpen(e.currentTarget.open)}
          className="group min-w-0 rounded-lg border border-border sm:col-span-2"
        >
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-3 py-2 text-body-sm hover:bg-panel-2 [&::-webkit-details-marker]:hidden">
            <IconChevron size={14} className="shrink-0 text-muted transition-transform group-open:rotate-90" />
            <span className="font-medium">Advanced</span>
            <span className={cx("min-w-0 flex-1 truncate text-small", limitsInvalid ? "text-err" : "text-faint")}>
              {limitsInvalid
                ? "Some values are out of range"
                : advancedSummary.length
                  ? advancedSummary.join(" · ")
                  : "Timeouts, concurrency, context window, fallback"}
            </span>
          </summary>
          <div className="grid gap-3 border-t border-border px-3 pb-3 pt-3 sm:grid-cols-2">
            <LimitField
              label="Request timeout (s)"
              value={limitDraft.timeout_secs}
              onChange={(v) => setLimit("timeout_secs", v)}
              placeholder={String(DEFAULT_TIMEOUT)}
              error={limits.timeout_secs.error}
              help={`How long the Mothership waits for a response. Blank uses ${DEFAULT_TIMEOUT}.`}
            />
            <LimitField
              label="Queue timeout (s)"
              value={limitDraft.queue_timeout_secs}
              onChange={(v) => setLimit("queue_timeout_secs", v)}
              placeholder="Same as request timeout"
              error={limits.queue_timeout_secs.error}
              help="How long a request may wait for a free slot."
            />
            <LimitField
              label="Max concurrent requests"
              value={limitDraft.max_concurrent}
              onChange={(v) => setLimit("max_concurrent", v)}
              placeholder="Unlimited"
              error={limits.max_concurrent.error}
              help="Requests beyond this wait in a queue on the Mothership; a local server usually handles 1-2. Left empty it is unlimited: the request rate is every running colony times its subagents."
            />
            <LimitField
              label="Context window (tokens)"
              value={limitDraft.context_tokens}
              onChange={(v) => setLimit("context_tokens", v)}
              placeholder="Claude default"
              error={limits.context_tokens.error}
              help="The model's context size, so agents compact before they hit it."
            />
            <FormField
              id={ids.fallback}
              label="Fallback model"
              info={
                <p>
                  A Claude model is used when the provider is unreachable, times out, the queue is full or its plan runs out. A
                  model on another provider of the same wire is used when its plan runs out: the Mothership retries there.
                </p>
              }
            >
              <select id={ids.fallback} value={fallback} onChange={(e) => setFallback(e.target.value)} className={inputClass}>
                <option value="">None</option>
                {fallback && !anthropicModels.some((m) => m.id === fallback) && !sameWire.includes(fallback) && (
                  <option value={fallback}>{fallback}</option>
                )}
                {anthropicModels.map((model) => (
                  <option key={model.id} value={model.id}>
                    {model.label === model.id ? model.id : `${model.id} · ${model.label}`}
                  </option>
                ))}
                {sameWire.map((model) => (
                  <option key={model} value={model}>
                    {model}
                  </option>
                ))}
              </select>
            </FormField>
          </div>
        </details>
        <details className="group min-w-0 rounded-lg border border-border sm:col-span-2">
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-3 py-2 text-body-sm hover:bg-panel-2 [&::-webkit-details-marker]:hidden">
            <IconChevron size={14} className="shrink-0 text-muted transition-transform group-open:rotate-90" />
            <span className="font-medium">Connection policy</span>
            <span className="min-w-0 flex-1 truncate text-small text-faint">{policySummary}</span>
          </summary>
          <div className="space-y-3 border-t border-border px-3 pb-3 pt-3">
            <Row
              id={ids.trusted}
              label="Trusted"
              inline
              info={
                <p>
                  Marks the connection as vetted to carry restricted-sensitivity work — secrets, .env files, infra config.
                  Left off, the security-aware routing gate keeps those paths away from this provider.
                </p>
              }
            >
              <Switch id={ids.trusted} labelledBy={`${ids.trusted}-label`} label="Trusted" checked={trusted} onChange={setTrusted} />
            </Row>
            <div className="min-w-0 space-y-1.5">
              <div className="flex items-center gap-1">
                <span className="text-small-lg font-medium text-muted">Model map</span>
                <InfoButton label="Model map">
                  <p>
                    Canonical model name → the name sent on the wire. A <Code>provider/model</Code> picked anywhere goes
                    out as the wire name on the right; a canonical with no row is sent as it is. A blank wire name sends
                    the canonical name.
                  </p>
                </InfoButton>
              </div>
              {modelMapRows.length === 0 && <p className="text-small text-faint">No mappings — every model name goes out as it is.</p>}
              {modelMapRows.map((row, index) => (
                <div key={index} className="flex items-center gap-2">
                  <input
                    value={row.canonical}
                    onChange={(e) => setModelMapRow(index, { canonical: e.target.value })}
                    placeholder="canonical name"
                    spellCheck={false}
                    autoComplete="off"
                    aria-label={`Canonical model name, row ${index + 1}`}
                    className={cx(inputClass, "font-mono text-body-sm")}
                  />
                  <span aria-hidden="true" className="shrink-0 text-faint">
                    →
                  </span>
                  <input
                    value={row.wire}
                    onChange={(e) => setModelMapRow(index, { wire: e.target.value })}
                    placeholder="wire name"
                    spellCheck={false}
                    autoComplete="off"
                    aria-label={`Wire model name, row ${index + 1}`}
                    className={cx(inputClass, "font-mono text-body-sm")}
                  />
                  <button
                    type="button"
                    onClick={() => removeModelMapRow(index)}
                    aria-label={`Remove model mapping ${index + 1}`}
                    className="grid size-8 shrink-0 cursor-pointer place-items-center rounded-lg text-faint hover:bg-panel-2 hover:text-text"
                  >
                    <IconX size={13} />
                  </button>
                </div>
              ))}
              <Button size="sm" variant="ghost" onClick={addModelMapRow}>
                Add mapping
              </Button>
              {mapError && <span className="block text-small text-err">{mapError}</span>}
            </div>
            <FormField
              id={ids.disabledTools}
              label="Disabled tools"
              info={<p>Claude Code tool names stripped from every request through this connection, so an agent cannot call them here.</p>}
              hint="Enter or a comma adds one."
            >
              <ChipsInput
                id={ids.disabledTools}
                values={disabledTools}
                onChange={setDisabledTools}
                placeholder={disabledTools.length ? "Add another" : "WebSearch, Bash, …"}
              />
            </FormField>
          </div>
        </details>
        <details className="group min-w-0 rounded-lg border border-border sm:col-span-2">
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-3 py-2 text-body-sm hover:bg-panel-2 [&::-webkit-details-marker]:hidden">
            <IconChevron size={14} className="shrink-0 text-muted transition-transform group-open:rotate-90" />
            <span className="font-medium">Pricing</span>
            <span className={cx("min-w-0 flex-1 truncate text-small", pricingInvalid ? "text-err" : "text-faint")}>
              {pricingInvalid
                ? "Some values are out of range"
                : pricingSummary.length
                  ? `${pricingSummary.join(" · ")} per million tokens`
                  : "Optional — unset counts as $0 spent"}
            </span>
          </summary>
          <div className="grid gap-3 border-t border-border px-3 pb-3 pt-3 sm:grid-cols-2">
            <PriceField
              label="Input ($ per million tokens)"
              value={pricingDraft.input_per_mtok}
              onChange={(v) => setPricing("input_per_mtok", v)}
              error={pricing.input_per_mtok.error}
              help="What a million fresh input tokens cost."
            />
            <PriceField
              label="Output ($ per million tokens)"
              value={pricingDraft.output_per_mtok}
              onChange={(v) => setPricing("output_per_mtok", v)}
              error={pricing.output_per_mtok.error}
              help="What a million output tokens cost."
            />
            <PriceField
              label="Cache read ($ per million tokens)"
              value={pricingDraft.cache_read_per_mtok}
              onChange={(v) => setPricing("cache_read_per_mtok", v)}
              error={pricing.cache_read_per_mtok.error}
              help="What a million tokens read back from the provider's prompt cache cost."
            />
            <PriceField
              label="Cache write ($ per million tokens)"
              value={pricingDraft.cache_write_per_mtok}
              onChange={(v) => setPricing("cache_write_per_mtok", v)}
              error={pricing.cache_write_per_mtok.error}
              help="What a million tokens written to the provider's prompt cache cost."
            />
            <p className="text-small leading-snug text-faint sm:col-span-2">
              Rates are dollars per million tokens, as the provider bills them, so a colony's spend budget sees this
              provider's traffic. A provider with no rates set still counts its routed tokens but adds $0 to the
              spend — the budget then only sees Claude's cost.
            </p>
          </div>
        </details>
        <details className="group min-w-0 rounded-lg border border-border sm:col-span-2">
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-3 py-2 text-body-sm hover:bg-panel-2 [&::-webkit-details-marker]:hidden">
            <IconChevron size={14} className="shrink-0 text-muted transition-transform group-open:rotate-90" />
            <span className="font-medium">Plan balance</span>
            <span className="min-w-0 flex-1 truncate text-small text-faint">
              {quotaUrl.trim() ? `Probe ${quotaUrl.trim()}` : "Optional — read what is left in a prepaid plan"}
            </span>
          </summary>
          <div className="grid gap-3 border-t border-border px-3 pb-3 pt-3 sm:grid-cols-2">
            <FormField id={ids.quotaUrl} label="Quota URL" hint="Same host as the base URL">
              <input
                id={ids.quotaUrl}
                value={quotaUrl}
                onChange={(e) => setQuotaUrl(e.target.value)}
                placeholder="https://api.example.com/plan"
                spellCheck={false}
                autoComplete="off"
                className={cx(inputClass, "font-mono text-body-sm")}
              />
            </FormField>
            <FormField id={ids.quotaPointer} label="Quota JSON pointer" hint="RFC 6901, like /data/remaining_tokens">
              <input
                id={ids.quotaPointer}
                value={quotaPointer}
                onChange={(e) => setQuotaPointer(e.target.value)}
                placeholder="/data/remaining_tokens"
                spellCheck={false}
                autoComplete="off"
                className={cx(inputClass, "font-mono text-body-sm")}
              />
            </FormField>
            <p className="text-small leading-snug text-faint sm:col-span-2">
              The provider's own credential is sent to that URL, so it must be on the same origin as the base URL —
              scheme, host and port; the Mothership refuses anything else. The pointer picks the remaining-token number
              out of the answer, shown on the health line, and must start with /.
            </p>
          </div>
        </details>
      </div>
      <div className="flex flex-wrap items-center gap-2 border-t border-border pt-3">
        {!isNew && (
          <Button size="sm" variant="danger" className="mr-auto" disabled={busy !== null} onClick={remove}>
            {busy === "delete" && <Spinner />} Remove provider
          </Button>
        )}
        <Button size="sm" variant="ghost" className={isNew ? "ml-auto" : ""} onClick={onCancel} disabled={busy !== null}>
          Cancel
        </Button>
        <Button size="sm" type="submit" variant="primary" disabled={invalid || busy !== null}>
          {busy === "save" && <Spinner />} {isNew ? "Add provider" : "Save"}
        </Button>
      </div>
    </form>
  );
}

function LimitField({
  label,
  value,
  onChange,
  placeholder,
  error,
  help,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
  error: string | null;
  help: string;
}) {
  const id = useId();
  return (
    <FormField id={id} label={label} info={<p>{help}</p>} error={error}>
      <input
        id={id}
        inputMode="numeric"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        aria-invalid={Boolean(error)}
        className={cx(inputClass, "font-mono text-body-sm", error && "border-err")}
      />
    </FormField>
  );
}

/** One pricing rate, in dollars per million tokens. Blank is allowed and prices that token kind at $0. */
function PriceField({
  label,
  value,
  onChange,
  error,
  help,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  error: string | null;
  help: string;
}) {
  const id = useId();
  return (
    <FormField id={id} label={label} info={<p>{help}</p>} error={error}>
      <input
        id={id}
        inputMode="decimal"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder="Unset"
        spellCheck={false}
        autoComplete="off"
        aria-invalid={Boolean(error)}
        className={cx(inputClass, "font-mono text-body-sm", error && "border-err")}
      />
    </FormField>
  );
}

export function ChipsInput({
  id,
  values,
  onChange,
  placeholder,
}: {
  id: string;
  values: string[];
  onChange: (values: string[]) => void;
  placeholder?: string;
}) {
  const [draft, setDraft] = useState("");
  const commit = (raw: string) => {
    const added = raw
      .split(/[,\s]+/)
      .map((v) => v.trim())
      .filter((v) => v && !values.includes(v));
    if (added.length) onChange([...values, ...new Set(added)]);
    setDraft("");
  };
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" || e.key === ",") {
      e.preventDefault();
      commit(draft);
    } else if (e.key === "Backspace" && !draft && values.length) {
      onChange(values.slice(0, -1));
    }
  };
  return (
    <div className="flex min-h-9 w-full min-w-0 flex-wrap items-center gap-1.5 rounded-lg border border-border bg-panel px-2 py-1.5 focus-within:border-accent focus-within:ring-2 focus-within:ring-[var(--accent-ring)]">
      {values.map((value) => (
        <span key={value} className="inline-flex max-w-full items-center gap-1 rounded-md bg-panel-2 py-0.5 pl-2 pr-1 font-mono text-small">
          <span className="truncate">{value}</span>
          <button
            type="button"
            onClick={() => onChange(values.filter((v) => v !== value))}
            aria-label={`Remove ${value}`}
            className="grid size-4 cursor-pointer place-items-center rounded text-faint hover:bg-panel-3 hover:text-text"
          >
            <IconX size={11} />
          </button>
        </span>
      ))}
      <input
        id={id}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
        onBlur={() => draft.trim() && commit(draft)}
        placeholder={placeholder}
        spellCheck={false}
        className="min-w-24 flex-1 bg-transparent px-1 font-mono text-body-sm text-text outline-none placeholder:text-faint"
      />
    </div>
  );
}
