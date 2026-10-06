// The merge train (issue #671): a read-only per-repository status block at the bottom of the
// publish module's settings. The mothership drives the train; the cockpit only watches it, and
// says nothing while the setting is off everywhere (or the mothership predates the route).
import { useEffect, useState } from "react";
import type { MergeTrainPr, MergeTrainRepo, MergeTrainStatus } from "../types";
import { useApi } from "../context";
import { Badge, timeAgo, type Tone } from "./ui";

/** The repositories worth a row: the train on, or pull requests still queued under it. */
export function visibleRepos(repos: MergeTrainRepo[]): MergeTrainRepo[] {
  return repos.filter((r) => r.state === "on" || r.prs.length > 0);
}

const STATE_TONE: Record<MergeTrainRepo["state"], Tone> = { on: "ok", off: "neutral", denied: "err" };
const CI_TONE: Record<MergeTrainRepo["base_ci"], Tone> = { green: "ok", pending: "warn", failing: "err", unknown: "neutral" };

/** One repository's train: name, state and base CI up top, then the queue as counts plus the skipped whys. */
function RepoTrain({ repo, now }: { repo: MergeTrainRepo; now?: Date }) {
  const withStatus = (status: MergeTrainPr["status"]) => repo.prs.filter((p) => p.status === status);
  const next = withStatus("next")[0];
  const waitingCi = withStatus("waiting_ci").length + withStatus("waiting").length;
  const rebase = withStatus("needs_rebase");
  const skipped = withStatus("skipped");
  return (
    <div className="py-2.5">
      <p className="flex flex-wrap items-center gap-2 text-body-sm">
        <span className="font-medium">{repo.repo}</span>
        <Badge tone={STATE_TONE[repo.state]}>{repo.state === "on" ? "train on" : repo.state}</Badge>
        <Badge tone={CI_TONE[repo.base_ci]} title={repo.base ? `CI on ${repo.base}` : undefined}>
          {repo.base ? `${repo.base} · base CI ${repo.base_ci}` : `base CI ${repo.base_ci}`}
        </Badge>
        {repo.last_merge && (
          <a href={repo.last_merge.pr_url} className="ml-auto text-small text-faint hover:underline" title={`Last merge: ${repo.last_merge.pr_url}`}>
            last merged {timeAgo(repo.last_merge.at, now)}
          </a>
        )}
      </p>
      <p className="mt-1 text-small-lg text-muted">
        {next ? (
          <>
            Next up:{" "}
            <a href={next.pr_url} className="font-medium text-accent hover:underline">
              {next.title}
            </a>
          </>
        ) : (
          "Nothing next up"
        )}
        {waitingCi > 0 && ` · ${waitingCi} waiting on CI`}
        {rebase.length > 0 && ` · ${rebase.length} needs rebase`}
      </p>
      {skipped.length > 0 && (
        <p className="mt-0.5 text-small-lg text-warn">
          Skipped: {skipped.map((p) => `${p.title} (${p.reason})`).join(", ")}
        </p>
      )}
    </div>
  );
}

export function MergeTrain({ repos, now }: { repos: MergeTrainRepo[]; now?: Date }) {
  const visible = visibleRepos(repos);
  if (visible.length === 0) return null;
  return (
    <div className="mt-3 rounded-xl border border-border bg-panel-2/40 px-4 pb-1">
      <p className="pt-2.5 text-small-lg font-medium text-muted">Merge train</p>
      <div className="divide-y divide-border">
        {visible.map((repo) => (
          <RepoTrain key={repo.repo} repo={repo} now={now} />
        ))}
      </div>
    </div>
  );
}

/**
 * Fetches /api/merge-train for the publish pane. Silent while loading, on any failure, and when
 * the mothership answers nothing worth showing — an older mothership's 404 reads the same as the
 * train being off.
 */
export function MergeTrainSection() {
  const api = useApi();
  const [status, setStatus] = useState<MergeTrainStatus | null>(null);
  useEffect(() => {
    let alive = true;
    api.mergeTrain().then((s) => alive && setStatus(s), () => alive && setStatus({ repos: [] }));
    return () => {
      alive = false;
    };
  }, [api]);
  if (!status) return null;
  return <MergeTrain repos={status.repos} />;
}
