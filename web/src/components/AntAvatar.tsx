// A subagent's avatar: a worker ant seen from above, facing right, animated by what the subagent is doing. The motion
// lives in index.css under `.ant`, keyed on `data-state`, and stops for people who prefer reduced motion.
import type { SubagentState } from "../sessionStream";

/** What the ant does, per state: the label is read out in place of the animation. */
const LABEL: Record<SubagentState | "paused", string> = {
  working: "working",
  thinking: "thinking",
  writing: "writing its report",
  done: "done",
  continued: "continued further down",
  paused: "paused",
};

export function AntAvatar({ state, size = 36 }: { state: SubagentState | "paused"; size?: number }) {
  return (
    <span className="ant-frame grid shrink-0 place-items-center rounded-xl bg-accent-soft text-accent" style={{ width: size, height: size }}>
      <svg
        className="ant"
        data-state={state}
        viewBox="0 0 36 28"
        width={size - 6}
        height={((size - 6) * 28) / 36}
        role="img"
        aria-label={`Settler, ${LABEL[state]}`}
      >
        {/* The ground scrolls left while the ant walks, so it reads as moving right. */}
        <line className="ant-ground" x1="0" y1="26.5" x2="36" y2="26.5" stroke="currentColor" strokeWidth="1" strokeLinecap="round" opacity="0.35" />
        <g className="ant-body">
          {/* Two tripods, as an ant walks: while one steps forward the other pushes back. */}
          <g className="ant-legs-a" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round">
            <path d="M18 12.4 21.2 7.6" />
            <path d="M15 12.4 11.8 7.6" />
            <path d="M16.5 16 16.5 21.2" />
          </g>
          <g className="ant-legs-b" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round">
            <path d="M18 15.6 21.2 20.4" />
            <path d="M15 15.6 11.8 20.4" />
            <path d="M16.5 12 16.5 6.8" />
          </g>
          <g className="ant-antennae" fill="none" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round">
            <path d="M24 12.4 27 9 29.6 9.6" />
            <path d="M24 15.6 27 19 29.6 18.4" />
          </g>
          <ellipse cx="9" cy="14" rx="5" ry="3.7" fill="currentColor" />
          <circle cx="16.5" cy="14" r="2.3" fill="currentColor" />
          <circle cx="22.4" cy="14" r="2.6" fill="currentColor" />
        </g>
        <g className="ant-thought" fill="currentColor">
          <circle cx="27.4" cy="5.2" r="0.9" />
          <circle cx="30.3" cy="3.4" r="1.2" />
          <circle cx="33.6" cy="1.9" r="1.5" />
        </g>
        <path className="ant-scribble" d="M25 24 27 22 29 24 31 22 33 24" fill="none" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" strokeLinejoin="round" />
        <g className="ant-done">
          <circle cx="32" cy="4.2" r="3.9" fill="var(--ok)" />
          <path d="M30.3 4.3 31.6 5.6 33.9 3.2" fill="none" stroke="white" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" />
        </g>
      </svg>
    </span>
  );
}
