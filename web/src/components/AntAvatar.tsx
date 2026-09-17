// A settler's avatar: a pixel-art worker ant in side profile (2 viewBox units per pixel), facing right, animated by what
// the settler is doing.
// The motion lives in index.css under `.ant`, keyed on data-s / data-activity / data-error / data-role,
// and stops under prefers-reduced-motion. Every moving part is a named group pivoting at its own joint.
import type { CSSProperties } from "react";
import type { SubagentState } from "../sessionStream";

export type AntState = SubagentState | "paused";
export type AntRole =
  | "scout"
  | "builder"
  | "surveyor"
  | "inspector"
  | "warden"
  | "tester"
  | "tracker"
  | "scribe"
  | "mender"
  | "mason"
  | "cartographer"
  | "pioneer";
/** What kind of tool is running; only read while state is "working". */
export type AntActivity = "run" | "search" | "read" | "edit" | "build" | "test";

const DOING: Record<AntState, string> = {
  working: "working",
  thinking: "thinking",
  writing: "writing its report",
  done: "done",
  continued: "continued further down",
  paused: "paused",
};

const ROLE_NAME: Record<AntRole, string> = {
  scout: "Scout",
  builder: "Builder",
  surveyor: "Surveyor",
  inspector: "Inspector",
  warden: "Warden",
  tester: "Tester",
  tracker: "Tracker",
  scribe: "Scribe",
  mender: "Mender",
  mason: "Mason",
  cartographer: "Cartographer",
  pioneer: "Pioneer",
};

/** Staggers every loop by -0.37s per step, so settlers shown together never move in lockstep. */
export const antPhase = (phase: number) => ({ "--ant-phase": `${-(phase * 0.37)}s` }) as CSSProperties;

export function AntAvatar({
  state,
  role = "pioneer",
  activity = "run",
  error = false,
  size = 36,
  phase = 0,
  ground = true,
  framed = true,
}: {
  state: AntState;
  role?: AntRole;
  activity?: AntActivity;
  /** Set for ~1.2s after a tool result with is_error: the ant stumbles and carries on. */
  error?: boolean;
  /** Frame size in px; the drawing is 6px smaller. Without a frame, the drawing's width. */
  size?: number;
  /** 0, 1, 2… for settlers shown together. */
  phase?: number;
  /** Hide the ground line (a shared trail draws it instead). */
  ground?: boolean;
  /** Draw the tinted frame around the ant; a crew's strip stands its ants straight on the trail. */
  framed?: boolean;
}) {
  const w = framed ? size - 6 : size;
  const ant = (
    <svg
      className="ant"
      data-s={state}
      data-role={role}
      data-activity={activity}
      data-error={error ? "true" : "false"}
      data-ground={ground ? "true" : "false"}
      viewBox="0 0 52 40"
      width={w}
      height={(w * 40) / 52}
      role="img"
      aria-label={`${ROLE_NAME[role]} Settler, ${DOING[state]}`}
      style={antPhase(phase)}
    >
      <line className="ant-ground" x1="0" y1="35" x2="52" y2="35" />
      <g className="ant-body">
        <g className="ant-legs-far">
          <g className="ant-leg ant-leg-front-far">
            <rect x="28" y="26" width="2" height="2" />
            <rect x="30" y="28" width="2" height="2" />
            <rect x="30" y="30" width="2" height="2" />
            <rect x="32" y="32" width="2" height="2" />
          </g>
          <g className="ant-leg ant-leg-mid-far">
            <rect x="24" y="26" width="2" height="2" />
            <rect x="24" y="28" width="2" height="2" />
            <rect x="24" y="30" width="2" height="2" />
            <rect x="24" y="32" width="2" height="2" />
          </g>
          <g className="ant-leg ant-leg-back-far">
            <rect x="20" y="26" width="2" height="2" />
            <rect x="18" y="28" width="2" height="2" />
            <rect x="18" y="30" width="2" height="2" />
            <rect x="16" y="32" width="2" height="2" />
          </g>
        </g>
        <g className="ant-fill ant-gaster">
          <rect x="8" y="18" width="8" height="2" />
          <rect x="6" y="20" width="12" height="2" />
          <rect x="4" y="22" width="16" height="2" />
          <rect x="6" y="24" width="12" height="2" />
          <rect x="8" y="26" width="8" height="2" />
        </g>
        <rect className="ant-seam" x="12" y="20" width="2" height="6" />
        <rect className="ant-fill ant-petiole" x="20" y="22" width="2" height="2" />
        <g className="ant-fill ant-thorax">
          <rect x="24" y="18" width="4" height="2" />
          <rect x="22" y="20" width="8" height="4" />
          <rect x="24" y="24" width="4" height="2" />
        </g>
        {/* Role accessories on the body. Only the one matching data-role is shown. */}
        <g className="ant-role">
          <g className="ant-role-scout">
            <rect className="ink" x="24" y="14" width="4" height="4" />
          </g>
          <g className="ant-role-surveyor">
            <rect className="ink" x="20" y="16" width="10" height="2" />
          </g>
          <g className="ant-role-warden">
            <rect className="ink" x="24" y="20" width="4" height="4" />
            <rect className="ink" x="25" y="24" width="2" height="2" />
          </g>
          <g className="ant-role-tester">
            <rect className="ink" x="8" y="20" width="2" height="6" />
            <rect className="ink" x="14" y="20" width="2" height="6" />
          </g>
          <g className="ant-role-mender">
            <rect className="ink" x="8" y="20" width="2" height="2" />
            <rect className="ink" x="14" y="20" width="2" height="2" />
            <rect className="ink" x="10" y="22" width="4" height="2" />
            <rect className="ink" x="8" y="24" width="2" height="2" />
            <rect className="ink" x="14" y="24" width="2" height="2" />
          </g>
          <g className="ant-role-mason">
            <rect className="ink" x="22" y="16" width="8" height="2" />
            <rect x="26" y="16" width="1" height="2" fill="var(--panel)" opacity="0.7" />
          </g>
          <g className="ant-role-cartographer">
            <rect x="9" y="20" width="6" height="6" fill="var(--panel)" />
            <rect className="ink" x="11" y="22" width="2" height="2" />
            <rect className="ink" x="11" y="20" width="2" height="1" />
          </g>
          <g className="ant-role-pioneer">
            <rect className="ink" x="8" y="14" width="8" height="4" />
            <rect className="ink" x="11" y="18" width="2" height="2" />
          </g>
        </g>
        <g className="ant-head">
          <g className="ant-fill ant-skull">
            <rect x="32" y="16" width="6" height="2" />
            <rect x="30" y="18" width="10" height="4" />
            <rect x="32" y="22" width="8" height="2" />
          </g>
          <rect className="ant-fill ant-jaw" x="40" y="22" width="2" height="2" />
          <g className="ant-eye">
            <rect x="36" y="18" width="2" height="2" fill="var(--panel)" />
          </g>
          <g className="ant-antennae ant-fill">
            <g className="ant-antenna-1">
              <rect x="34" y="14" width="2" height="2" />
              <rect x="36" y="12" width="2" height="2" />
              <rect x="38" y="10" width="2" height="2" />
              <rect x="40" y="8" width="2" height="2" />
            </g>
            <g className="ant-antenna-2">
              <rect x="32" y="14" width="2" height="2" />
              <rect x="32" y="12" width="2" height="2" />
              <rect x="32" y="10" width="2" height="2" />
              <rect x="34" y="8" width="2" height="2" />
            </g>
          </g>
          {/* Role accessories on the head move with it. */}
          <g className="ant-role">
            <g className="ant-role-builder">
              <rect className="ink" x="32" y="14" width="6" height="2" />
              <rect className="ink" x="30" y="16" width="2" height="2" />
              <rect className="ink" x="38" y="16" width="2" height="2" />
            </g>
            <g className="ant-role-inspector">
              <rect className="ink" x="34" y="16" width="6" height="2" />
              <rect className="ink" x="34" y="18" width="2" height="2" />
              <rect className="ink" x="38" y="18" width="2" height="2" />
              <rect className="ink" x="34" y="20" width="6" height="2" />
            </g>
            <g className="ant-role-tracker">
              <rect className="ink" x="32" y="16" width="2" height="2" />
              <rect className="ink" x="40" y="14" width="2" height="2" opacity="0.35" />
              <rect className="ink" x="42" y="12" width="2" height="2" opacity="0.35" />
            </g>
            <g className="ant-role-scribe">
              <rect className="ink" x="30" y="12" width="2" height="4" />
              <rect className="ink" x="28" y="8" width="2" height="4" />
            </g>
          </g>
        </g>
        <g className="ant-legs-near">
          <g className="ant-leg ant-leg-front">
            <rect x="30" y="26" width="2" height="2" />
            <rect x="32" y="28" width="2" height="2" />
            <rect x="32" y="30" width="2" height="2" />
            <rect x="34" y="32" width="2" height="2" />
          </g>
          <g className="ant-leg ant-leg-mid">
            <rect x="26" y="26" width="2" height="2" />
            <rect x="26" y="28" width="2" height="2" />
            <rect x="28" y="30" width="2" height="2" />
            <rect x="28" y="32" width="2" height="2" />
          </g>
          <g className="ant-leg ant-leg-back">
            <rect x="22" y="26" width="2" height="2" />
            <rect x="20" y="28" width="2" height="2" />
            <rect x="20" y="30" width="2" height="2" />
            <rect x="18" y="32" width="2" height="2" />
          </g>
        </g>
        {/* What it carries while working: search/read → lens, edit → leaf, build → block, test → flag. */}
        <g className="ant-carry">
          <g className="ant-carry-lens">
            <rect className="ink" x="42" y="14" width="6" height="2" />
            <rect className="ink" x="42" y="18" width="6" height="2" />
            <rect className="ink" x="42" y="16" width="2" height="2" />
            <rect className="ink" x="46" y="16" width="2" height="2" />
            <rect className="ink" x="40" y="20" width="2" height="2" />
          </g>
          <g className="ant-carry-leaf">
            <rect x="42" y="18" width="2" height="2" fill="var(--ok)" />
            <rect x="44" y="16" width="4" height="4" fill="var(--ok)" />
            <rect x="46" y="14" width="2" height="2" fill="var(--ok)" />
            <rect x="44" y="18" width="2" height="2" fill="var(--ok-soft)" opacity="0.6" />
          </g>
          <g className="ant-carry-block">
            <rect className="ink" x="42" y="14" width="6" height="6" />
            <rect x="42" y="17" width="6" height="1" fill="var(--panel)" opacity="0.4" />
          </g>
          <g className="ant-carry-flag">
            <rect className="ant-fill" x="42" y="10" width="2" height="12" />
            <rect className="ink" x="44" y="10" width="6" height="4" />
          </g>
        </g>
      </g>
      <g className="ant-thought ant-fill">
        <rect x="40" y="10" width="2" height="2" />
        <rect x="44" y="6" width="3" height="3" />
        <rect x="48" y="1" width="4" height="4" />
      </g>
      <path className="ant-scribble" d="M38 33H48" />
      <rect className="ant-fill ant-pebble" x="40" y="32" width="2" height="2" />
      <g className="ant-done">
        <rect x="42" y="4" width="8" height="8" fill="var(--ok)" />
        <rect x="43" y="8" width="2" height="2" fill="var(--panel)" />
        <rect x="45" y="9" width="2" height="2" fill="var(--panel)" />
        <rect x="47" y="7" width="2" height="2" fill="var(--panel)" />
        <rect x="48" y="5" width="1" height="2" fill="var(--panel)" />
      </g>
    </svg>
  );
  if (!framed) return ant;
  return (
    <span
      className="settler-frame grid shrink-0 place-items-center rounded-xl bg-accent-soft text-accent"
      data-s={state}
      style={{ width: size, height: size }}
    >
      {ant}
    </span>
  );
}
