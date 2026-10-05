// The cockpit-global GitHub pause banner (issue #1074): while GitHub refuses the mothership's
// account — suspended, a revoked token, or secondary rate limits that keep coming — GET /api/status
// `github_pause` says so, and every cockpit view banners it above its own content: the cause, what to
// do next, and what is held. A revoked token offers the Connections settings page, where GitHub is
// reconnected. It clears itself when the mothership's probe finds GitHub working again, so there is
// nothing to dismiss. Absent on an older mothership. Rendered to static markup in the tests.
import type { ReactElement } from "react";

import type { GitHubPause } from "../features/providers/types";

/** The banner's words: the cause and the next step, then what waits, then when GitHub is asked again. */
export function gitHubPauseText(pause: GitHubPause, now: Date = new Date()): string {
  const parts = [`${pause.message ?? "GitHub paused"}: ${pause.next_step ?? "waiting for GitHub"}.`];
  const held: string[] = [];
  const queued = pause.queued ?? 0;
  const publishes = pause.held_publishes ?? 0;
  if (queued > 0) held.push(`${queued} queued ${queued === 1 ? "colony waits" : "colonies wait"}`);
  if (publishes > 0) held.push(`${publishes} ${publishes === 1 ? "publish is" : "publishes are"} held`);
  parts.push(
    `Launches, publishes, merges and GitHub writes are paused${held.length ? ` (${held.join(", ")})` : ""}; running colonies keep working.`,
  );
  if (pause.next_probe_at) {
    const minutes = Math.max(0, Math.ceil((Date.parse(pause.next_probe_at) - now.getTime()) / 60_000));
    if (Number.isFinite(minutes)) parts.push(minutes <= 1 ? "Checking GitHub again within a minute." : `Checking GitHub again in ${minutes} minutes.`);
  }
  return parts.join(" ");
}

/** The GitHub pause banner: one line while paused, nothing otherwise. */
export function GitHubBanner({
  pause,
  onReconnect,
  now,
}: {
  /** `/api/status` `github_pause`; missing or not paused renders nothing. */
  pause: GitHubPause | null | undefined;
  /** Opens the Connections settings section, where GitHub is reconnected. */
  onReconnect: () => void;
  now?: Date;
}): ReactElement | null {
  if (!pause?.paused) return null;
  return (
    <div className="px-6 pt-4">
      <div
        role="alert"
        className="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-md border border-warn bg-warn-soft px-3 py-2 text-sm text-warn"
      >
        <span>{gitHubPauseText(pause, now)}</span>
        {pause.cause === "token_revoked" ? (
          <button
            type="button"
            onClick={onReconnect}
            className="cursor-pointer rounded-md border border-warn px-2 py-0.5 text-[12.5px] font-semibold hover:underline"
          >
            Reconnect GitHub
          </button>
        ) : null}
        {pause.cause === "suspended" ? (
          <a
            href="https://support.github.com"
            target="_blank"
            rel="noreferrer"
            className="rounded-md border border-warn px-2 py-0.5 text-[12.5px] font-semibold hover:underline"
          >
            GitHub support
          </a>
        ) : null}
      </div>
    </div>
  );
}
