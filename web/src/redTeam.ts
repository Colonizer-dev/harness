// Red-team runs (issue #212): pure helpers shared by the overview card and the nest overlay.
// What a run "is doing" — gated at the nest's edge, or raiding — lives here so the two
// viewers cannot disagree about which ants to draw.
import { isLive, type Tone } from "./components/ui";
import type { RedTeamHunter, RedTeamRun, RedTeamSynthesis, Session } from "./types";

/** How the UI reads each run state. `armed` and `waiting` are the gated pair; `running` and `draining` the raiding pair. */
export const RED_TEAM_STATE: Record<RedTeamRun["state"], { label: string; tone: Tone }> = {
  armed: { label: "Armed — waiting for the nest to empty", tone: "warn" },
  waiting: { label: "Queued for a slot", tone: "warn" },
  running: { label: "Raiding", tone: "err" },
  draining: { label: "Draining", tone: "warn" },
  done: { label: "Done", tone: "ok" },
  stopped: { label: "Stopped", tone: "neutral" },
};

/** How the UI reads each synthesis state (issue #309): the merge colony's own lifecycle after a raid. */
export const RED_TEAM_SYNTHESIS: Record<RedTeamSynthesis["state"], { label: string; tone: Tone }> = {
  pending: { label: "Synthesis queued", tone: "warn" },
  running: { label: "Synthesizing", tone: "accent" },
  done: { label: "Synthesis done", tone: "ok" },
  failed: { label: "Synthesis failed", tone: "err" },
};

/** How many colonies have their microVM up; the gate's foot-soldier count. */
export function liveCount(sessions: Session[]): number {
  return sessions.filter((s) => isLive(s.status)).length;
}

/** The swarm is out: ants march. */
export function isRaiding(run: RedTeamRun): boolean {
  return run.state === "running" || run.state === "draining";
}

/** The gate is shut: armed hunters wait at the edge of the nest. */
export function isGated(run: RedTeamRun): boolean {
  return run.state === "armed" || run.state === "waiting";
}

/** Anything the ants are still part of — the two terminal states are the only ones that end them. */
export function isActive(run: RedTeamRun): boolean {
  return run.state !== "done" && run.state !== "stopped";
}

/**
 * Why a run is not raiding yet. The server's own reason wins when it set one; otherwise the run's
 * state is armed or waiting, so "the nest is still full" is the honest default.
 */
export function gateMessage(run: RedTeamRun, live: number): string {
  if (run.gate_reason) return run.gate_reason;
  return `armed — waiting for the nest to empty (${live} live)`;
}

/** The run's hunter sessions that still exist in the list, joined by session_id — the ants that have a chamber to land on. */
export function raidTarget(run: RedTeamRun, sessions: Session[]): Session[] {
  const byId = new Map(sessions.map((s) => [s.id, s]));
  const pick = (hunter: RedTeamHunter): Session | null => byId.get(hunter.session_id) ?? null;
  return run.hunters.map(pick).filter((s): s is Session => s !== null);
}