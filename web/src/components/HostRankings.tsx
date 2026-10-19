// Ranked provider hosts for one canonical model (issue #1039): wherever a model value is picked or
// the cheapest-host policy is switched on, the price order the policy routes by is shown — cheapest
// first, rates per million tokens, each host's price verification and its restrictions as badges.
import { useEffect, useId, useState } from "react";
import { useApi } from "../context";
import type { ModelHost, ModelHostRanking } from "../features/models/types";
import { verifiedTextOf } from "./settings/providerCatalog";
import { Badge, type Tone } from "./ui";

/** `in $X · out $Y` from a host's rates; a null rate is skipped, and a host with neither is "price unknown". */
function priceText(host: ModelHost): string {
  const parts: string[] = [];
  if (host.input_per_mtok != null) parts.push(`in $${host.input_per_mtok}`);
  if (host.output_per_mtok != null) parts.push(`out $${host.output_per_mtok}`);
  return parts.length > 0 ? parts.join(" · ") : "price unknown";
}

/** The warning badges a host carries, in reading order. */
function hostBadges(host: ModelHost): { label: string; tone: Tone }[] {
  const badges: { label: string; tone: Tone }[] = [];
  if (host.degraded) badges.push({ label: "degraded", tone: "warn" });
  if (host.out_of_quota) badges.push({ label: "out of quota", tone: "err" });
  if (host.tools_limited) badges.push({ label: "tools limited", tone: "neutral" });
  if (host.host_sensitive) badges.push({ label: "host-sensitive", tone: "warn" });
  return badges;
}

export function HostRankings({ rankings, currentModel, heading }: {
  rankings: ModelHostRanking[];
  /** Current model value, e.g. "deepseek/deepseek-v4-flash"; its canonical picks which ranking to show. */
  currentModel?: string;
  heading?: string;
}) {
  // A `section` with an accessible name is a region, so the ranking is nameable; the heading it
  // already renders is the name (an `aria-label` on a bare `div` would be ignored — role generic).
  const headingId = useId();
  // The ranking is keyed by the canonical part after the first "/"; a value without one (a Claude
  // alias or ID) or one nobody serves on a second provider has nothing to show.
  const slash = currentModel?.indexOf("/") ?? -1;
  const canonical = slash > 0 ? currentModel?.slice(slash + 1) : undefined;
  const ranking = canonical === undefined ? undefined : rankings.find((r) => r.model === canonical);
  if (!ranking) return null;
  const title = heading ?? `Hosts for ${ranking.model}`;
  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-1">
      <h4 id={headingId} className="text-small font-medium text-faint">{title}</h4>
      <ol className="flex flex-col gap-1">
        {ranking.hosts.map((host, i) => (
          <li key={host.model} className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-1 text-small">
            <span aria-hidden="true" className="w-3 shrink-0 text-right text-meta-lg text-faint">
              {i + 1}
            </span>
            <span className="truncate font-medium text-text">{host.provider_name}</span>
            <span className="text-faint">{priceText(host)}</span>
            <span className="text-faint">· {verifiedTextOf(host.last_verified_at)}</span>
            {host.model === currentModel && <Badge tone="accent">in use</Badge>}
            {hostBadges(host).map((badge) => (
              <Badge key={badge.label} tone={badge.tone}>
                {badge.label}
              </Badge>
            ))}
          </li>
        ))}
      </ol>
    </section>
  );
}

/**
 * Fetches the rankings (GET /api/model-hosts) and shows the one for `currentModel`. Nothing while
 * they load, nothing on an older mothership (the route answers 404, read as no data) and nothing
 * for a model no ranking covers — the picker and the settings pane both stay as they were.
 */
export function HostRankingsPanel({ currentModel, heading }: { currentModel?: string; heading?: string }) {
  const api = useApi();
  const [rankings, setRankings] = useState<ModelHostRanking[] | null>(null);
  useEffect(() => {
    let cancelled = false;
    api
      .modelHosts()
      .then((list) => !cancelled && setRankings(list))
      .catch(() => !cancelled && setRankings([]));
    return () => {
      cancelled = true;
    };
  }, [api]);
  if (rankings === null) return null;
  return <HostRankings rankings={rankings} currentModel={currentModel} heading={heading} />;
}
