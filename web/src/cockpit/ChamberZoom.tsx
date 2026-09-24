// The chamber zoom: a chamber (or tunnel) click selects the colony AND grows this full-plot
// overlay out of the chamber (`ck-zoom` from the chamber's slot). Inside is the chamber's
// interior — one big blob in dark soil, the tunnel mouth up top, side tunnels from the
// colony's steps — with the colony's own ant and every settler working in it.
import { useEffect, useRef, type CSSProperties, type ReactElement } from "react";

import { AntAvatar, type AntActivity, type AntState } from "../components/AntAvatar";
import { SESSION_STATUS } from "../components/ui";
import { antActivity, describeTool } from "../components/activity";
import type { Session, SessionStatus } from "../types";
import type { SubagentView } from "../sessionStream";
import { AntBubble } from "./AntBubble";
import { BUBBLE_TONE, colonySays, settlerSays } from "./bubbles";
import { feedEntry } from "./feed";
import { branchPaths, tunnelPath, tunnelSeed } from "./nest";
import { taskLine } from "../summary";

/** Closed, open on a colony, or playing the zoom-out before unmounting. */
export type ZoomState =
  | { phase: "closed" }
  | { phase: "open" | "closing"; id: string; x: number; y: number };

export type ZoomAction =
  | { type: "open"; id: string; x: number; y: number }
  | { type: "close" }
  | { type: "closed" }
  | { type: "selection"; id: string | null };

/**
 * The zoom's state machine, pure so the tests pin it without rendering. Opening always lands
 * open; Escape/the back button/the soil run the zoom-out first (`closing`); an outside
 * selection change drops the zoom outright.
 */
export function zoomReducer(state: ZoomState, action: ZoomAction): ZoomState {
  switch (action.type) {
    case "open":
      return { phase: "open", id: action.id, x: action.x, y: action.y };
    case "close":
      return state.phase === "open" ? { ...state, phase: "closing" } : state;
    case "closed":
      return state.phase === "closed" ? state : { phase: "closed" };
    case "selection":
      return state.phase !== "closed" && action.id !== state.id ? { phase: "closed" } : state;
  }
}

/** The den in zoom-local geometry: a 480×460 stage, the chamber low so the tunnel mouth drops in from above. */
const DEN = { x: 240, y: 290, r: 140 };
const DEN_BOX = { width: 480, height: 460 };

/** Where the crew works: the first three inside the blob, the rest at the wall where the side tunnels start. */
const SPOTS: { left: string; top: string }[] = [
  { left: "28%", top: "64%" },
  { left: "52%", top: "78%" },
  { left: "72%", top: "58%" },
  { left: "15%", top: "60%" },
  { left: "85%", top: "60%" },
  { left: "38%", top: "88%" },
  { left: "62%", top: "88%" },
];

/** The colony's own ant mirrors the colony the way the overview's stand-in does. */
function colonyAntState(status: SessionStatus): AntState {
  switch (status) {
    case "running":
    case "starting":
      return "working";
    case "waiting_for_answer":
    case "idle":
      return "thinking";
    case "publishing":
      return "writing";
    case "pr_opened":
    case "merged":
      return "done";
    default:
      return "paused";
  }
}

/** What a settler's ant carries: its current tool's doing, the last tool's between steps. */
function settlerActivity(settler: SubagentView): AntActivity {
  const tool = settler.current ?? settler.last;
  return tool ? antActivity(describeTool(tool.name, tool.input)) : "run";
}

export function ChamberZoom({
  session,
  settlers,
  liveDetail = null,
  slot,
  blob,
  edge,
  closing,
  onClose,
  onClosed,
  onOpen,
}: {
  session: Session;
  /** Real settlers, like the nest's: only the selected colony has any. */
  settlers: SubagentView[];
  liveDetail?: string | null;
  /** The chamber's plot position, which the overlay grows out of (and back into). */
  slot: { x: number; y: number };
  /** The chamber's hand-drawn radii and tone edge, so the den reads as the same chamber. */
  blob: string;
  edge: string;
  closing: boolean;
  onClose: () => void;
  onClosed: () => void;
  onOpen: (id: string) => void;
}): ReactElement {
  // Focus the way back on open; no full focus trap, the overlay is one Escape away.
  const backRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    backRef.current?.focus();
  }, []);
  // Escape closes only the zoom: the inspector has none, and the header's org menu
  // preventDefaults its own, so a preventDefaulted event means a nearer menu answered it.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      event.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Unmounts on animationend; under reduced motion no animation runs, so unmount at once.
  // The timeout covers a dropped animationend either way.
  useEffect(() => {
    if (!closing) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      onClosed();
      return;
    }
    const timer = window.setTimeout(onClosed, 350);
    return () => window.clearTimeout(timer);
  }, [closing, onClosed]);

  const status = SESSION_STATUS[session.status];
  const steps = settlers.reduce((total, settler) => total + settler.steps, 0);
  const seed = tunnelSeed(session.repo, session.issue);
  const mouth = tunnelPath(DEN, DEN_BOX, seed);
  const branches = branchPaths(DEN, steps, DEN_BOX, seed);
  const busy = session.status === "running" || session.status === "starting";
  const colonyBubble = { ...colonySays(liveDetail, feedEntry(session).text), tone: edge };
  const issue = session.issue != null ? ` #${session.issue}` : "";
  const settlerCount = `${settlers.length} settler${settlers.length === 1 ? "" : "s"}`;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={`Inside ${session.repo}${issue}`}
      className="absolute inset-0 z-[5]"
      style={{
        transformOrigin: `${slot.x}px ${slot.y}px`,
        animation: closing ? "ck-zoom 0.3s ease-in reverse" : "ck-zoom 0.3s ease-out",
      }}
      onAnimationEnd={(event) => {
        if (closing && event.target === event.currentTarget) onClosed();
      }}
    >
      {/* The dark soil around the chamber: clicking it zooms back out. */}
      <div
        className="absolute inset-0 cursor-zoom-out"
        onClick={onClose}
        style={{ background: "linear-gradient(rgb(0 0 0 / 0.5), rgb(0 0 0 / 0.66))" }}
      />
      <header
        className="absolute inset-x-0 top-0 z-[2] flex items-center gap-2.5 nest-glass border-b border-border px-3.5 py-2"
      >
        <button
          ref={backRef}
          type="button"
          onClick={onClose}
          aria-label="back to the nest"
          className="shrink-0 cursor-pointer rounded-md px-2 py-1 text-[13px] text-muted transition-colors hover:bg-panel-2 hover:text-text"
        >
          ← nest
        </button>
        <span className="shrink-0 font-mono text-[12px] text-text">
          {session.repo}{issue}
        </span>
        <span className="shrink-0 font-mono text-[11px]" style={{ color: edge }}>
          {status?.label ?? ""}
        </span>
        <span className="min-w-0 flex-1 truncate text-[13px] text-muted">
          {taskLine(session, status?.label ?? "")}
        </span>
        <span className="shrink-0 font-mono text-[11px] text-faint tabular-nums">
          {steps} steps · {settlerCount}
        </span>
        <button
          type="button"
          onClick={() => onOpen(session.id)}
          className="shrink-0 cursor-pointer rounded-md bg-text px-3 py-1 text-[13px] font-medium text-bg transition-opacity hover:opacity-85"
        >
          open colony →
        </button>
      </header>
      {/* Clicks on the blob stay inside; a double-click opens the colony (the overlay now covers the chamber button). */}
      <div
        className="absolute inset-0 top-[41px] flex items-center justify-center"
        onClick={(event) => {
          // The wrapper covers the soil, so a click landing straight on it is a soil click
          // and zooms back out; clicks on the blob or an ant bubble up with a deeper target.
          if (event.target === event.currentTarget) onClose();
        }}
        onDoubleClick={() => onOpen(session.id)}
      >
        <div className="relative h-full" style={{ aspectRatio: "480 / 460", maxWidth: "100%" }}>
          <svg viewBox="0 0 480 460" className="absolute inset-0 h-full w-full" style={{ overflow: "visible" }} aria-hidden="true">
            <g>
              <path d={mouth} fill="none" stroke="var(--tunnel-wall)" strokeWidth="14" strokeLinecap="round" strokeLinejoin="round" />
              <path d={mouth} fill="none" stroke="var(--tunnel-floor)" strokeWidth="8" strokeLinecap="round" strokeLinejoin="round" />
              {busy && (
                <path
                  d={mouth}
                  fill="none"
                  stroke={edge}
                  strokeWidth="1.5"
                  strokeLinecap="round"
                  strokeDasharray="3 6"
                  opacity="0.7"
                  style={{ animation: "ck-flow 1.6s linear infinite" }}
                />
              )}
            </g>
            {branches.map((branch, k) => (
              <g key={k} style={{ animation: "ck-dig 1.8s ease-out both" }}>
                <path d={branch.d} fill="none" stroke="var(--tunnel-wall)" strokeWidth="11" strokeLinecap="round" strokeLinejoin="round" />
                <path d={branch.d} fill="none" stroke="var(--tunnel-floor)" strokeWidth="6" strokeLinecap="round" strokeLinejoin="round" />
              </g>
            ))}
          </svg>
          <div
            aria-hidden="true"
            className="absolute border-[2px]"
            style={{
              left: "20.8%",
              top: "32.6%",
              width: "58.3%",
              height: "60.9%",
              borderRadius: blob,
              borderColor: edge,
              background: "radial-gradient(120% 120% at 30% 20%, var(--panel-2), var(--plot))",
            }}
          />
          {/* The colony's own ant: the orchestrator, bigger, saying the live detail. */}
          <div data-zoom-ant="colony" className="absolute -translate-x-1/2 -translate-y-1/2" style={{ left: "46%", top: "40%" }}>
            <span className="relative flex flex-col items-center">
              <span className="absolute bottom-full left-1/2 -translate-x-1/2 pb-1.5">
                <AntBubble text={colonyBubble.text} title={colonyBubble.title} tone={colonyBubble.tone} />
              </span>
              <span className="block" style={{ animation: "ck-wander 11s ease-in-out 0s infinite" } as CSSProperties}>
                <AntAvatar state={colonyAntState(session.status)} size={46} phase={0} ground={false} framed={false} />
              </span>
              <span className="mt-0.5 font-mono text-[10px] text-muted">orchestrator</span>
            </span>
          </div>
          {settlers.map((settler, i) => {
            const spot = SPOTS[i % SPOTS.length];
            const saying = settlerSays(settler);
            return (
              <div
                key={settler.agent.id}
                data-zoom-ant={settler.agent.id}
                className="absolute -translate-x-1/2 -translate-y-1/2"
                style={{ left: spot.left, top: spot.top }}
              >
                <span className="relative flex flex-col items-center">
                  <span className="absolute bottom-full left-1/2 -translate-x-1/2 pb-1.5">
                    <AntBubble text={saying.text} title={saying.title} tone={BUBBLE_TONE[settler.state]} />
                  </span>
                  <span
                    className="block"
                    style={{ animation: `ck-wander ${8 + (i % 3) * 2.5}s ease-in-out ${-(i * 1.7)}s infinite` } as CSSProperties}
                  >
                    <AntAvatar
                      state={settler.state}
                      role={settler.role}
                      activity={settlerActivity(settler)}
                      size={30}
                      phase={i + 1}
                      ground={false}
                      framed={false}
                    />
                  </span>
                  <span className="mt-0.5 max-w-[90px] truncate font-mono text-[10px] text-muted">{settler.name}</span>
                </span>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
