// The pure half of loops (loops.rs): the operator's local schedule choice as the UTC cadence the
// mothership stores, a cadence back in words, the Composer's `/loop <interval> <prompt>` shorthand,
// the map loops that keep architecture maps fresh, and the prompt templates the "New loop" dialog
// offers. Kept apart from the view so it is testable.
import type { Loop, LoopCadence, NewLoop } from "../types";
import { WEEKDAYS, describeCadence, toUtcCadence } from "./redTeamPlan";

export type LoopChoice =
  | { every: "interval"; minutes: number }
  | { every: "every_days"; days: number; time: string }
  | { every: "daily"; time: string }
  | { every: "weekly"; weekday: number; time: string }
  | { every: "monthly"; day: number; time: string }
  | { every: "self_paced" };

export const MIN_INTERVAL = 15;
export const MAX_INTERVAL = 7 * 24 * 60;
/** An `every_days` cadence runs 1–365 days apart (the server refuses the rest); the dialog offers these. */
export const MAX_DAYS = 365;
export const DAY_PRESETS: readonly number[] = [7, 14, 30, 60, 90];

function hm(time: string): [number, number] {
  const [h, m] = time.split(":").map((n) => Number.parseInt(n, 10));
  return [Number.isFinite(h) ? h : 0, Number.isFinite(m) ? m : 0];
}

const pad = (n: number) => String(n).padStart(2, "0");

/** The local choice as the UTC cadence the server stores. */
export function toUtcLoopCadence(choice: LoopChoice, now = new Date()): LoopCadence {
  switch (choice.every) {
    case "interval":
      return { every: "interval", minutes: Math.min(MAX_INTERVAL, Math.max(MIN_INTERVAL, Math.round(choice.minutes))) };
    case "self_paced":
      return { every: "self_paced" };
    case "daily": {
      const [h, m] = hm(choice.time);
      const local = new Date(now);
      local.setHours(h, m, 0, 0);
      return { every: "daily", hour: local.getUTCHours(), minute: local.getUTCMinutes() };
    }
    case "every_days": {
      const [h, m] = hm(choice.time);
      const local = new Date(now);
      local.setHours(h, m, 0, 0);
      const days = Math.round(choice.days);
      return { every: "every_days", days: Number.isFinite(days) ? Math.min(MAX_DAYS, Math.max(1, days)) : 1, hour: local.getUTCHours(), minute: local.getUTCMinutes() };
    }
    default:
      return toUtcCadence(choice, now) as LoopCadence;
  }
}

/** The stored UTC cadence back as a local choice, for editing. */
export function toLocalChoice(cadence: LoopCadence, now = new Date()): LoopChoice {
  switch (cadence.every) {
    case "interval":
      return { every: "interval", minutes: cadence.minutes };
    case "self_paced":
      return { every: "self_paced" };
    case "daily": {
      const d = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate(), cadence.hour, cadence.minute));
      return { every: "daily", time: `${pad(d.getHours())}:${pad(d.getMinutes())}` };
    }
    case "every_days": {
      const d = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate(), cadence.hour, cadence.minute));
      return { every: "every_days", days: cadence.days, time: `${pad(d.getHours())}:${pad(d.getMinutes())}` };
    }
    case "weekly": {
      const d = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate(), cadence.hour, cadence.minute));
      d.setUTCDate(d.getUTCDate() + ((cadence.weekday - ((d.getUTCDay() + 6) % 7) + 7) % 7));
      return { every: "weekly", weekday: (d.getDay() + 6) % 7, time: `${pad(d.getHours())}:${pad(d.getMinutes())}` };
    }
    case "monthly": {
      const d = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), Math.min(cadence.day, 28), cadence.hour, cadence.minute));
      return { every: "monthly", day: cadence.day > 28 ? cadence.day : d.getDate(), time: `${pad(d.getHours())}:${pad(d.getMinutes())}` };
    }
  }
}

/** "every 15 minutes" / "every 2 hours" / "every day at 09:00" / "self-paced", in local time. */
export function describeLoopCadence(cadence: LoopCadence, now = new Date()): string {
  switch (cadence.every) {
    case "interval":
      return `every ${intervalWords(cadence.minutes)}`;
    case "self_paced":
      return "self-paced: each run picks the next";
    case "daily": {
      const local = toLocalChoice(cadence, now) as { time: string };
      return `every day at ${local.time}`;
    }
    case "every_days": {
      const local = toLocalChoice(cadence, now) as { time: string };
      return `every ${cadence.days} ${cadence.days === 1 ? "day" : "days"} at ${local.time}`;
    }
    default:
      return describeCadence(cadence, now).replace(/^Every /, "every ").replace(/^Monthly /, "monthly ");
  }
}

export function intervalWords(minutes: number): string {
  if (minutes % 1440 === 0) {
    const d = minutes / 1440;
    return d === 1 ? "day" : `${d} days`;
  }
  if (minutes % 60 === 0) {
    const h = minutes / 60;
    return h === 1 ? "hour" : `${h} hours`;
  }
  return `${minutes} minutes`;
}

/** "in 3h", "in 12m", "now", "3h ago": how far a time is from now. */
export function relative(iso: string | null, now = Date.now()): string {
  if (!iso) return "—";
  const diff = Math.round((new Date(iso).getTime() - now) / 60_000);
  const span = (m: number) => (m >= 2880 ? `${Math.round(m / 1440)}d` : m >= 90 ? `${Math.round(m / 60)}h` : `${m}m`);
  if (Math.abs(diff) < 1) return "now";
  return diff > 0 ? `in ${span(diff)}` : `${span(-diff)} ago`;
}

/** `15m`, `2h`, `1d`, `90min`, `1 hour`: an interval in minutes, or null. */
export function parseInterval(raw: string): number | null {
  const m = raw.trim().toLowerCase().match(/^(\d+(?:\.\d+)?)\s*(m|min|mins|minute|minutes|h|hr|hrs|hour|hours|d|day|days)$/);
  if (!m) return null;
  const n = Number.parseFloat(m[1]);
  const unit = m[2][0];
  const minutes = unit === "m" ? n : unit === "h" ? n * 60 : n * 1440;
  return Number.isFinite(minutes) && minutes > 0 ? Math.round(minutes) : null;
}

/**
 * The Composer's `/loop` shorthand, like Claude Code's: `/loop 1h check CI on main` runs every hour;
 * `/loop check CI on main` (no interval) is self-paced; `/loop 14d …` runs every 14 days, anchored
 * at this UTC time of day. Null when the text is not a `/loop` command.
 */
export function parseLoopCommand(text: string, now = new Date()): { cadence: LoopCadence; prompt: string; error: string | null } | null {
  const m = text.trim().match(/^\/loop(?:\s+([\s\S]*))?$/i);
  if (!m) return null;
  const rest = (m[1] ?? "").trim();
  const [first, ...more] = rest.split(/\s+/);
  const minutes = first ? parseInterval(first) : null;
  const prompt = (minutes != null ? more.join(" ") : rest).trim();
  if (!prompt) return { cadence: { every: "self_paced" }, prompt: "", error: "Say what the loop should do: /loop 1h check CI on main and fix flakes" };
  if (minutes == null) return { cadence: { every: "self_paced" }, prompt, error: null };
  if (minutes < MIN_INTERVAL) return { cadence: { every: "interval", minutes }, prompt, error: `A loop runs at most every ${MIN_INTERVAL} minutes` };
  if (minutes <= MAX_INTERVAL) return { cadence: { every: "interval", minutes }, prompt, error: null };
  // Past a week the shorthand speaks in whole days (8d–365d), anchored at the current UTC time.
  const days = Math.round(minutes / 1440);
  if (minutes % 1440 !== 0 || days > MAX_DAYS)
    return { cadence: { every: "interval", minutes }, prompt, error: "Over 7 days a loop runs in whole days, up to 365 of them: /loop 14d check CI" };
  return { cadence: { every: "every_days", days, hour: now.getUTCHours(), minute: now.getUTCMinutes() }, prompt, error: null };
}

/** A short name for a loop from its prompt. */
export function nameFromPrompt(prompt: string): string {
  const line = prompt.trim().split("\n")[0].replace(/\s+/g, " ");
  return line.length > 60 ? `${line.slice(0, 57).trimEnd()}…` : line || "Loop";
}

/** The Loops page's row line: when a colony loop runs, or which maps a map loop refreshes. */
export function describeLoop(l: Pick<Loop, "kind" | "repo" | "cadence">): string {
  if ((l.kind ?? "colony") !== "map") return describeLoopCadence(l.cadence);
  const what = l.repo.endsWith("/*") ? `Refreshes the maps of every repository in ${l.repo.slice(0, -2)}` : `Refreshes the map of ${l.repo}`;
  return `${what} · ${describeLoopCadence(l.cadence)}`;
}

/** A default name for a map loop, from its scope. */
export function mapLoopName(repo: string): string {
  return repo.endsWith("/*") ? `Keep every map in ${repo.slice(0, -2)} fresh` : `Keep the map of ${repo} fresh`;
}

/** The POST body for a map loop at 03:00 local: this repository, or every repository in its org. */
export function mapLoopBody(repo: string, all: boolean, days: number, now = new Date()): NewLoop {
  const scope = all ? `${repo.split("/")[0]}/*` : repo;
  return {
    name: mapLoopName(scope),
    repo: scope,
    prompt: "",
    kind: "map",
    cadence: toUtcLoopCadence({ every: "every_days", days, time: "03:00" }, now),
    tz_offset_minutes: -now.getTimezoneOffset(),
    autopilot: true,
    enabled: true,
  };
}

export const LOOP_TEMPLATES: { label: string; prompt: string; choice: LoopChoice }[] = [
  {
    label: "Triage new issues",
    prompt:
      "Triage issues opened since the last run: label them, ask for missing reproduction details in a comment, close exact duplicates with a link, and fix any that are small and clear in one pull request.",
    choice: { every: "daily", time: "09:00" },
  },
  {
    label: "Keep dependencies current",
    prompt:
      "Update outdated dependencies that have no breaking changes, run every check, and open one pull request with the updates and a short changelog of what moved. Leave major upgrades for a person and list them in the PR.",
    choice: { every: "weekly", weekday: 0, time: "08:00" },
  },
  {
    label: "Fix flaky tests from last night's CI",
    prompt:
      "Look at the failed and re-run CI jobs on the default branch from the last 24 hours, find tests that failed and then passed without a code change, and fix the flakiness at its cause. Open a pull request per root cause.",
    choice: { every: "daily", time: "07:00" },
  },
  {
    label: "Summarise yesterday's merged PRs into CHANGELOG",
    prompt:
      "Add a user-facing CHANGELOG entry under Unreleased for every pull request merged yesterday that does not have one yet, in the file's existing style, and open one pull request.",
    choice: { every: "daily", time: "06:00" },
  },
];

export { WEEKDAYS };
