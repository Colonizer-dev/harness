// The provider form's per-model price editor and the price feed's read-only price list (issue #1038),
// split out of ProviderForm.tsx to keep both reviewable.
import { Badge, Button, InfoButton, cx, inputClass } from "../ui";
import { IconX } from "../icons";
import type { FeedPrice, ProviderPricing } from "../../types";
import {
  PRICING_KEYS,
  pricingDraftOf,
  pricingRatesOf,
  pricingSummaryOf,
  sourceLinkOf,
  verifiedTextOf,
  type ModelPricingRow,
} from "./providerCatalog";
import { Code } from "./ui";

/** The per-model rate fields, labelled for their inputs — the connection pricing's five, compact. */
const MODEL_RATE_LABEL: Record<keyof ProviderPricing, string> = {
  input_per_mtok: "Input $/Mtok",
  output_per_mtok: "Output $/Mtok",
  cache_read_per_mtok: "Cache read $/Mtok",
  cache_write_per_mtok: "Cache write $/Mtok",
  thinking_per_mtok: "Thinking $/Mtok",
};

/**
 * Per-model prices (issue #1038): one row per model, the id as written after <provider>/ and the same
 * rate fields the connection's pricing uses. Blank ids and rate-less rows drop on save; duplicates are
 * refused, like the model map's.
 */
export function ModelPricingEditor({
  rows,
  onChange,
  error,
}: {
  rows: ModelPricingRow[];
  onChange: (rows: ModelPricingRow[]) => void;
  error: string | null;
}) {
  const setRow = (index: number, patch: Partial<ModelPricingRow>) =>
    onChange(rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
  return (
    <div className="min-w-0 space-y-1.5 sm:col-span-2">
      <div className="flex items-center gap-1">
        <span className="text-small-lg font-medium text-muted">Per-model prices</span>
        <InfoButton label="Per-model prices">
          <p>
            Rates for one model, overriding the connection's rates above for that model alone. The id is the model as
            written after the slash in <Code>provider/model</Code>, and a rate left blank bills at $0.
          </p>
        </InfoButton>
      </div>
      {rows.length === 0 && <p className="text-small text-faint">No per-model prices — every model is billed at the connection's rates.</p>}
      {rows.map((row, index) => (
        <div key={index} className="flex min-w-0 flex-wrap items-center gap-2">
          <input
            value={row.model}
            onChange={(e) => setRow(index, { model: e.target.value })}
            placeholder="model id"
            spellCheck={false}
            autoComplete="off"
            aria-label={`Model id, row ${index + 1}`}
            className={cx(inputClass, "w-44 shrink-0 font-mono text-body-sm")}
          />
          {PRICING_KEYS.map((key) => (
            <input
              key={key}
              value={row.pricing[key]}
              onChange={(e) => setRow(index, { pricing: { ...row.pricing, [key]: e.target.value } })}
              placeholder={MODEL_RATE_LABEL[key].replace(" $/Mtok", "")}
              inputMode="decimal"
              spellCheck={false}
              autoComplete="off"
              aria-label={`${MODEL_RATE_LABEL[key]}, row ${index + 1}`}
              className={cx(inputClass, "w-28 font-mono text-body-sm")}
            />
          ))}
          <button
            type="button"
            onClick={() => onChange(rows.filter((_, i) => i !== index))}
            aria-label={`Remove model price ${index + 1}`}
            className="grid size-8 shrink-0 cursor-pointer place-items-center rounded-lg text-faint hover:bg-panel-2 hover:text-text"
          >
            <IconX size={13} />
          </button>
        </div>
      ))}
      <Button size="sm" variant="ghost" onClick={() => onChange([...rows, { model: "", pricing: pricingDraftOf(null) }])}>
        Add model price
      </Button>
      {error && <span className="block text-small text-err">{error}</span>}
    </div>
  );
}

/** The host of a feed price's source URL, for the link's text; an unparsable one shows as it is. */
function sourceHost(url: string): string {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}

/**
 * What the price feed lists for this provider (issue #1038): read-only, each row saying when the feed
 * last verified the price and linking its source. Your own rates win on the server, so a row your
 * per-model or connection price covers is marked overridden, and an unverified-in-14-days one stale.
 */
export function FeedPrices({
  prices,
  modelPricing,
  connectionPriced,
}: {
  prices: FeedPrice[];
  modelPricing?: Record<string, ProviderPricing>;
  connectionPriced: boolean;
}) {
  return (
    <div className="min-w-0 space-y-2 rounded-lg border border-border px-2.5 py-2 sm:col-span-2">
      <p className="text-small text-faint">
        Prices the price feed lists for this provider — your own rates win, so a row marked overridden is not what the
        gateway charges.
      </p>
      {prices.map((price) => {
        const href = sourceLinkOf(price.source);
        const summary = pricingSummaryOf(pricingRatesOf(price.pricing)).join(" · ");
        return (
          <div key={price.model} className="min-w-0">
            <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5">
              <span className="font-mono text-body-sm">{price.model}</span>
              {summary && <span className="text-body-sm text-muted">{summary} per million tokens</span>}
              {(modelPricing?.[price.model] || connectionPriced) && <Badge tone="info">overridden by your price</Badge>}
              {price.stale && <Badge tone="warn">stale — verified over 14 days ago</Badge>}
            </div>
            <p className="text-small text-faint">
              from feed, {verifiedTextOf(price.last_verified_at)}
              {href && (
                <>
                  {" · "}
                  <a href={href} target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">
                    {sourceHost(href)}
                  </a>
                </>
              )}
            </p>
          </div>
        );
      })}
    </div>
  );
}
