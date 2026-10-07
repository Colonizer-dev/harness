// The merge steward's Pull requests list for one org (issue #1172): each pull request a colony
// opened, what the steward is doing about it, and a manual Merge now. Lives in the org's Workspaces
// settings page, under the auto-merge setting that drives it.
import { useCallback, useEffect, useState } from "react";
import { errorMessage, useApi, useToast } from "../context";
import type { StewardOrg, StewardPhase, StewardPr } from "../types";
import { Badge, Button, Spinner, timeAgo, type Tone } from "./ui";

/** The phase's word and tone, as the list shows them. */
export const PHASES: Record<StewardPhase, { label: string; tone: Tone }> = {
  waiting: { label: "waiting", tone: "neutral" },
  merging: { label: "merging", tone: "ok" },
  rebasing: { label: "rebasing", tone: "info" },
  fixing: { label: "fixing", tone: "accent" },
  ci_blocked: { label: "ci blocked", tone: "warn" },
  needs_attention: { label: "needs attention", tone: "err" },
};

/** The `owner/name#7` a pull request URL names, for a compact link label. */
export function prLabel(url: string): string {
  const m = /github\.com\/([^/]+\/[^/]+)\/pull\/(\d+)/.exec(url);
  return m ? `${m[1]}#${m[2]}` : url;
}

/** Whether Merge now is offered: not while the steward already is merging it, or a colony is mid-run. */
export function canMergeNow(pr: StewardPr): boolean {
  return pr.state !== "merging" && pr.colony_status === "pr_opened";
}

export function PullRequestRows({
  prs,
  busy,
  onMerge,
  now,
}: {
  prs: StewardPr[];
  /** The URL being merged right now, if any. */
  busy: string | null;
  onMerge: (pr: StewardPr) => void;
  now?: Date;
}) {
  if (prs.length === 0) return <div className="py-3 text-small text-muted">No open pull requests from this org's colonies.</div>;
  return (
    <ul className="divide-y divide-border">
      {prs.map((pr) => (
        <li key={pr.url} className="flex flex-wrap items-center gap-x-3 gap-y-1.5 py-2.5">
          <div className="min-w-0 flex-1 basis-56">
            <div className="flex flex-wrap items-center gap-2">
              <Badge tone={PHASES[pr.state].tone}>{PHASES[pr.state].label}</Badge>
              <a href={pr.url} target="_blank" rel="noreferrer" className="text-body-sm font-medium hover:underline [overflow-wrap:anywhere]">
                {prLabel(pr.url)}
              </a>
            </div>
            <div className="text-small text-muted [overflow-wrap:anywhere]">
              {pr.title}
              {pr.reason ? ` · ${pr.reason}` : ""}
              {pr.since ? ` · ${timeAgo(pr.since, now)}` : ""}
            </div>
          </div>
          <Button size="sm" disabled={!canMergeNow(pr) || busy !== null} onClick={() => onMerge(pr)} aria-label={`Merge ${prLabel(pr.url)} now`}>
            {busy === pr.url && <Spinner />} Merge now
          </Button>
        </li>
      ))}
    </ul>
  );
}

/** The org's list, fetched when the settings page opens and refreshed after a merge. */
export function PullRequestsSection({ org }: { org: string }) {
  const api = useApi();
  const toast = useToast();
  const [info, setInfo] = useState<StewardOrg | null | undefined>(undefined);
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .mergeSteward()
      .then((all) => setInfo(all.orgs.find((o) => o.org.toLowerCase() === org.toLowerCase()) ?? null))
      .catch(() => setInfo(null));
  }, [api, org]);
  useEffect(load, [load]);

  const merge = async (pr: StewardPr) => {
    setBusy(pr.url);
    try {
      const done = await api.mergeNow(pr.url);
      toast(done.message || `Merged ${prLabel(pr.url)}`);
      load();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="border-b border-border py-3 last:border-b-0">
      <h3 className="text-meta-lg font-semibold uppercase tracking-wide text-faint">Pull requests</h3>
      {info === undefined ? (
        <div className="py-3 text-small text-muted">Loading…</div>
      ) : (
        <PullRequestRows prs={info?.prs ?? []} busy={busy} onMerge={(pr) => void merge(pr)} />
      )}
    </section>
  );
}
