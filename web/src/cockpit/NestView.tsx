// The base: every colony in the workspace drawn as a nest. The mothership sits on the surface,
// each colony is a chamber dug below it, and a tunnel joins the two — thick and flowing while the
// colony works, dashed and quiet once it stops. Ants carry work up and down the live tunnels.
//
// The geometry is all in nest.ts, pinned by its own tests; this file only decides what each colony
// looks like. One honest limit shapes it: the mothership streams events for one colony at a time,
// so only the selected colony has real settlers. Every other chamber shows a single ant standing in
// for the colony itself, with its state read off the colony's status.
import { useLayoutEffect, useRef, useState, type CSSProperties, type ReactElement } from "react";

import { AntAvatar, type AntState } from "../components/AntAvatar";
import { SESSION_STATUS, type Tone } from "../components/ui";
import { needsYou } from "../notifications";
import type { SubagentView } from "../sessionStream";
import type { Session, SessionStatus } from "../types";
import {
  MAX_CHAMBERS,
  SURFACE_Y,
  branchPaths,
  normalizeBox,
  slotAt,
  surfaceGrass,
  tunnelPath,
  type NestBox,
} from "./nest";

/** The palette entry a status tone points at, for the colours picked per colony at runtime. */
const TONE_VAR: Record<Tone, string> = {
  neutral: "var(--faint)",
  info: "var(--info)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  err: "var(--err)",
  accent: "var(--accent)",
};

/**
 * The ant standing in for a whole colony. It is not a settler — the colony's real settlers are only
 * known while its stream is open — so it says what the colony as a whole is doing.
 */
function antStateFor(status: SessionStatus): AntState {
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

const isBusy = (status: SessionStatus) => status === "running" || status === "starting";

/**
 * Four hand-drawn border-radii, cycled by slot. A single radius makes every chamber the same circle;
 * rotating through these is what stops the nest looking like a chart of bubbles.
 */
const BLOBS = [
  "48% 52% 44% 56% / 52% 46% 54% 48%",
  "52% 48% 56% 44% / 46% 54% 48% 52%",
  "45% 55% 50% 50% / 55% 45% 55% 45%",
  "54% 46% 48% 52% / 48% 56% 44% 52%",
] as const;

/** `webshop#42` when the chamber is wide enough to read it, `#42` when it is not. */
function chamberLabel(session: Session, diameter: number): string {
  const short = session.repo.split("/")[1] ?? session.repo;
  if (session.issue == null) return short;
  return diameter >= 112 ? `${short}#${session.issue}` : `#${session.issue}`;
}

export function NestView({
  sessions,
  selectedId,
  mothershipSelected,
  settlers,
  backlogCount,
  onSelect,
  onOpen,
  onSelectMothership,
  onLaunch,
}: {
  /** Already filtered to the chosen org and sorted; the view takes the first MAX_CHAMBERS. */
  sessions: Session[];
  selectedId: string | null;
  mothershipSelected: boolean;
  /** Real settlers, and only for `selectedId` — the harness streams one colony at a time. */
  settlers: SubagentView[];
  /** Open issues across the workspace's repositories; the frontier's badge. */
  backlogCount: number;
  onSelect: (id: string) => void;
  onOpen: (id: string) => void;
  onSelectMothership: () => void;
  onLaunch: () => void;
}): ReactElement {
  const plotRef = useRef<HTMLDivElement | null>(null);
  const [box, setBox] = useState<NestBox>(() => normalizeBox(0, 0));

  useLayoutEffect(() => {
    const element = plotRef.current;
    if (!element) return;
    // Measuring on every frame would re-lay the whole nest out mid-drag, so the box only moves when
    // the rounded reading actually changes.
    const measure = () => {
      const next = normalizeBox(element.clientWidth, element.clientHeight);
      setBox((current) => (current.width === next.width && current.height === next.height ? current : next));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const chambers = sessions.slice(0, MAX_CHAMBERS);
  // Side tunnels are the work a colony has done, and steps are only known for the open colony.
  const selectedSteps = settlers.reduce((total, settler) => total + settler.steps, 0);
  const mothershipX = Math.round(box.width / 2);
  const returned = sessions.filter((s) => s.status === "pr_opened").slice(0, 3);
  const queued = sessions.filter((s) => s.status === "queued").slice(0, 2);
  const waiting = sessions.filter(needsYou);
  const freeSlot = chambers.length < MAX_CHAMBERS ? slotAt(chambers.length, box) : null;

  const placed = chambers.map((session, index) => {
    const slot = slotAt(index, box);
    const tone = SESSION_STATUS[session.status]?.tone ?? "neutral";
    return { session, slot, index, edge: TONE_VAR[tone], path: tunnelPath(slot, box) };
  });

  return (
    <div className="cockpit nest relative grid min-h-0 flex-1 grid-rows-[minmax(0,1fr)_auto] overflow-hidden">
      {/* The dotted ground, fading out before it reaches the strip. */}
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 [mask-image:linear-gradient(#000_55%,transparent)]"
        style={{ backgroundImage: "radial-gradient(var(--dots) 1px, transparent 1.2px)", backgroundSize: "9px 9px" }}
      />
      <div
        aria-hidden="true"
        className="pointer-events-none absolute left-1/2 top-[8%] h-[620px] w-[620px] -translate-x-1/2 -translate-y-1/2 blur-[30px]"
        style={{ background: "radial-gradient(circle, var(--accent-soft), transparent 62%)" }}
      />

      <div ref={plotRef} className="relative min-h-0 overflow-hidden">
        <div className="absolute inset-0">
          {/* The soil: everything below the surface line is darker than the sky above it. */}
          <div
            aria-hidden="true"
            className="absolute inset-x-0 bottom-0"
            style={{
              top: SURFACE_Y,
              background: "linear-gradient(color-mix(in oklab, var(--panel-2) 70%, transparent), transparent 60%)",
            }}
          />

          {surfaceGrass(box).map((tuft, i) => (
            <span
              key={i}
              aria-hidden="true"
              className="absolute w-0.5 rounded-t-sm bg-ok opacity-50"
              style={{
                left: tuft.left,
                top: tuft.top,
                height: tuft.height,
                transform: `rotate(${tuft.rotation}deg)`,
                transformOrigin: "50% 100%",
              }}
            />
          ))}

          {/* The frontier: the issues nobody has settled yet. */}
          <button
            type="button"
            onClick={onLaunch}
            title={`frontier · ${backlogCount} open issues`}
            aria-label={`frontier: ${backlogCount} open issues, launch a colony`}
            className="absolute flex -translate-x-1/2 -translate-y-full cursor-pointer flex-col items-center transition-transform hover:scale-105"
            style={{ left: Math.round(box.width * 0.86), top: SURFACE_Y }}
          >
            <span aria-hidden="true" className="relative h-[58px] w-[70px]">
              <span className="absolute left-0 top-6 h-[34px] w-[38px] rounded-full bg-ok opacity-[0.26]" />
              <span className="absolute left-[30px] top-5 h-[38px] w-10 rounded-full bg-ok opacity-[0.32]" />
              <span className="absolute left-[14px] top-0 h-[42px] w-11 rounded-full bg-ok opacity-[0.42]" />
              <span className="absolute -right-1 -top-1 min-w-5 rounded-[10px] border border-ok bg-panel px-1.5 text-center font-mono text-[10px] font-semibold leading-[18px] text-ok tabular-nums">
                {backlogCount}
              </span>
            </span>
            <span aria-hidden="true" className="-mt-2 h-5 w-1.5 rounded-sm bg-border-strong" />
          </button>

          <svg
            viewBox={`0 0 ${box.width} ${box.height}`}
            className="pointer-events-none absolute inset-0 h-full w-full"
            aria-hidden="true"
          >
            <line x1="0" y1={SURFACE_Y} x2={box.width} y2={SURFACE_Y} stroke="var(--border-strong)" strokeWidth="1.5" />

            {placed.flatMap(({ session, slot }) =>
              branchPaths(slot, session.id === selectedId ? selectedSteps : 0, box).map((branch, k) => (
                <path
                  key={`${session.id}-b${k}`}
                  d={branch.d}
                  fill="none"
                  stroke="var(--border-strong)"
                  strokeWidth="1.2"
                  strokeLinecap="round"
                  strokeDasharray="100"
                  pathLength={100}
                  opacity="0.7"
                  style={{ animation: "ck-dig 1.8s ease-out both" }}
                />
              )),
            )}

            {placed.map(({ session, path, edge }) => {
              const hot = isBusy(session.status) || needsYou(session);
              const digging = session.status === "starting";
              return (
                <g key={session.id}>
                  <path
                    d={path}
                    fill="none"
                    stroke={hot ? edge : "var(--border-strong)"}
                    strokeWidth={hot ? 2.5 : 1.5}
                    strokeLinecap="round"
                    strokeDasharray={digging ? "100" : hot ? "3 4" : "2 5"}
                    pathLength={100}
                    opacity={hot ? 0.55 : 0.4}
                    style={{
                      animation: digging
                        ? "ck-dig 2.4s ease-out both"
                        : isBusy(session.status)
                          ? "ck-flow 1.4s linear infinite"
                          : undefined,
                    }}
                  />
                  {/* A fat transparent copy so the tunnel itself is a target, not just the chamber. */}
                  <path
                    d={path}
                    fill="none"
                    stroke="transparent"
                    strokeWidth="18"
                    className="pointer-events-stroke cursor-pointer"
                    onClick={() => onSelect(session.id)}
                  />
                </g>
              );
            })}
          </svg>

          {/* Carriers: one ant per working colony, riding its own tunnel. */}
          {placed
            .filter(({ session }) => isBusy(session.status))
            .map(({ session, path, slot, index }) => {
              const facingLeft = slot.x < mothershipX;
              return (
                <div
                  key={`carry-${session.id}`}
                  className="absolute left-0 top-0 z-[3]"
                  style={{
                    offsetPath: `path("${path}")`,
                    offsetRotate: "0deg",
                    offsetDistance: "0%",
                    animation: `ck-carry ${7 + index * 1.3}s ease-in-out ${-index * 2.3}s infinite alternate`,
                  } as CSSProperties}
                >
                  <button
                    type="button"
                    onClick={() => onSelect(session.id)}
                    title={`${chamberLabel(session, 112)} · working`}
                    aria-label={`${chamberLabel(session, 112)}, working`}
                    className="block -translate-x-1/2 -translate-y-1/2 cursor-pointer"
                  >
                    <span className="block" style={{ transform: `scaleX(${facingLeft ? -1 : 1})` }}>
                      <AntAvatar state="working" size={26} phase={index} ground={false} framed={false} />
                    </span>
                  </button>
                </div>
              );
            })}

          {/* The surface: colonies that returned a pull request walk it home, queued ones wait by the entrance. */}
          {[
            ...returned.map((session, i) => ({ session, left: box.width * (0.12 + i * 0.1), i, state: "working" as AntState })),
            ...queued.map((session, i) => ({ session, left: box.width * (0.62 + i * 0.09), i, state: "thinking" as AntState })),
          ].map(({ session, left, i, state }) => (
            <div
              key={`walk-${session.id}`}
              className="absolute z-[3]"
              style={{
                left: Math.round(left),
                top: SURFACE_Y,
                animation: `ck-walk ${session.status === "queued" ? 30 : 9 + i * 2}s ease-in-out ${-i * 3}s infinite alternate`,
              }}
            >
              <button
                type="button"
                onClick={() => onSelect(session.id)}
                title={`${chamberLabel(session, 112)} · ${SESSION_STATUS[session.status]?.label ?? ""}`}
                aria-label={`${chamberLabel(session, 112)}, ${SESSION_STATUS[session.status]?.label ?? ""}`}
                className="block -translate-x-1/2 -translate-y-full cursor-pointer"
              >
                <AntAvatar state={state} size={30} phase={i} ground={false} framed={false} />
              </button>
            </div>
          ))}

          {/* The mothership, on the surface, where every tunnel starts. */}
          <button
            type="button"
            onClick={onSelectMothership}
            title="mothership"
            aria-label="mothership"
            className="absolute z-[2] flex -translate-x-1/2 translate-y-[-62%] cursor-pointer flex-col items-center gap-1 transition-transform hover:scale-105"
            style={{ left: mothershipX, top: SURFACE_Y }}
          >
            <span
              className="grid h-16 w-16 place-items-center rounded-full border border-accent bg-panel text-accent"
              style={{
                boxShadow: mothershipSelected
                  ? "0 0 0 2px var(--accent), 0 0 40px var(--accent-soft)"
                  : "0 0 34px var(--accent-soft)",
              }}
            >
              <svg width="30" height="30" viewBox="0 0 24 24" aria-hidden="true">
                <path d="M12 2.8 20 7.4v9.2L12 21.2 4 16.6V7.4z" fill="none" stroke="currentColor" strokeWidth="2" strokeLinejoin="round" />
                <circle cx="12" cy="12" r="2.8" fill="currentColor" />
              </svg>
            </span>
            <span className="font-mono text-[10px] tracking-[0.2em] text-faint">MOTHERSHIP</span>
          </button>

          {placed.map(({ session, slot, index, edge }) => {
            const selected = session.id === selectedId;
            const hot = isBusy(session.status) || needsYou(session);
            const diameter = slot.r * 2;
            const status = SESSION_STATUS[session.status];
            // Only the open colony has real settlers; every other chamber shows the colony itself.
            const ants = selected && settlers.length > 0 ? settlers.slice(0, 3) : null;
            return (
              <button
                key={session.id}
                type="button"
                onClick={() => onSelect(session.id)}
                onDoubleClick={() => onOpen(session.id)}
                title={`${session.repo}#${session.issue ?? ""} · ${session.issue_title || status?.label}`}
                aria-label={`${session.repo} ${session.issue != null ? `#${session.issue}` : ""}, ${status?.label ?? ""}`}
                className="absolute flex cursor-pointer flex-col items-center justify-center gap-[3px] overflow-hidden border-[1.5px] p-1.5 text-center transition-[transform,box-shadow] duration-300"
                style={{
                  left: slot.x - slot.r,
                  top: slot.y - slot.r,
                  width: diameter,
                  height: diameter,
                  borderRadius: BLOBS[index % BLOBS.length],
                  borderColor: edge,
                  background: "radial-gradient(120% 120% at 30% 20%, var(--panel-2), var(--plot))",
                  transform: selected ? "scale(1.06)" : undefined,
                  boxShadow: selected
                    ? `0 0 0 2px ${edge}, 0 0 46px color-mix(in oklab, ${edge} 40%, transparent)`
                    : hot
                      ? `0 0 34px color-mix(in oklab, ${edge} 28%, transparent), inset 0 -14px 26px rgb(0 0 0 / 0.18)`
                      : "inset 0 -14px 26px rgb(0 0 0 / 0.14)",
                  // No fill mode: an animation that held its last frame would keep owning `transform`
                  // and the selection scale would never get to transition.
                  animation: "ck-grow 0.7s cubic-bezier(.2,.9,.3,1.15)",
                }}
              >
                <span className="max-w-full truncate font-mono text-[11px] font-medium text-text">
                  {chamberLabel(session, diameter)}
                </span>
                <span className="font-mono text-[10px] tracking-[0.08em]" style={{ color: edge }}>
                  {status?.label ?? ""}
                </span>
                <span className="flex h-[22px] items-end gap-px">
                  {ants
                    ? ants.map((settler, k) => (
                        <AntAvatar
                          key={settler.agent.id}
                          state={settler.state}
                          role={settler.role}
                          size={28}
                          phase={k}
                          ground={false}
                          framed={false}
                        />
                      ))
                    : <AntAvatar state={antStateFor(session.status)} size={28} phase={index} ground={false} framed={false} />}
                </span>
              </button>
            );
          })}

          {freeSlot && (
            <button
              type="button"
              onClick={onLaunch}
              title="launch a colony here"
              aria-label="dig a new chamber"
              className="absolute flex h-28 w-28 cursor-pointer flex-col items-center justify-center gap-1 border-[1.5px] border-dashed border-border-strong text-faint transition-colors hover:border-accent hover:text-accent"
              style={{
                left: freeSlot.x - 56,
                top: freeSlot.y - 56,
                borderRadius: "48% 52% 45% 55% / 52% 46% 54% 48%",
              }}
            >
              <span aria-hidden="true" className="text-[22px] leading-none">
                +
              </span>
              <span className="font-mono text-[10px] tracking-[0.1em]">DIG</span>
            </button>
          )}
        </div>
      </div>

      {waiting.length > 0 && (
        <div className="relative flex flex-wrap gap-2.5 px-5 pb-4.5">
          {waiting.map((session) => (
            <button
              key={session.id}
              type="button"
              onClick={() => onOpen(session.id)}
              className="flex max-w-[520px] cursor-pointer items-center gap-3 rounded-xl border border-border bg-panel py-2.5 pl-3.5 pr-3 text-left transition-colors hover:border-warn"
            >
              <span
                aria-hidden="true"
                className="h-2 w-2 shrink-0 rounded-full bg-warn"
                style={{ animation: "ck-beacon 1.8s ease-out infinite" }}
              />
              <span className="shrink-0 font-mono text-[11.5px] text-muted">{chamberLabel(session, 112)}</span>
              <span className="min-w-0 truncate">{session.issue_title || "waiting on your answer"}</span>
              <span className="shrink-0 text-[12.5px] font-semibold text-accent">answer →</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
