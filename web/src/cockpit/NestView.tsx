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
import { Avatar } from "../components/Avatar";
import { SESSION_STATUS, type Tone, orgOf, sameOrg } from "../components/ui";
import { needsYou } from "../notifications";
import { isActive, isRaiding } from "../redTeam";
import type { SubagentView } from "../sessionStream";
import type { RedTeamRun, Session, SessionStatus } from "../types";
import { KIND_DOT } from "./InboxView";
import { feedEntry } from "./feed";
import { RedAnts } from "./RedAnts";
import {
  MAX_CHAMBERS,
  SURFACE_Y,
  branchPaths,
  normalizeBox,
  slotAt,
  surfaceGrass,
  tunnelPath,
  tunnelSeed,
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

/**
 * Per-chamber text balloons (issue #218), tier (a)+(b): every chamber reads its colony-level
 * `feedEntry(session).text`, and the selected chamber escalates to the live `agentDetail` from
 * the mothership's single event stream when it is non-empty. No per-chamber websocket fan-out.
 */
export interface BalloonAnchor {
  id: string;
  x: number;
  y: number;
  r: number;
  diameter: number;
  updatedAt: string;
  selected: boolean;
}

/**
 * Balloons only where the chamber has room for them: the same 112px threshold the chamber label
 * and avatar already use, so a balloon never hangs off a chamber too small to read.
 */
export const BALLOON_MIN_DIAMETER = 112;

/** Balloons whose anchors sit closer than this are the same scribble: keep one. */
const BALLOON_MIN_SEPARATION_PX = 120;

/**
 * Density rule, deterministic: drop chambers too small for a balloon, always keep the selected
 * chamber, then take the rest newest-first (`updated_at`, `id` breaking ties exactly like
 * `feedEntries`) and skip any whose anchor is within BALLOON_MIN_SEPARATION_PX of one already
 * shown. Pure so the tests can pin it without laying the nest out.
 */
export function planBalloons(anchors: BalloonAnchor[]): BalloonAnchor[] {
  const roomy = anchors.filter((a) => a.selected || a.diameter >= BALLOON_MIN_DIAMETER);
  const rest = roomy
    .filter((a) => !a.selected)
    .sort((a, b) => Date.parse(b.updatedAt) - Date.parse(a.updatedAt) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  const shown = roomy.filter((a) => a.selected);
  for (const anchor of rest) {
    if (shown.every((s) => Math.hypot(s.x - anchor.x, s.y - anchor.y) >= BALLOON_MIN_SEPARATION_PX)) shown.push(anchor);
  }
  return shown;
}

export function NestView({
  sessions,
  selectedId,
  mothershipSelected,
  redRuns = [],
  settlers,
  liveDetail = null,
  backlogCount,
  avatarFor,
  onSelect,
  onOpen,
  onSelectMothership,
  onLaunch,
}: {
  /** Already filtered to the chosen org and sorted; the view takes the first MAX_CHAMBERS. */
  sessions: Session[];
  selectedId: string | null;
  mothershipSelected: boolean;
  /** Red-team runs (issue #212): a live one targeting this nest's org marches ants over the plot. */
  redRuns?: RedTeamRun[];
  /** Real settlers, and only for `selectedId` — the harness streams one colony at a time. */
  settlers: SubagentView[];
  /**
   * The live `agentDetail` from the mothership's single event stream (tier (b)). It belongs to
   * the streamed colony, so only the selected chamber reads it — and only while non-empty, with
   * the colony-level feed line as the fallback.
   */
  liveDetail?: string | null;
  /** Open issues across the workspace's repositories; the frontier's badge. */
  backlogCount: number;
  /** The org's avatar, for the chamber's own badge; null when nothing knows one. */
  avatarFor: (org: string) => string | null;
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
    const seed = tunnelSeed(session.repo, session.issue);
    // Steps are only known for the colony whose stream is open, so only its branches are dug.
    const branches = branchPaths(slot, session.id === selectedId ? selectedSteps : 0, box, seed);
    return { session, slot, index, edge: TONE_VAR[tone], path: tunnelPath(slot, box, seed), branches };
  });

  // One text balloon per chamber: the colony-level feed line, except the selected chamber
  // escalates to the live stream detail while it has one. KIND_DOT keeps the dot reading the
  // same as the inbox and the timeline; the border reuses the chamber's own tone edge.
  const balloonEntries = placed.map(({ session, slot }) => {
    const tone = SESSION_STATUS[session.status]?.tone ?? "neutral";
    const entry = feedEntry(session);
    return {
      session,
      slot,
      diameter: slot.r * 2,
      edge: TONE_VAR[tone],
      dot: KIND_DOT[entry.kind],
      text: session.id === selectedId && liveDetail ? liveDetail : entry.text,
    };
  });
  const visibleBalloonIds = new Set(
    planBalloons(
      balloonEntries.map(({ session, slot, diameter }) => ({
        id: session.id,
        x: slot.x,
        y: slot.y,
        r: slot.r,
        diameter,
        updatedAt: session.updated_at,
        selected: session.id === selectedId,
      })),
    ).map((a) => a.id),
  );
  const balloons = balloonEntries.filter(({ session }) => visibleBalloonIds.has(session.id));
  // The raid a red-team run is staging on this nest: the first active one whose org is ours.
  // Its ants render only while the run is live — a done or stopped run has isActive() false,
  // so the column vanishes with it.
  const raid = redRuns.find((r) => isActive(r) && sessions.some((s) => sameOrg(orgOf(s), r.org || r.repo.split("/")[0])));

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

            {/* Side tunnels, dug the same way as the main corridor but narrower. */}
            {placed.flatMap(({ session, branches }) =>
              branches.map((branch, k) => (
                <g key={`${session.id}-b${k}`} style={{ animation: "ck-dig 1.8s ease-out both" }}>
                  <path d={branch.d} fill="none" stroke="var(--tunnel-wall)" strokeWidth="11" strokeLinecap="round" strokeLinejoin="round" />
                  <path d={branch.d} fill="none" stroke="var(--tunnel-floor)" strokeWidth="6" strokeLinecap="round" strokeLinejoin="round" />
                </g>
              )),
            )}

            {placed.map(({ session, path, edge }) => {
              const hot = isBusy(session.status) || needsYou(session);
              const busy = isBusy(session.status);
              const digging = session.status === "starting";
              return (
                <g key={session.id}>
                  {/* Three passes make a corridor rather than a line: the earth it is cut through,
                      the floor worn into it, and a thin line of traffic along the middle. */}
                  <g style={{ animation: digging ? "ck-dig 3s ease-out both" : undefined }}>
                    <path d={path} fill="none" stroke="var(--tunnel-wall)" strokeWidth="16" strokeLinecap="round" strokeLinejoin="round" />
                    <path d={path} fill="none" stroke="var(--tunnel-floor)" strokeWidth="9" strokeLinecap="round" strokeLinejoin="round" />
                  </g>
                  <path
                    d={path}
                    fill="none"
                    stroke={hot ? edge : "var(--border-strong)"}
                    strokeWidth="1.5"
                    strokeLinecap="round"
                    strokeDasharray={busy ? "3 6" : "2 7"}
                    opacity={hot ? 0.7 : 0.35}
                    style={{ animation: busy && !digging ? "ck-flow 1.6s linear infinite" : undefined }}
                  />
                  {/* A fat transparent copy so the tunnel itself is a target, not just the chamber. */}
                  <path
                    d={path}
                    fill="none"
                    stroke="transparent"
                    strokeWidth="22"
                    className="pointer-events-stroke cursor-pointer"
                    onClick={() => onSelect(session.id)}
                  />
                </g>
              );
            })}
          </svg>

          {/* Every working settler is out in a tunnel: the first hauls to the mothership and back,
              the rest work the side branches their own steps dug. A colony whose stream is not open
              has no settler list, so it sends the one ant that stands for the colony itself. */}
          {placed
            .filter(({ session }) => isBusy(session.status))
            .flatMap(({ session, path, slot, index, branches }) => {
              const facingLeft = slot.x < mothershipX;
              const mine = session.id === selectedId ? settlers : [];
              const active = mine.filter((a) => a.state === "working" || a.state === "thinking");
              const crew = active.length ? active : mine.slice(0, 1);
              // No real crew to show: one nameless ant for the colony.
              const riders: (SubagentView | null)[] = crew.length ? crew : [null];
              return riders.map((settler, k) => {
                const branch = k > 0 && branches.length ? branches[(k - 1) % branches.length] : null;
                const ride = branch ? branch.d : path;
                const seconds = branch ? 12 + ((index * 7 + k * 5) % 6) : 26 + index * 3 + k * 4;
                const doing = settler?.current?.name ?? settler?.last?.name ?? "working";
                const who = settler ? `${settler.name} · ${doing}` : `${chamberLabel(session, 112)} · working`;
                return (
                  <div
                    key={`carry-${session.id}-${settler?.agent.id ?? "solo"}`}
                    className="absolute left-0 top-0 z-[3]"
                    style={{
                      offsetPath: `path("${ride}")`,
                      offsetRotate: "0deg",
                      offsetDistance: "0%",
                      animation: `ck-carry ${seconds}s ease-in-out ${-((index * 5.3 + k * 7.1) % seconds)}s infinite`,
                    } as CSSProperties}
                  >
                    <button
                      type="button"
                      onClick={() => onSelect(session.id)}
                      title={who}
                      aria-label={who}
                      className="block -translate-x-1/2 -translate-y-1/2 cursor-pointer"
                    >
                      <span className="block" style={{ transform: `scaleX(${facingLeft ? -1 : 1})` }}>
                        <AntAvatar
                          state={settler?.state ?? "working"}
                          role={settler?.role}
                          size={26}
                          phase={index + k}
                          ground={false}
                          framed={false}
                        />
                      </span>
                    </button>
                  </div>
                );
              });
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
                {/* Below ~112px the chamber only has room for the label, the status and the ants. */}
                {diameter >= 112 && (
                  <Avatar name={session.repo.split("/")[0]} src={avatarFor(session.repo.split("/")[0])} size={22} rounded="md" />
                )}
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

          {/* Per-chamber text balloons: a sibling overlay above the carriers (z-[3]), never inside
              the chamber buttons, and pointer-events-none so chambers and tunnels stay clickable.
              One short line each — truncate clips it, the title keeps the full string — toned by
              the chamber's state. No animation of their own, so the `.cockpit`
              prefers-reduced-motion kill-switch has nothing to cover; aria-hidden because the
              chambers' title/aria-label already convey the text. */}
          <div aria-hidden="true" className="pointer-events-none absolute inset-0 z-[4]">
            {balloons.map(({ session, slot, diameter, edge, dot, text }) => (
              <span
                key={`balloon-${session.id}`}
                title={text}
                className="absolute -translate-x-1/2 -translate-y-full truncate rounded-full border bg-panel px-2 py-0.5 font-mono text-[10px] text-muted"
                style={{ left: slot.x, top: slot.y - slot.r - 6, maxWidth: diameter, borderColor: edge }}
              >
                <span
                  aria-hidden="true"
                  className="mr-1 inline-block h-[7px] w-[7px] rounded-full"
                  style={{ background: dot }}
                />
                {text}
              </span>
            ))}
          </div>

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

        {/* The raiders: red-team ants (issue #212) marching over soil, tunnels and chambers
            — beneath the mothership and the worker riders, and never swallowing a click. */}
        {raid && (
          <RedAnts
            mode={isRaiding(raid) ? "raiding" : "waiting"}
            count={raid.swarm_size}
            className="z-[1]"
          />
        )}
      </div>

      {waiting.length > 0 && (
        <div className="relative flex flex-wrap gap-2.5 px-5 pb-4.5">
          {waiting.map((session) => (
            <button
              key={session.id}
              type="button"
              // Selecting fills the inspector with the question; the pane then offers the way in.
              onClick={() => onSelect(session.id)}
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
