import { useEffect, useState } from "react";
import { errorMessage, useApi } from "../context";
import type { PluginDir, PluginListing } from "../types";
import { Badge, Button, InfoButton, Spinner, Switch, inputClass } from "./ui";

/** The plugin directories a colony could load, listed once when a dialog opens. */
export function usePlugins(): { listing: PluginListing | null; error: string | null } {
  const api = useApi();
  const [listing, setListing] = useState<PluginListing | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    api
      .plugins()
      .then((result) => !cancelled && setListing(result))
      .catch((e) => !cancelled && setError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api]);
  return { listing, error };
}

/** The comma-separated `plugins` setting as names, in order, without blanks or repeats (plugins.rs). */
export function pluginNames(value: unknown): string[] {
  const names: string[] = [];
  for (const name of String(value ?? "").split(",").map((n) => n.trim())) {
    if (name && !names.includes(name)) names.push(name);
  }
  return names;
}

const plural = (n: number, word: string) => `${n.toLocaleString()} ${word}${n === 1 ? "" : "s"}`;

/** What switching a skillset on adds to every colony, e.g. "286 skills · 68 agents". */
export function pluginCost(plugin: PluginDir): string {
  const parts = [plural(plugin.skills, "skill")];
  if (plugin.agents) parts.push(plural(plugin.agents, "agent"));
  if (plugin.commands) parts.push(plural(plugin.commands, "command"));
  return parts.join(" · ");
}

export function PluginBadges({ plugin }: { plugin: PluginDir }) {
  return (
    <>
      <Badge tone={plugin.source === "vendored" ? "accent" : "neutral"}>
        {plugin.source === "vendored" ? "vendored" : "local"}
        {plugin.version ? ` ${plugin.version}` : ""}
      </Badge>
      {plugin.shadows_vendored && <Badge tone="warn">replaces vendored</Badge>}
    </>
  );
}

/**
 * The claude-code module's `plugins` setting as one switch per skillset. The value stays the same
 * comma-separated list of names the mothership reads, written in the listing's order so switching one off
 * and on again leaves the setting unchanged.
 */
export function SkillsetField({
  label,
  description,
  value,
  onChange,
}: {
  label: string;
  description?: string;
  value: unknown;
  onChange: (value: string) => void;
}) {
  const { listing, error } = usePlugins();
  const names = pluginNames(value);
  const known = new Set(listing?.plugins.map((p) => p.name) ?? []);
  const missing = listing ? names.filter((name) => !known.has(name)) : [];

  const write = (enabled: Set<string>) =>
    onChange(
      [...(listing?.plugins.map((p) => p.name) ?? []), ...missing].filter((name) => enabled.has(name)).join(","),
    );
  const toggle = (name: string, on: boolean) => {
    const enabled = new Set(names);
    if (on) enabled.add(name);
    else enabled.delete(name);
    write(enabled);
  };

  return (
    <div className="space-y-2.5 py-3">
      <div className="flex items-center gap-1">
        <span className="text-[13.5px] font-medium">{label}</span>
        {description && (
          <InfoButton label={label}>
            <p>{description}</p>
          </InfoButton>
        )}
      </div>

      {error ? (
        // Without the list, the setting can still be edited as the names it stores.
        <div className="space-y-1">
          <input
            value={String(value ?? "")}
            onChange={(e) => onChange(e.target.value)}
            aria-label={`${label}, comma-separated names`}
            className={inputClass}
          />
          <span className="block text-[12px] text-err">Couldn't list skillsets ({error}); edit the names directly.</span>
        </div>
      ) : !listing ? (
        <div className="flex h-9 items-center gap-2 text-[12.5px] text-muted">
          <Spinner /> Listing skillsets…
        </div>
      ) : (
        <>
          <div className="divide-y divide-border rounded-lg border border-border">
            {listing.plugins.map((plugin) => (
              <div key={plugin.name} className="flex items-start gap-3 px-3 py-2.5">
                <div className="pt-0.5">
                  <Switch
                    checked={names.includes(plugin.name)}
                    onChange={(on) => toggle(plugin.name, on)}
                    label={`Load the ${plugin.name} skillset`}
                  />
                </div>
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="text-[13.5px] font-medium">{plugin.name}</span>
                    <PluginBadges plugin={plugin} />
                  </div>
                  {plugin.description && <div className="mt-0.5 line-clamp-2 text-[12px] text-muted">{plugin.description}</div>}
                  <div className="mt-0.5 text-[12px] text-faint">{pluginCost(plugin)}</div>
                </div>
              </div>
            ))}
            {missing.map((name) => (
              <div key={name} className="flex items-center gap-3 px-3 py-2.5">
                <Badge tone="err">missing</Badge>
                <div className="min-w-0 flex-1 text-[13px]">
                  <span className="font-medium [overflow-wrap:anywhere]">{name}</span>
                  <span className="block text-[12px] text-muted">Not installed: a colony loading it fails to boot.</span>
                </div>
                <Button size="sm" variant="ghost" onClick={() => toggle(name, false)}>
                  Remove
                </Button>
              </div>
            ))}
            {listing.plugins.length === 0 && missing.length === 0 && (
              <p className="px-3 py-2.5 text-[12.5px] text-muted">No skillsets are installed.</p>
            )}
          </div>
          <p className="text-[12px] text-faint">
            Add your own by putting a Claude Code plugin directory in{" "}
            <code className="font-mono text-[11.5px] [overflow-wrap:anywhere]">{listing.local_root}</code>.
          </p>
        </>
      )}
    </div>
  );
}
