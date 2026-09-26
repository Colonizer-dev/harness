// The map view's "keep this map up to date?" question: whether an enabled map loop already refreshes
// this repository, and otherwise a once-per-repository offer to create one (issue #564). The answer
// lives in localStorage, so "Not now" stays quiet for a month and a deleted loop can be re-made from
// the map bar's "Keep fresh…" link. The decision itself is pure, so it is testable apart from the map.
import { useState, type ReactElement } from "react";
import { Button, cx } from "../components/ui";
import type { Loop, NewLoop } from "../types";
import { DAY_PRESETS, MAX_DAYS, describeLoopCadence, mapLoopBody } from "./loops";

/** Where the answer lives, one key per repository: `{"answer":"created"}` or `{"answer":"not_now","at":<iso>}`. */
export const mapRefreshKey = (repo: string): string => `colonizer.mapRefresh.${repo.toLowerCase()}`;

/** How long "Not now" hides the question before it may ask again. */
export const NOT_NOW_MS = 30 * 86_400_000;

export type MapRefreshAnswer = { answer: "created" } | { answer: "not_now"; at: string };

/** The stored answer, or null when there is none — missing, or garbled, both read as never asked. */
export function parseMapRefreshAnswer(raw: string | null): MapRefreshAnswer | null {
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as MapRefreshAnswer;
    if (parsed?.answer === "created") return parsed;
    if (parsed?.answer === "not_now" && typeof parsed.at === "string") return parsed;
    return null;
  } catch {
    return null;
  }
}

export type MapRefreshDecision = "ask" | "hidden" | { covered: Loop };

/**
 * What the map does about freshness: show the question, show nothing (already asked, or "Not now"
 * inside its month), or stand down because an enabled map loop covers the repository — this one, or
 * `owner/*` across its org, owners compared case-insensitively like GitHub's.
 */
export function mapRefreshPrompt(loops: readonly Loop[], repo: string, remembered: MapRefreshAnswer | null, now = Date.now()): MapRefreshDecision {
  const owner = repo.split("/")[0].toLowerCase();
  const covered = loops.find((l) => {
    if (!l.enabled || (l.kind ?? "colony") !== "map") return false;
    const scope = l.repo.toLowerCase();
    return scope === repo.toLowerCase() || (scope.endsWith("/*") && scope.slice(0, -2) === owner);
  });
  if (covered) return { covered };
  if (remembered?.answer === "created") return "hidden";
  if (remembered?.answer === "not_now" && now - new Date(remembered.at).getTime() < NOT_NOW_MS) return "hidden";
  return "ask";
}

/** The line that replaces the question once a map loop covers this repository. */
export function MapRefreshCovered({ covered, onEdit }: { covered: Loop; onEdit: () => void }): ReactElement {
  const words = covered.cadence.every === "every_days" ? `Refreshed every ${covered.cadence.days} days` : `Refreshed ${describeLoopCadence(covered.cadence)}`;
  return (
    <span className="text-[12.5px] text-faint">
      {words} ·{" "}
      <button type="button" onClick={onEdit} className="cursor-pointer border-0 bg-transparent p-0 text-muted underline decoration-dotted underline-offset-2 hover:text-text">
        edit
      </button>
    </span>
  );
}

/** The question: how often to re-map, and whether this repository or the whole org. */
export function MapRefreshPrompt({
  repo,
  onCreate,
  onNotNow,
}: {
  repo: string;
  onCreate: (body: NewLoop) => void;
  onNotNow: () => void;
}): ReactElement {
  const [days, setDays] = useState(14);
  const [all, setAll] = useState(false);
  const preset = DAY_PRESETS.includes(days);
  const field = "rounded-lg border border-border bg-transparent px-2 py-1 text-[12.5px] text-text outline-none focus:border-border-strong";
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-xl border border-border bg-panel px-4 py-2.5 text-[13px]">
      <span className="font-semibold text-text">Keep this map up to date?</span>
      <span className="text-muted">Re-map every</span>
      <select
        aria-label="days between re-maps"
        value={preset ? days : "custom"}
        // A non-preset day count, so picking "custom" shows the input instead of the menu.
        onChange={(e) => setDays(e.target.value === "custom" ? 45 : Number(e.target.value))}
        className={cx(field, "w-32")}
      >
        {DAY_PRESETS.map((d) => (
          <option key={d} value={d}>
            {d} days
          </option>
        ))}
        <option value="custom">custom (days)</option>
      </select>
      {!preset && (
        <input type="number" min={1} max={MAX_DAYS} aria-label="days between re-maps" value={days} onChange={(e) => setDays(Number(e.target.value))} className={cx(field, "w-16")} />
      )}
      <span className="text-muted">for</span>
      <label className="flex items-center gap-1.5 text-muted">
        <input type="radio" name="map-refresh-scope" checked={!all} onChange={() => setAll(false)} />
        this repository
      </label>
      <label className="flex items-center gap-1.5 text-muted">
        <input type="radio" name="map-refresh-scope" checked={all} onChange={() => setAll(true)} />
        all repositories in {repo.split("/")[0]}
      </label>
      <Button variant="primary" size="sm" className="ml-auto" onClick={() => onCreate(mapLoopBody(repo, all, days))}>
        Create loop
      </Button>
      <Button size="sm" onClick={onNotNow}>
        Not now
      </Button>
    </div>
  );
}
