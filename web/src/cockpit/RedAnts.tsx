// Red-team raids (issue #212): a decoration-only layer of red ants storming the plot a run is
// attacking. The ants are IconAnt tinted with the error red — the same worker on a new mission.
// The layer is aria-hidden and pointer-events-none, so the nest underneath stays fully
// interactive. Motion is the rt-* keyframes in index.css, applied inline the way the nest applies
// ck-carry; reduced motion freezes the column at the edge, which is exactly the inline `left`.
import type { CSSProperties, ReactElement } from "react";

import { IconAnt } from "../components/icons";

/** The layer never draws more ants than this, however large the swarm the run asked for. */
const RT_MAX = 6;

export function RedAnts({
  mode,
  count,
  className,
}: {
  /** "raiding" marches the column onto the target; "waiting" clusters it at the plot's edge. */
  mode: "waiting" | "raiding";
  /** The swarm size, from the run; the layer caps its drawing at RT_MAX. */
  count: number;
  className?: string;
}): ReactElement {
  const ants = Math.max(1, Math.min(count, RT_MAX));
  return (
    <div aria-hidden="true" className={`pointer-events-none absolute inset-0 overflow-hidden ${className ?? ""}`}>
      {mode === "raiding" && <TargetPulse />}
      {Array.from({ length: ants }, (_, i) => {
        const marching = mode === "raiding";
        return (
          <span
            key={i}
            className={`pointer-events-none absolute text-err ${marching ? "rt-march" : "rt-twitch"}`}
            style={marching ? raidingStyle(i) : waitingStyle(i)}
          >
            <IconAnt size={16} />
          </span>
        );
      })}
    </div>
  );
}

/** The target at the heart of the nest: a dashed red ring breathing where the raid lands. */
function TargetPulse(): ReactElement {
  return (
    <span className="pointer-events-none absolute text-err" style={{ left: "50%", top: "38%" }}>
      <span
        aria-hidden="true"
        className="rt-pulse absolute block rounded-full border-[1.5px] border-dashed border-current opacity-70"
        style={{ marginLeft: -23, marginTop: -23, width: 46, height: 46, animation: "rt-pulse 2.6s ease-in-out infinite" }}
      />
    </span>
  );
}

/** A march: out of the left edge, wobbling, fading in and landing near the centre (≥4s cycle). */
function raidingStyle(i: number): CSSProperties {
  return {
    top: `${12 + ((i * 37) % 55)}%`,
    left: "0%",
    opacity: 0.9,
    animation: `rt-march ${4.5 + (i % 3)}s ease-in-out ${i * 0.8}s infinite`,
  };
}

/** Waits at the edge: a barely-there twitch, so the column reads alive without marching. */
function waitingStyle(i: number): CSSProperties {
  return {
    top: `${16 + ((i * 29) % 60)}%`,
    left: `${2 + (i % 3) * 3.5}%`,
    opacity: 0.9,
    animation: `rt-twitch ${2.6 + (i % 4) * 1.1}s ease-in-out ${i * 0.45}s infinite`,
  };
}