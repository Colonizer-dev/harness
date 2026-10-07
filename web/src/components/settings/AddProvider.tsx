// "Add a provider" (issue #1204): a short grid of logo tiles for the endpoints most people use, then a
// collapsed search over the full catalogue. Picking either goes straight to a short form (key, with the
// models prefilled from the catalogue). The legal line is one muted sentence with the full text behind
// "Why?".
import { useId, useState } from "react";

import { PROVIDER_CATALOG, type CatalogEntry } from "../../providerCatalog";
import { IconChevron, IconPlus } from "../icons";
import { ProviderMark } from "../providerMark";
import { Badge, cx, inputClass } from "../ui";
import { ADD_PRESETS, WIRE_LABEL, presetLabel } from "./providerCatalog";

/** The host part of a base URL, which is what tells two endpoints apart in a list. */
function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/** The quick choices' ids, so the search does not list them a second time. */
const QUICK = new Set<string>(ADD_PRESETS.map((p) => p.preset));

export function AddProvider({ disabled, onPick }: { disabled: boolean; onPick: (preset: string) => void }) {
  const headId = useId();
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState("");
  const [why, setWhy] = useState(false);
  const more = PROVIDER_CATALOG.filter((entry) => !QUICK.has(entry.id));
  const needle = query.trim().toLowerCase();
  const matches = more.filter((entry: CatalogEntry) => !needle || entry.name.toLowerCase().includes(needle) || entry.base_url.toLowerCase().includes(needle));

  return (
    <section aria-labelledby={headId} className="mt-7">
      <h4 id={headId} className="text-body-lg font-medium">
        Add a provider
      </h4>
      <p className="mb-3 mt-0.5 text-small-lg text-muted">Pick one, paste its key, and its models are filled in.</p>
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
        {ADD_PRESETS.map(({ preset, label }) => (
          <button
            key={preset}
            type="button"
            disabled={disabled}
            onClick={() => onPick(preset)}
            className={cx(
              "flex min-w-0 cursor-pointer select-none items-center gap-2.5 rounded-xl border border-border bg-panel px-2.5 py-2.5 text-left",
              "text-body-sm font-medium text-text transition-colors hover:bg-panel-2",
              "disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:bg-panel",
            )}
          >
            <ProviderMark preset={preset} name={presetLabel(preset)} />
            <span className="min-w-0 truncate">{label}</span>
          </button>
        ))}
      </div>
      <button
        type="button"
        disabled={disabled}
        aria-expanded={searching}
        onClick={() => setSearching((open) => !open)}
        className="mt-2 flex w-full cursor-pointer items-center gap-2 rounded-xl border border-dashed border-border-strong px-3 py-2.5 text-left text-body-sm text-muted transition-colors hover:bg-panel-2 hover:text-text disabled:cursor-not-allowed disabled:opacity-45"
      >
        <IconPlus size={15} />
        <span className="flex-1">Search {more.length} more…</span>
        <IconChevron size={14} className={cx("transition-transform", searching && "rotate-90")} />
      </button>
      {searching && (
        <div className="mt-2 space-y-2 rounded-xl border border-border bg-panel p-2.5">
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search by name or address"
            aria-label="Search providers"
            autoFocus
            className={inputClass}
          />
          <ul className="scroll-thin max-h-64 space-y-0.5 overflow-y-auto">
            {matches.map((entry) => (
              <li key={entry.id}>
                <button
                  type="button"
                  disabled={disabled}
                  onClick={() => {
                    setSearching(false);
                    setQuery("");
                    onPick(entry.id);
                  }}
                  className="flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-2 py-1.5 text-left hover:bg-panel-2 disabled:cursor-not-allowed disabled:opacity-45"
                >
                  <ProviderMark preset={entry.id} name={entry.name} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-body-sm font-medium">{entry.name}</span>
                    <span className="block truncate font-mono text-meta-lg text-faint">{hostOf(entry.base_url)}</span>
                  </span>
                  {entry.wire === "openai" && <Badge tone="info">{WIRE_LABEL.openai}</Badge>}
                </button>
              </li>
            ))}
            {matches.length === 0 && <li className="px-2 py-3 text-body-sm text-faint">Nothing matches that.</li>}
          </ul>
        </div>
      )}
      <p className="mt-4 text-meta-lg leading-snug text-faint">
        The catalogue isn't vetted or endorsed.{" "}
        <button type="button" aria-expanded={why} onClick={() => setWhy((v) => !v)} className="cursor-pointer underline decoration-dotted underline-offset-2 hover:text-muted">
          Why?
        </button>
      </p>
      {why && (
        <p className="mt-1.5 rounded-lg bg-panel-2 px-3 py-2 text-meta-lg leading-snug text-muted">
          {PROVIDER_CATALOG.length} endpoints come from the cc-switch catalogue. Colonizer neither vets nor endorses them, and many resell access
          rather than run the model themselves. Logos and names are the property of their owners; Colonizer is not affiliated with, endorsed by or
          connected to any of them.
        </p>
      )}
    </section>
  );
}
