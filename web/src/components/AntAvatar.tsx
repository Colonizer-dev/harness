// A settler's avatar: a worker ant in side profile, facing right, animated by what the settler is doing.
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
      <line className="ant-ground" x1="2" y1="35" x2="50" y2="35" />
      <g className="ant-body">
        <g className="ant-legs-far">
          <path className="ant-leg ant-leg-front-far" d="M27 21.5 30.5 18 33.5 34.5" />
          <path className="ant-leg ant-leg-mid-far" d="M24 22 24.5 18.5 25.5 34.5" />
          <path className="ant-leg ant-leg-back-far" d="M21 22 17.5 19 14 34.5" />
        </g>
        <ellipse className="ant-fill ant-gaster" cx="13.5" cy="23" rx="8.5" ry="6" transform="rotate(-8 13.5 23)" />
        <circle className="ant-fill ant-petiole" cx="20.5" cy="21.6" r="1.9" />
        <ellipse className="ant-fill ant-thorax" cx="25.5" cy="20.5" rx="5.2" ry="3.9" />
        {/* Role accessories on the body. Only the one matching data-role is shown. */}
        <g className="ant-role">
          <g className="ant-role-scout">
            <rect className="ink" x="22.8" y="13.4" width="5.4" height="3.8" rx="1.1" />
            <path className="ink-line" d="M25.5 13.4v-1.6" strokeWidth="1.1" />
          </g>
          <g className="ant-role-surveyor">
            <rect className="ink" x="20.5" y="13.9" width="9.5" height="2.6" rx="1.3" transform="rotate(-10 25.25 15.2)" />
          </g>
          <g className="ant-role-warden">
            <path className="ink" d="M22.8 18.3h5.4v2.6q0 2.3-2.7 3.2q-2.7-.9-2.7-3.2z" />
          </g>
          <g className="ant-role-tester">
            <path className="ink-line" d="M11.4 18.3v9.6M14.6 17.8v9.8" strokeWidth="1.4" />
          </g>
          <g className="ant-role-mender">
            <path className="ink-line" d="M9.8 20.6l4.4 4.4M14.2 20.6l-4.4 4.4" strokeWidth="1.4" />
          </g>
          <g className="ant-role-mason">
            <rect className="ink" x="22.6" y="13.5" width="5.8" height="3.4" />
            <path d="M25.5 13.5v1.7M22.6 15.2h5.8" fill="none" stroke="var(--panel)" strokeWidth="0.7" />
          </g>
          <g className="ant-role-cartographer">
            <circle cx="13.5" cy="22.8" r="2.7" fill="var(--panel)" stroke="var(--ant-ink)" strokeWidth="1" />
            <path className="ink" d="M13.5 20.5l1 2.3-1 2.3-1-2.3z" />
          </g>
          <g className="ant-role-pioneer">
            <rect className="ink" x="9.6" y="14.6" width="7.4" height="4.8" rx="2.2" />
            <path className="ink-line" d="M13.3 19.4v3.2" strokeWidth="1.1" />
          </g>
        </g>
        <g className="ant-head">
          <ellipse className="ant-fill" cx="34.8" cy="18" rx="4.6" ry="4.1" />
          <path className="ant-jaw" d="M38.9 19.7 40.6 21.1" />
          <g className="ant-eye">
            <circle cx="36.2" cy="17" r="1.35" fill="var(--panel)" />
            <circle className="ant-fill" cx="36.6" cy="17" r="0.7" />
          </g>
          <g className="ant-antennae">
            <path d="M36.5 14.3Q38 9.5 43.5 8.2" />
            <path d="M35.5 14.3Q35.5 9 40 6.3" />
          </g>
          {/* Role accessories on the head move with it. */}
          <g className="ant-role">
            <g className="ant-role-builder">
              <path className="ink" d="M30.4 16.4A4.5 4.5 0 0 1 39.2 16.4Z" />
              <path className="ink-line" d="M29.4 16.6H40.3" strokeWidth="1.2" />
            </g>
            <g className="ant-role-inspector">
              <circle className="ink-line" cx="36.2" cy="17" r="2.5" strokeWidth="1.1" />
              <path className="ink-line" d="M33.7 17 31.2 15.6" strokeWidth="1.1" />
            </g>
            <g className="ant-role-tracker">
              <circle className="ink" cx="33.4" cy="14.4" r="1.4" />
              <path className="ink" d="M34.6 13.7 40.6 9.4 41 12.6Z" opacity="0.3" />
            </g>
            <g className="ant-role-scribe">
              <path className="ink-line" d="M32.6 14.6 29.4 8.6" strokeWidth="1.3" />
              <circle className="ink" cx="29.1" cy="8.1" r="1" />
            </g>
          </g>
        </g>
        <g className="ant-legs-near">
          <path className="ant-leg ant-leg-front" d="M28.5 21.5 32.5 18 36 34.5" />
          <path className="ant-leg ant-leg-mid" d="M25.5 22.5 26.5 18.5 28 34.5" />
          <path className="ant-leg ant-leg-back" d="M22.5 22.5 19 19 15.5 34.5" />
        </g>
        {/* What it carries while working: search/read → lens, edit → leaf, build → block, test → flag. */}
        <g className="ant-carry">
          <g className="ant-carry-lens">
            <circle cx="44" cy="17" r="3.2" fill="none" stroke="var(--ant-ink)" strokeWidth="1.2" />
            <path d="M41.7 19.3 40.5 20.8" stroke="var(--ant-ink)" strokeWidth="1.4" strokeLinecap="round" />
          </g>
          <g className="ant-carry-leaf">
            <path d="M40.5 20.5Q40 13 48 12Q48.5 19.5 40.5 20.5Z" fill="var(--ok)" />
            <path d="M41 20 46.5 14" fill="none" stroke="var(--ok-soft)" strokeWidth="0.8" />
          </g>
          <g className="ant-carry-block">
            <rect x="40.5" y="14" width="6.5" height="6.5" rx="1" fill="var(--ant-ink)" />
            <path d="M40.5 17.2h6.5" stroke="var(--panel)" strokeWidth="0.7" />
          </g>
          <g className="ant-carry-flag">
            <path className="ant-line" d="M41 21V8.5" strokeWidth="1.2" />
            <path d="M41 8.5h6.5l-2 2.7 2 2.7H41Z" fill="var(--ant-ink)" />
          </g>
        </g>
      </g>
      <g className="ant-thought">
        <circle className="ant-fill" cx="40.5" cy="10.5" r="1" />
        <circle className="ant-fill" cx="43.6" cy="7.4" r="1.35" />
        <circle className="ant-fill" cx="47.4" cy="3.9" r="1.8" />
      </g>
      <path className="ant-scribble" d="M38.5 33q1.5-2.2 3 0t3 0t3 0" />
      <circle className="ant-fill ant-pebble" cx="41" cy="34" r="1.2" />
      <g className="ant-done">
        <circle cx="45" cy="8" r="4.2" fill="var(--ok)" />
        <path
          d="M43 8.1 44.4 9.5 47 6.7"
          fill="none"
          stroke="var(--panel)"
          strokeWidth="1.4"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
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
