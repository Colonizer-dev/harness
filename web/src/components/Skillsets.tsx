import { useCallback, useEffect, useState } from "react";
import { errorMessage, useApi } from "../context";
import type { DownloadableSkillset, PluginDir, PluginListing } from "../types";
import { Badge, Button, InfoButton, Spinner, Switch, inputClass } from "./ui";

/** The plugin directories a colony could load, listed when a dialog opens and again after a download lands. */
export function usePlugins(): { listing: PluginListing | null; error: string | null; reload: () => void } {
  const api = useApi();
  const [listing, setListing] = useState<PluginListing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [round, setRound] = useState(0);
  useEffect(() => {
    let cancelled = false;
    api
      .plugins()
      .then((result) => !cancelled && setListing(result))
      .catch((e) => !cancelled && setError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api, round]);
  const reload = useCallback(() => setRound((n) => n + 1), []);
  return { listing, error, reload };
}

/** What a downloadable skillset offers, for its row before it is downloaded (the listing has no manifest yet). */
const DOWNLOADABLE_ABOUT: Record<string, string> = {
  graft:
    "A code map of the colony's repository (graft by Nanonets): ranked answers with exact file:line, every caller of a symbol, a file's API. Each colony builds its own map on first use, offline.",
  "understand-anything":
    "A knowledge graph of the colony's repository (understand-anything, MIT, by Egonex): what a change would affect, a domain and business-flow view, guided onboarding tours. The first full analysis is a multi-agent pass, so colonies use the file-scoped commands only.",
};

const megabytes = (bytes: number) => `${(bytes / 1_048_576).toFixed(bytes >= 10 * 1_048_576 ? 0 : 1)} MB`;

/** How big a skillset's download is, in MB, for the row before it is asked for. */
const DOWNLOADABLE_MB: Record<string, number> = { graft: 80, "understand-anything": 3 };

/** One line about where a download is; null when the row's button says it all. */
export function downloadLine(d: DownloadableSkillset): string | null {
  const size = DOWNLOADABLE_MB[d.name];
  switch (d.state) {
    case "downloading":
      return d.total ? `Downloading… ${megabytes(d.bytes)} of ${megabytes(d.total)}` : `Downloading… ${megabytes(d.bytes)}`;
    case "unpacking":
      return "Unpacking…";
    case "failed":
      return `Download failed: ${d.error ?? "unknown error"}`;
    case "unavailable":
      return "Not published for this machine yet — a later Colonizer release will offer it.";
    case "local":
      return `Your own plugins/${d.name} directory is used instead; remove it to download the pinned bundle.`;
    case "idle":
      return d.installed_release && d.release
        ? `Version ${d.installed_release} is on disk; ${d.release} is a new download.`
        : `Downloaded when you ask${size ? `, about ${size} MB` : ""}${d.release ? ` (${d.release})` : ""}.`;
    default:
      return null;
  }
}

/**
 * A skillset that is not shipped with the app: its name, what it is, and a Download button that turns into
 * progress. Once it is on disk it leaves this row and appears in the list above as an ordinary skillset.
 */
export function DownloadableRow({
  item,
  onDownload,
  starting = false,
}: {
  item: DownloadableSkillset;
  onDownload: () => void;
  starting?: boolean;
}) {
  const busy = starting || item.state === "downloading" || item.state === "unpacking";
  const share = item.state === "downloading" && item.total ? Math.min(100, (item.bytes / item.total) * 100) : null;
  const line = downloadLine(item);
  const canDownload = item.state === "idle" || item.state === "failed";
  return (
    <div className="flex items-start gap-3 px-3 py-2.5" data-download-state={item.state}>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-body font-medium">{item.name}</span>
          <Badge tone="neutral">downloadable</Badge>
        </div>
        {DOWNLOADABLE_ABOUT[item.name] && <div className="mt-0.5 line-clamp-2 text-small text-muted">{DOWNLOADABLE_ABOUT[item.name]}</div>}
        {line && <div className={`mt-0.5 text-small ${item.state === "failed" ? "text-err" : "text-faint"}`}>{line}</div>}
        {share !== null && (
          <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-panel-3" role="progressbar" aria-valuenow={Math.round(share)} aria-valuemin={0} aria-valuemax={100}>
            <div className="h-full rounded-full bg-accent transition-[width] duration-500" style={{ width: `${share}%` }} />
          </div>
        )}
      </div>
      {(canDownload || busy) && (
        <Button size="sm" variant={item.state === "failed" ? "secondary" : "primary"} onClick={onDownload} disabled={busy} aria-label={`Download the ${item.name} skillset`}>
          {busy ? <Spinner /> : null}
          {busy ? "Downloading" : item.state === "failed" ? "Retry" : "Download"}
        </Button>
      )}
    </div>
  );
}

/**
 * Starts the download of each downloadable skillset and follows every one of them until it settles,
 * then reloads the skillset list. One map by name, so a listing with several downloads on it gets
 * one row each rather than only the first.
 */
export function useSkillsetDownloads(initial: DownloadableSkillset[] | undefined, reload: () => void) {
  const api = useApi();
  const [starting, setStarting] = useState<string[]>([]);
  // The listing is the source of truth between polls; a download's own answers win until the listing
  // comes back with the skillset on disk, at which point this map is dropped with the effect below.
  const [answers, setAnswers] = useState<Record<string, DownloadableSkillset>>({});
  useEffect(() => setAnswers({}), [initial]);
  const running = Object.values(answers).filter((d) => d.state === "downloading" || d.state === "unpacking").map((d) => d.name);
  const polling = running.join(",");
  useEffect(() => {
    if (!polling) return;
    let cancelled = false;
    const timer = window.setInterval(() => {
      for (const name of running) {
        api
          .skillset(name)
          .then((next) => {
            if (cancelled) return;
            setAnswers((s) => ({ ...s, [name]: next }));
            if (next.state === "installed") reload();
          })
          .catch(() => {
            /* the next tick tries again */
          });
      }
    }, 1000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [api, polling, reload]);
  const start = (name: string) => {
    setStarting((s) => [...s, name]);
    api
      .skillsetDownload(name)
      .then((next) => setAnswers((s) => ({ ...s, [name]: next })))
      .catch((e) =>
        setAnswers((s) => {
          const current = s[name] ?? initial?.find((d) => d.name === name);
          return current ? { ...s, [name]: { ...current, state: "failed", error: errorMessage(e) } } : s;
        }),
      )
      .finally(() => setStarting((s) => s.filter((n) => n !== name)));
  };
  return {
    rows: (initial ?? []).map((d) => answers[d.name] ?? d),
    isStarting: (name: string) => starting.includes(name),
    start,
  };
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

export function PluginBadges({ plugin, downloaded = false }: { plugin: PluginDir; downloaded?: boolean }) {
  return (
    <>
      <Badge tone={plugin.source === "vendored" ? "accent" : "neutral"}>
        {plugin.source === "vendored" ? "vendored" : downloaded ? "downloaded" : "local"}
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
  const { listing, error, reload } = usePlugins();
  const downloads = useSkillsetDownloads(listing?.downloadable, reload);
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
        <span className="text-body font-medium">{label}</span>
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
          <span className="block text-small text-err">Couldn't list skillsets ({error}); edit the names directly.</span>
        </div>
      ) : !listing ? (
        <div className="flex h-9 items-center gap-2 text-small-lg text-muted">
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
                    <span className="text-body font-medium">{plugin.name}</span>
                    <PluginBadges
                      plugin={plugin}
                      downloaded={listing.downloadable?.some((d) => d.name === plugin.name && d.state === "installed") ?? false}
                    />
                  </div>
                  {plugin.description && <div className="mt-0.5 line-clamp-2 text-small text-muted">{plugin.description}</div>}
                  <div className="mt-0.5 text-small text-faint">{pluginCost(plugin)}</div>
                </div>
              </div>
            ))}
            {missing.map((name) => (
              <div key={name} className="flex items-center gap-3 px-3 py-2.5">
                <Badge tone="err">missing</Badge>
                <div className="min-w-0 flex-1 text-body-sm">
                  <span className="font-medium [overflow-wrap:anywhere]">{name}</span>
                  <span className="block text-small text-muted">Not installed: a colony loading it fails to boot.</span>
                </div>
                <Button size="sm" variant="ghost" onClick={() => toggle(name, false)}>
                  Remove
                </Button>
              </div>
            ))}
            {listing.plugins.length === 0 && missing.length === 0 && (
              <p className="px-3 py-2.5 text-small-lg text-muted">No skillsets are installed.</p>
            )}
            {/* Offered for download until it is on disk; then it is a row above like any other. */}
            {downloads.rows
              .filter((d) => d.state !== "installed" && !(d.state === "local" && known.has(d.name)))
              .map((d) => (
                <DownloadableRow key={d.name} item={d} onDownload={() => downloads.start(d.name)} starting={downloads.isStarting(d.name)} />
              ))}
          </div>
          <p className="text-small text-faint">
            Add your own by putting a Claude Code plugin directory in{" "}
            <code className="font-mono text-meta-lg [overflow-wrap:anywhere]">{listing.local_root}</code>.
          </p>
        </>
      )}
    </div>
  );
}
