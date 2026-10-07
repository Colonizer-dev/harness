// What the carrier ants say: one short line each, read off the settler's own state. Pure, so
// the nest overview and the chamber zoom share it.
import { describeTool } from "../components/activity";
import type { SubagentState, SubagentView } from "../sessionStream";

/** A bubble's two strings: the clipped line it shows, and the full line its title keeps. */
export interface BubbleText {
  text: string;
  title: string;
}

/** A bubble stays one short line; longer readings clip here, the full string living in `title`. */
export const BUBBLE_MAX_LENGTH = 72;

/** The dot (and border) a bubble borrows per settler state, in the inbox KIND_DOT's spirit. */
export const BUBBLE_TONE: Record<SubagentState, string> = {
  working: "var(--accent)",
  thinking: "var(--faint)",
  writing: "var(--info)",
  done: "var(--ok)",
  continued: "var(--faint)",
};

function clipBubble(full: string): BubbleText {
  const clean = full.replace(/\s+/g, " ").trim();
  if (clean.length <= BUBBLE_MAX_LENGTH) return { text: clean, title: clean };
  return { text: `${clean.slice(0, BUBBLE_MAX_LENGTH - 1)}…`, title: clean };
}

const lowerFirst = (value: string): string => value.slice(0, 1).toLowerCase() + value.slice(1);

const firstLine = (report: string): string => (report.split("\n")[0] ?? "").trim();

/** What one settler's carrier says: its tool while working, its last tool while thinking. */
export function settlerSays(settler: Pick<SubagentView, "state" | "current" | "last" | "report">): BubbleText {
  switch (settler.state) {
    case "working": {
      const tool = settler.current ?? settler.last;
      return clipBubble(tool ? describeTool(tool.name, tool.input).label : "Working…");
    }
    case "thinking": {
      const last = settler.last;
      return clipBubble(last ? `Thinking after ${lowerFirst(describeTool(last.name, last.input).label)}` : "Thinking…");
    }
    case "writing": {
      const line = firstLine(settler.report);
      return clipBubble(line ? line : "Writing its report");
    }
    case "done": {
      const line = firstLine(settler.report);
      return clipBubble(line ? `Done: ${line}` : "Done");
    }
    case "continued":
      return clipBubble("Carried on further down");
  }
}

/** What the colony's own ant says: the live stream detail while it has one, else the feed line. */
export function colonySays(liveDetail: string | null | undefined, feedText: string): BubbleText {
  const live = (liveDetail ?? "").trim();
  return clipBubble(live ? live : feedText);
}

/** The overview shows at most this many carrier bubbles, however many ants ride the tunnels. */
export const MAX_ANT_BUBBLES = 4;

/**
 * Deterministic density: one bubble per ant at most, `limit` overall; settlers ride in launch
 * order, so the tail keeps the newest crew. Pure for the tests.
 */
export function planAntBubbles<T>(riders: readonly T[], limit: number = MAX_ANT_BUBBLES): T[] {
  if (limit <= 0) return [];
  return riders.slice(Math.max(0, riders.length - limit));
}
