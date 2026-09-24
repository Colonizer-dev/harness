// The Chat personas as ants: Pip the forager, Sarge the soldier, Silka the weaver and Mellie the
// honeypot. Each is a small top-down SVG whose motion lives in index.css under `.persona-ant`
// (antennae twitch and a gentle bob when idle; legs walk and the ant's own move plays when active),
// all stopped under prefers-reduced-motion. Separate from the settlers' pixel ants (AntAvatar),
// which share the `.ant` class names.
import type { CSSProperties, ReactElement } from "react";
import type { AntKind, Persona } from "./logic";

/** How much an ant moves: not at all, breathing (bob, antennae, its prop), or walking. A parent with
 *  `persona-ant-host` wakes a still or idle ant up to walking on hover. */
export type AntMotion = "none" | "idle" | "active";

/** Body and leg colours per ant, tuned to read on both the light and the dark panel. */
export const ANT_COLORS: Record<AntKind, { body: string; dark: string; accent: string }> = {
  forager: { body: "#d9773b", dark: "#9c4f22", accent: "#6cc24a" },
  soldier: { body: "#c8452c", dark: "#8a2a1a", accent: "#f2c14e" },
  weaver: { body: "#e09a2f", dark: "#a0661a", accent: "#7fd3c0" },
  honeypot: { body: "#7a4a2a", dark: "#4f2e18", accent: "#f5b92e" },
};

// Legs: [x at the thorax, y at the thorax, knee x, knee y, foot x, foot y] on the left side; the
// right side mirrors them around x = 20. Tripod gait: front-left, mid-right and back-left step together.
const LEGS: [number, number, number, number, number, number][] = [
  [18, 17.5, 13.5, 14.5, 10.5, 10.5],
  [17.6, 20, 12.5, 20, 8.5, 21.5],
  [18, 22.5, 13.5, 25.5, 11, 30.5],
];

function Leg({ leg, side, group }: { leg: (typeof LEGS)[number]; side: 1 | -1; group: "a" | "b" }): ReactElement {
  const x = (v: number) => (side === 1 ? v : 40 - v);
  const [ax, ay, kx, ky, fx, fy] = leg;
  return (
    <path
      className={`pa-leg pa-leg-${group}`}
      d={`M${x(ax)} ${ay} L${x(kx)} ${ky} L${x(fx)} ${fy}`}
      style={{ transformOrigin: `${x(ax)}px ${ay}px`, "--pa-dir": side } as CSSProperties}
    />
  );
}

/** One persona's ant, `size` px square. */
export function PersonaAnt({ persona, size = 28, motion = "idle", className }: { persona: Pick<Persona, "kind" | "ant" | "species">; size?: number; motion?: AntMotion; className?: string }): ReactElement {
  const { kind } = persona;
  const c = ANT_COLORS[kind];
  const soldier = kind === "soldier";
  const honeypot = kind === "honeypot";
  const head = soldier ? { rx: 6.2, ry: 5.4, cy: 11.2 } : { rx: 4.6, ry: 4.2, cy: 11.6 };
  const gaster = honeypot ? { rx: 8.2, ry: 8.6, cy: 30.2 } : { rx: 5.4, ry: 6.8, cy: 30.4 };
  const eyeY = head.cy - 0.6;
  const eyeDx = soldier ? 3 : 2.2;
  return (
    <svg
      className={["persona-ant", className].filter(Boolean).join(" ")}
      data-kind={kind}
      data-motion={motion}
      viewBox="0 0 40 40"
      width={size}
      height={size}
      role="img"
      aria-label={`${persona.ant}, ${persona.species.toLowerCase()}`}
      style={{ "--pa-body": c.body, "--pa-dark": c.dark, "--pa-accent": c.accent } as CSSProperties}
    >
      <g className="pa-bob">
        {/* Silka's silk: a thread paid out behind her, flowing while she works. */}
        {kind === "weaver" && <path className="pa-silk" d="M20 36.5 C 24 40, 30 34, 34 37.5 S 38 33, 39 30" />}
        <g className="pa-legs">
          {LEGS.map((leg, i) => (
            <Leg key={`l${i}`} leg={leg} side={1} group={i === 1 ? "b" : "a"} />
          ))}
          {LEGS.map((leg, i) => (
            <Leg key={`r${i}`} leg={leg} side={-1} group={i === 1 ? "a" : "b"} />
          ))}
        </g>
        <g className="pa-gaster" style={{ transformOrigin: `20px ${gaster.cy}px` }}>
          <ellipse cx="20" cy={gaster.cy} rx={gaster.rx} ry={gaster.ry} className={honeypot ? "pa-honey" : "pa-fill"} />
          {honeypot ? (
            <>
              {/* The honeypot's full crop, bands stretched thin, with a highlight. */}
              <path d={`M${20 - gaster.rx + 1.6} ${gaster.cy - 2.4} Q20 ${gaster.cy - 4.2} ${20 + gaster.rx - 1.6} ${gaster.cy - 2.4}`} className="pa-band" />
              <path d={`M${20 - gaster.rx + 1} ${gaster.cy + 1.8} Q20 ${gaster.cy} ${20 + gaster.rx - 1} ${gaster.cy + 1.8}`} className="pa-band" />
              <ellipse cx="16.8" cy={gaster.cy - 4.4} rx="2" ry="1.3" className="pa-shine" />
            </>
          ) : (
            <path d={`M${20 - gaster.rx + 1.2} ${gaster.cy - 1} Q20 ${gaster.cy - 2.6} ${20 + gaster.rx - 1.2} ${gaster.cy - 1}`} className="pa-band" />
          )}
        </g>
        {honeypot && <circle className="pa-drop" cx="20" cy="38.4" r="1.2" />}
        <circle cx="20" cy="24.4" r="1.3" className="pa-fill" />
        <ellipse cx="20" cy="20" rx="3.1" ry="4.1" className="pa-fill" />
        {soldier && <path d="M18.2 18.6 L20 20.6 L21.8 18.6" className="pa-chevron" />}
        <g className="pa-head" style={{ transformOrigin: `20px ${head.cy + head.ry}px` }}>
          <g className="pa-antenna" style={{ transformOrigin: `${20 - eyeDx + 0.4}px ${head.cy - head.ry + 1.4}px`, "--pa-dir": 1 } as CSSProperties}>
            <path d={`M${20 - eyeDx + 0.4} ${head.cy - head.ry + 1.4} L${14.6 - (soldier ? 1.4 : 0)} ${head.cy - head.ry - 3} L${11.6 - (soldier ? 1.4 : 0)} ${head.cy - head.ry - 2.2}`} className="pa-line" />
            <circle cx={11.6 - (soldier ? 1.4 : 0)} cy={head.cy - head.ry - 2.2} r="0.9" className="pa-tip" />
          </g>
          <g className="pa-antenna" style={{ transformOrigin: `${20 + eyeDx - 0.4}px ${head.cy - head.ry + 1.4}px`, "--pa-dir": -1 } as CSSProperties}>
            <path d={`M${20 + eyeDx - 0.4} ${head.cy - head.ry + 1.4} L${25.4 + (soldier ? 1.4 : 0)} ${head.cy - head.ry - 3} L${28.4 + (soldier ? 1.4 : 0)} ${head.cy - head.ry - 2.2}`} className="pa-line" />
            <circle cx={28.4 + (soldier ? 1.4 : 0)} cy={head.cy - head.ry - 2.2} r="0.9" className="pa-tip" />
          </g>
          {soldier && (
            <>
              {/* Sarge's mandibles, which snap shut on whatever bug wanders by. */}
              <path className="pa-mandible" d="M18.1 6.4 C 15.6 5.2, 15.8 2.6, 18.6 2.2" style={{ transformOrigin: "18.1px 6.4px", "--pa-dir": 1 } as CSSProperties} />
              <path className="pa-mandible" d="M21.9 6.4 C 24.4 5.2, 24.2 2.6, 21.4 2.2" style={{ transformOrigin: "21.9px 6.4px", "--pa-dir": -1 } as CSSProperties} />
            </>
          )}
          <ellipse cx="20" cy={head.cy} rx={head.rx} ry={head.ry} className="pa-fill" />
          <circle cx={20 - eyeDx} cy={eyeY} r="1.35" className="pa-eye" />
          <circle cx={20 + eyeDx} cy={eyeY} r="1.35" className="pa-eye" />
          <circle cx={20 - eyeDx + 0.25} cy={eyeY + 0.25} r="0.6" className="pa-pupil" />
          <circle cx={20 + eyeDx + 0.25} cy={eyeY + 0.25} r="0.6" className="pa-pupil" />
          {soldier && <path d={`M${20 - eyeDx - 1.4} ${eyeY - 1.9} L${20 - eyeDx + 1.2} ${eyeY - 1.2} M${20 + eyeDx + 1.4} ${eyeY - 1.9} L${20 + eyeDx - 1.2} ${eyeY - 1.2}`} className="pa-brow" />}
          {kind === "forager" && (
            // Pip's leaf, held up in the jaws.
            <g className="pa-leaf" style={{ transformOrigin: "20px 7px" }}>
              <path d="M20 7.4 C 16.2 5.8, 16.4 1.6, 20 0.6 C 23.6 1.6, 23.8 5.8, 20 7.4 Z" className="pa-leaf-fill" />
              <path d="M20 7 L20 1.6" className="pa-leaf-rib" />
            </g>
          )}
          {kind === "honeypot" && (
            // Mellie's quill, for the notes.
            <g className="pa-quill" style={{ transformOrigin: "23px 7px" }}>
              <path d="M22.6 7.6 L27.6 0.8" className="pa-quill-shaft" />
              <path d="M27.6 0.8 C 25.2 1.8, 24.4 3.6, 24.6 4.9 C 26.2 4.4, 27.4 2.8, 27.6 0.8 Z" className="pa-quill-vane" />
            </g>
          )}
        </g>
      </g>
    </svg>
  );
}
