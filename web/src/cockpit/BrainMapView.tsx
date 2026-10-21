// The brain map: the owner's Living Brain as a 3D graph, with each running or queued colony drawn as an
// ant walking over it. The graph comes from the mothership's proxy (GET /api/brain/graph) and is
// structure only; the ants come from the sessions the cockpit already has, placed on the node their
// repository matched. Drawn on a 2D canvas with a perspective projection (as livingbrain.wiki draws
// its own brain: no WebGL, no library, nothing from a CDN).
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";

import { settingsPath } from "../components/settings/nav";
import { errorMessage, useApi } from "../context";
import type { BrainMapAnswer, BrainMapConnected } from "../features/brainMap/types";
import type { Session } from "../features/sessions/types";
import { SESSION_STATUS } from "../components/ui";
import { go } from "../router";
import {
  DEFAULT_CAMERA,
  adjacency,
  antPlan,
  antWorld,
  hashString,
  isAntStatus,
  kindColour,
  layoutBrain,
  nearest,
  nodeRadius,
  projectPoint,
  type Ant,
  type AntTone,
  type Camera,
} from "./brainMap/scene";

const REFRESH_MS = 60_000;
const TAU = Math.PI * 2;
const TONE_TOKEN: Record<AntTone, { token: string; fallback: string }> = {
  info: { token: "--info", fallback: "#6ea8ff" },
  accent: { token: "--accent", fallback: "#e0551f" },
  warn: { token: "--warn", fallback: "#d99a2b" },
  muted: { token: "--muted", fallback: "#9a9aa3" },
};

interface Scene {
  key: number;
  positions: Float32Array;
  neighbours: number[][];
  maxDegree: number;
}

interface Hit {
  x: number;
  y: number;
  r: number;
  ant?: Ant;
  node?: number;
}

function reducedMotion(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/** A key for a graph's shape: its kinds, degrees and links. A refresh of the same brain keeps its layout. */
function graphKey(answer: BrainMapConnected): number {
  const parts: string[] = [];
  for (const node of answer.nodes) parts.push(`${node.kind}:${node.degree}`);
  for (const [a, b] of answer.links) parts.push(`${a}-${b}`);
  return hashString(parts.join("|"));
}

const STATUS_LABEL = (status: Session["status"]) => SESSION_STATUS[status]?.label ?? status;

export function BrainMapView({ sessions, onOpenColony }: { sessions: readonly Session[]; onOpenColony: (id: string) => void }) {
  const api = useApi();
  const [answer, setAnswer] = useState<BrainMapAnswer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [structure, setStructure] = useState(false);
  const [paused, setPaused] = useState(false);
  const [hidden, setHidden] = useState<ReadonlySet<string>>(() => new Set());
  const [selected, setSelected] = useState<Ant | null>(null);
  const [tip, setTip] = useState<{ x: number; y: number; lines: string[] } | null>(null);
  const [retry, setRetry] = useState(0);

  const stageRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const sceneRef = useRef<Scene | null>(null);
  const answerRef = useRef<BrainMapConnected | null>(null);
  const antsRef = useRef<Ant[]>([]);
  const hitsRef = useRef<Hit[]>([]);
  const cameraRef = useRef<Camera>({ ...DEFAULT_CAMERA });
  const timeRef = useRef(0);
  const pausedRef = useRef(paused);
  const hiddenRef = useRef(hidden);
  const selectedRef = useRef<Ant | null>(null);
  const lastInteraction = useRef(0);
  const pointers = useRef(new Map<number, { x: number; y: number }>());
  const drag = useRef<{ x: number; y: number; moved: number; at: number; pinch: number; zoom: number } | null>(null);
  const colours = useRef<Record<AntTone, string> | null>(null);

  const repos = useMemo(() => {
    const set = new Set<string>();
    for (const s of sessions) if (isAntStatus(s.status)) set.add(s.repo);
    return [...set].sort();
  }, [sessions]);
  const reposKey = repos.join(",");

  // The brain's map, from the proxy: now, when the structure toggle or the colonies' repositories
  // change (both after a short settle), and once a minute after that. The proxy's cache does the rest.
  const wanted = useRef({ structure, repos });
  wanted.current = { structure, repos };
  const load = useCallback(async () => {
    try {
      const next = await api.brainMap({ structure: wanted.current.structure, repos: wanted.current.repos });
      setAnswer(next);
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api]);

  useEffect(() => {
    const settle = window.setTimeout(() => void load(), 250);
    return () => window.clearTimeout(settle);
  }, [load, structure, reposKey, retry]);

  useEffect(() => {
    const timer = window.setInterval(() => void load(), REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [load]);

  const connected = answer?.state === "connected" ? answer : null;
  const remote = connected?.remote ?? false;

  // The layout: recomputed only when the brain's shape changes, so a refresh does not reshuffle it.
  const scene = useMemo<Scene | null>(() => {
    if (!connected) return null;
    const key = graphKey(connected);
    const previous = sceneRef.current;
    if (previous && previous.key === key) return previous;
    let maxDegree = 0;
    for (const node of connected.nodes) maxDegree = Math.max(maxDegree, node.degree);
    const links = connected.links;
    return {
      key,
      positions: layoutBrain(connected.nodes.length, links),
      neighbours: adjacency(connected.nodes.length, links),
      maxDegree,
    };
    // `connected` is derived from `answer`; the key already decides whether anything changed.
  }, [answer]);

  const ants = useMemo(() => (connected ? antPlan(sessions, connected) : []), [sessions, connected]);

  // Everything the render loop reads lives in refs, so a frame never waits on React.
  useEffect(() => {
    sceneRef.current = scene;
    answerRef.current = connected;
  }, [scene, connected]);
  useEffect(() => {
    antsRef.current = ants;
    if (selectedRef.current && !ants.some((a) => a.id === selectedRef.current?.id)) {
      selectedRef.current = null;
      setSelected(null);
    }
  }, [ants]);
  useEffect(() => {
    pausedRef.current = paused;
  }, [paused]);
  useEffect(() => {
    hiddenRef.current = hidden;
  }, [hidden]);

  useEffect(() => {
    const style = getComputedStyle(document.documentElement);
    colours.current = Object.fromEntries(
      (Object.keys(TONE_TOKEN) as AntTone[]).map((tone) => [
        tone,
        style.getPropertyValue(TONE_TOKEN[tone].token).trim() || TONE_TOKEN[tone].fallback,
      ]),
    ) as Record<AntTone, string>;
  }, []);

  // The canvas follows its stage's size, at the screen's pixel density.
  useEffect(() => {
    const stage = stageRef.current;
    const canvas = canvasRef.current;
    if (!stage || !canvas) return;
    const fit = () => {
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const { width, height } = stage.getBoundingClientRect();
      canvas.width = Math.max(1, Math.round(width * dpr));
      canvas.height = Math.max(1, Math.round(height * dpr));
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
    };
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(stage);
    return () => observer.disconnect();
  }, [connected]);

  // The render loop. It stops drawing while the page is hidden.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const motionless = reducedMotion();
    let frame = 0;
    let last = performance.now();

    const draw = (now: number) => {
      const dt = Math.min(0.1, (now - last) / 1000);
      last = now;
      const dpr = canvas.width / Math.max(1, canvas.clientWidth);
      const width = canvas.clientWidth;
      const height = canvas.clientHeight;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, width, height);
      const cam = cameraRef.current;
      const current = answerRef.current;
      const s = sceneRef.current;
      const hits: Hit[] = [];
      if (!current || !s) {
        hitsRef.current = hits;
        return;
      }
      if (!motionless && !pausedRef.current) timeRef.current += dt;
      // Auto-rotate, slowly, until the person touches the map; then leave it be for a few seconds.
      if (!motionless && !pausedRef.current && now - lastInteraction.current > 4000 && !drag.current) {
        cam.yaw += dt * 0.05;
      }
      const hiddenKinds = hiddenRef.current;
      const shown = (index: number) => !hiddenKinds.has(current.nodes[index].kind);
      const pos = s.positions;
      const projected = new Float32Array(current.nodes.length * 4);
      for (let i = 0; i < current.nodes.length; i += 1) {
        const p = projectPoint(pos[i * 3], pos[i * 3 + 1], pos[i * 3 + 2], cam, width, height);
        projected[i * 4] = p.x;
        projected[i * 4 + 1] = p.y;
        projected[i * 4 + 2] = p.depth;
        projected[i * 4 + 3] = p.scale;
      }

      // Links, in three depth bins so the far ones sit behind.
      const bins: [number, number, number][][] = [[], [], []];
      for (const [a, b] of current.links) {
        if (!shown(a) || !shown(b)) continue;
        const depth = (projected[a * 4 + 2] + projected[b * 4 + 2]) / 2;
        bins[depth < -0.3 ? 0 : depth < 0.3 ? 1 : 2].push([a, b, depth]);
      }
      const alphas = [0.1, 0.17, 0.26];
      ctx.lineWidth = 1;
      bins.forEach((bin, k) => {
        if (!bin.length) return;
        ctx.strokeStyle = `oklch(0.78 0.03 270 / ${alphas[k]})`;
        ctx.beginPath();
        for (const [a, b] of bin) {
          ctx.moveTo(projected[a * 4], projected[a * 4 + 1]);
          ctx.lineTo(projected[b * 4], projected[b * 4 + 1]);
        }
        ctx.stroke();
      });

      // Nodes, far to near.
      const order = Array.from({ length: current.nodes.length }, (_, i) => i)
        .filter(shown)
        .sort((a, b) => projected[b * 4 + 2] - projected[a * 4 + 2]);
      for (const i of order) {
        const node = current.nodes[i];
        const x = projected[i * 4];
        const y = projected[i * 4 + 1];
        const scale = projected[i * 4 + 3];
        const r = nodeRadius(node.degree, s.maxDegree, scale);
        ctx.globalAlpha = Math.max(0.35, Math.min(1, 0.45 + 0.55 * scale));
        ctx.fillStyle = kindColour(node.kind);
        ctx.beginPath();
        ctx.arc(x, y, r, 0, TAU);
        ctx.fill();
        hits.push({ x, y, r, node: i });
      }
      ctx.globalAlpha = 1;

      // Ants, over the nodes. Each keeps its own walk; a still one sits dimmer.
      const world = new Float32Array(3);
      const colourOf = colours.current;
      for (const ant of antsRef.current) {
        const at = ant.node;
        if (at == null || at >= current.nodes.length) continue;
        const selectedAnt = selectedRef.current?.id === ant.id;
        antWorld(ant, motionless ? 0 : timeRef.current, pos, s.neighbours, world, 0);
        const p = projectPoint(world[0], world[1], world[2], cam, width, height);
        const r = Math.max(3, 4.4 * p.scale);
        const colour = colourOf?.[ant.tone] ?? TONE_TOKEN[ant.tone].fallback;
        const alpha = ant.moving ? 1 : ant.tone === "muted" ? 0.45 : 0.7;
        if (selectedAnt) {
          ctx.globalAlpha = 0.9;
          ctx.strokeStyle = colour;
          ctx.lineWidth = 1.5;
          ctx.beginPath();
          ctx.arc(p.x, p.y, r * 2.6, 0, TAU);
          ctx.stroke();
        }
        ctx.globalAlpha = alpha;
        ctx.fillStyle = colour;
        ctx.strokeStyle = colour;
        ctx.lineWidth = Math.max(0.7, r * 0.22);
        const wiggle = ant.moving && !motionless ? Math.sin(timeRef.current * 18 + ant.seed) * r * 0.35 : 0;
        for (const dx of [-0.5, 0, 0.6]) {
          ctx.beginPath();
          ctx.moveTo(p.x + dx * r * 1.6, p.y);
          ctx.lineTo(p.x + dx * r * 2.1, p.y + r * 1.4 + wiggle);
          ctx.lineTo(p.x + dx * r * 2.6, p.y + r * 1.9 - wiggle);
          ctx.moveTo(p.x + dx * r * 1.6, p.y);
          ctx.lineTo(p.x + dx * r * 2.1, p.y - r * 1.4 - wiggle);
          ctx.lineTo(p.x + dx * r * 2.6, p.y - r * 1.9 + wiggle);
          ctx.stroke();
        }
        ctx.beginPath();
        ctx.arc(p.x - r * 1.5, p.y, r * 0.95, 0, TAU);
        ctx.arc(p.x, p.y, r * 0.65, 0, TAU);
        ctx.arc(p.x + r * 1.1, p.y, r * 0.75, 0, TAU);
        ctx.fill();
        ctx.globalAlpha = 1;
        hits.push({ x: p.x, y: p.y, r: Math.max(r, 7), ant });
      }
      hitsRef.current = hits;
    };

    const tick = (now: number) => {
      if (document.visibilityState !== "hidden") draw(now);
      frame = window.requestAnimationFrame(tick);
    };
    frame = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(frame);
  }, [connected]);

  // Wheel zooms; the listener is not passive, so the page does not scroll under the map.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      lastInteraction.current = performance.now();
      cameraRef.current.zoom = Math.max(0.5, Math.min(3, cameraRef.current.zoom * Math.exp(-event.deltaY * 0.0012)));
    };
    canvas.addEventListener("wheel", onWheel, { passive: false });
    return () => canvas.removeEventListener("wheel", onWheel);
  }, [connected]);

  const pointAt = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  };

  const pick = (x: number, y: number, slop: number): Hit | null => {
    const hits = hitsRef.current;
    const ants = hits.filter((h) => h.ant);
    return nearest(ants, x, y, slop) ?? nearest(hits.filter((h) => h.node != null), x, y, slop / 2);
  };

  const onPointerDown = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    event.currentTarget.setPointerCapture(event.pointerId);
    const p = pointAt(event);
    pointers.current.set(event.pointerId, p);
    lastInteraction.current = performance.now();
    if (pointers.current.size === 1) {
      drag.current = { x: p.x, y: p.y, moved: 0, at: performance.now(), pinch: 0, zoom: cameraRef.current.zoom };
    } else if (pointers.current.size === 2) {
      const [a, b] = [...pointers.current.values()];
      if (drag.current) drag.current.pinch = Math.hypot(a.x - b.x, a.y - b.y);
      if (drag.current) drag.current.zoom = cameraRef.current.zoom;
    }
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    const p = pointAt(event);
    const known = pointers.current.get(event.pointerId);
    if (!known) {
      const hit = pick(p.x, p.y, 6);
      setTip(hit ? tipFor(hit, p) : null);
      return;
    }
    const prev = known;
    pointers.current.set(event.pointerId, p);
    lastInteraction.current = performance.now();
    if (pointers.current.size === 2 && drag.current?.pinch) {
      const [a, b] = [...pointers.current.values()];
      const dist = Math.hypot(a.x - b.x, a.y - b.y);
      cameraRef.current.zoom = Math.max(0.5, Math.min(3, drag.current.zoom * (dist / drag.current.pinch)));
      return;
    }
    if (drag.current && pointers.current.size === 1) {
      const dx = p.x - prev.x;
      const dy = p.y - prev.y;
      drag.current.moved += Math.abs(dx) + Math.abs(dy);
      cameraRef.current.yaw += dx * 0.008;
      cameraRef.current.pitch = Math.max(-1.2, Math.min(1.2, cameraRef.current.pitch + dy * 0.006));
      setTip(null);
    }
  };

  const onPointerUp = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    const p = pointAt(event);
    const wasOne = pointers.current.size === 1;
    pointers.current.delete(event.pointerId);
    if (wasOne && drag.current) {
      const tap = drag.current.moved < 8 && performance.now() - drag.current.at < 500;
      drag.current = null;
      if (tap) {
        const hit = pick(p.x, p.y, 12);
        if (hit?.ant) {
          selectedRef.current = hit.ant;
          setSelected(hit.ant);
        } else {
          selectedRef.current = null;
          setSelected(null);
        }
        setTip(null);
      }
    } else if (pointers.current.size === 0) {
      drag.current = null;
    }
  };

  const onPointerLeave = () => setTip(null);

  const onKeyDown = (event: ReactKeyboardEvent<HTMLCanvasElement>) => {
    const step = 0.08;
    const cam = cameraRef.current;
    lastInteraction.current = performance.now();
    if (event.key === "ArrowLeft") cam.yaw -= step;
    else if (event.key === "ArrowRight") cam.yaw += step;
    else if (event.key === "ArrowUp") cam.pitch = Math.max(-1.2, cam.pitch - step);
    else if (event.key === "ArrowDown") cam.pitch = Math.min(1.2, cam.pitch + step);
    else if (event.key === "+" || event.key === "=") cam.zoom = Math.min(3, cam.zoom * 1.15);
    else if (event.key === "-") cam.zoom = Math.max(0.5, cam.zoom / 1.15);
    else if (event.key === "Escape") {
      selectedRef.current = null;
      setSelected(null);
    } else return;
    event.preventDefault();
  };

  const tipFor = (hit: Hit, at: { x: number; y: number }) => {
    if (hit.ant) return { x: at.x, y: at.y, lines: [hit.ant.title, `${hit.ant.repo} · ${STATUS_LABEL(hit.ant.status)}`] };
    const node = connected?.nodes[hit.node ?? -1];
    if (!node) return null;
    const name = node.label ?? `Structure only · ${node.hash ?? "hidden"}`;
    return { x: at.x, y: at.y, lines: [name, `${node.kind} · ${node.degree} link${node.degree === 1 ? "" : "s"}`] };
  };

  const toggleKind = (kind: string) =>
    setHidden((prev) => {
      const next = new Set(prev);
      if (next.has(kind)) next.delete(kind);
      else next.add(kind);
      return next;
    });

  const kindCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const node of connected?.nodes ?? []) counts.set(node.kind, (counts.get(node.kind) ?? 0) + 1);
    return [...counts.entries()].sort((a, b) => b[1] - a[1]);
  }, [connected]);

  const openSettings = () => go(settingsPath("living-brain"));
  const stateLabel = connected
    ? `${connected.counts.nodes} nodes · ${connected.counts.links} links${connected.truncated ? " (the largest part of the brain)" : ""}`
    : "";

  return (
    <section aria-label="Brain map" className="flex min-h-0 flex-1 flex-col gap-3 px-4 py-3 text-text md:px-6">
      <header className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <h1 className="text-lg font-semibold">Brain</h1>
        {stateLabel && <span className="text-sm text-muted">{stateLabel}</span>}
        {connected && (
        <div className="ml-auto flex flex-wrap items-center gap-2">
          <button
            type="button"
            className="rounded-md border border-border px-2.5 py-1 text-sm disabled:opacity-60"
            aria-pressed={remote || structure}
            disabled={remote}
            title={remote ? "Remote access shows structure only" : "Show structure only: labels become opaque hashes"}
            onClick={() => setStructure((v) => !v)}
          >
            Structure only{remote ? " (remote)" : ""}
          </button>
          <button
            type="button"
            className="rounded-md border border-border px-2.5 py-1 text-sm"
            aria-pressed={paused}
            onClick={() => setPaused((v) => !v)}
          >
            {paused ? "Resume motion" : "Pause motion"}
          </button>
          <button
            type="button"
            className="rounded-md border border-border px-2.5 py-1 text-sm"
            onClick={() => {
              cameraRef.current = { ...DEFAULT_CAMERA };
            }}
          >
            Reset view
          </button>
        </div>
        )}
      </header>

      <div ref={stageRef} className="relative min-h-[360px] flex-1 overflow-hidden rounded-xl border border-border bg-bg">
        <canvas
          ref={canvasRef}
          role="img"
          aria-label={
            connected
              ? `3D brain map: ${connected.counts.nodes} nodes, ${ants.length} colonies drawn as ants. Drag to turn, scroll or pinch to zoom.`
              : "3D brain map"
          }
          tabIndex={connected ? 0 : -1}
          className="absolute inset-0 block touch-none select-none outline-none focus-visible:ring-2 focus-visible:ring-accent"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onPointerLeave={onPointerLeave}
          onKeyDown={onKeyDown}
        />

        {connected && kindCounts.length > 0 && (
          <div className="absolute left-2 right-2 top-2 flex gap-1.5 overflow-x-auto pb-1 [scrollbar-width:none]">
            {kindCounts.map(([kind, count]) => {
              const on = !hidden.has(kind);
              return (
                <button
                  key={kind}
                  type="button"
                  aria-pressed={on}
                  onClick={() => toggleKind(kind)}
                  className={`flex shrink-0 items-center gap-1.5 rounded-full border border-border bg-panel/80 px-2 py-0.5 text-xs backdrop-blur ${on ? "" : "opacity-45"}`}
                >
                  <span aria-hidden="true" className="inline-block size-2 rounded-full" style={{ background: kindColour(kind) }} />
                  <span>{kind}</span>
                  <span className="text-muted">{count}</span>
                </button>
              );
            })}
          </div>
        )}

        {tip && (
          <div
            role="tooltip"
            className="pointer-events-none absolute z-10 max-w-[16rem] rounded-md border border-border bg-panel px-2 py-1 text-xs shadow"
            style={{ left: tip.x + 12, top: tip.y + 12 }}
          >
            {tip.lines.map((line, i) => (
              <div key={i} className={i === 0 ? "font-medium" : "text-muted"}>
                {line}
              </div>
            ))}
          </div>
        )}

        {connected && connected.nodes.length > 0 && ants.length === 0 && (
          <p className="absolute bottom-2 left-2 rounded-md bg-panel/80 px-2 py-1 text-xs text-muted backdrop-blur">
            No colonies running right now. The brain is still here.
          </p>
        )}

        {selected && (
          <div className="absolute bottom-2 left-2 right-2 max-w-md rounded-lg border border-border bg-panel p-3 shadow md:right-auto md:w-96">
            <div className="flex items-start gap-2">
              <div className="min-w-0 flex-1">
                <div className="truncate font-medium">{selected.title}</div>
                <div className="truncate text-sm text-muted">
                  {selected.repo} · {STATUS_LABEL(selected.status)}
                </div>
              </div>
              <button
                type="button"
                aria-label="Close"
                className="rounded px-1.5 text-muted hover:text-text"
                onClick={() => {
                  selectedRef.current = null;
                  setSelected(null);
                }}
              >
                ×
              </button>
            </div>
            <button
              type="button"
              className="mt-2 rounded-md bg-accent px-3 py-1 text-sm text-on-accent"
              onClick={() => onOpenColony(selected.id)}
            >
              Open colony
            </button>
          </div>
        )}

        {!connected && (
          <EmptyState answer={answer} error={error} onConnect={openSettings} onRetry={() => setRetry((n) => n + 1)} loading={!answer && !error} />
        )}
        {connected && connected.nodes.length === 0 && (
          <EmptyState answer={answer} error={null} onConnect={openSettings} onRetry={() => {}} loading={false} />
        )}
      </div>
    </section>
  );
}

function EmptyState({
  answer,
  error,
  loading,
  onConnect,
  onRetry,
}: {
  answer: BrainMapAnswer | null;
  error: string | null;
  loading: boolean;
  onConnect: () => void;
  onRetry: () => void;
}) {
  let title = "";
  let body = "";
  let action: "connect" | "retry" | null = null;
  if (loading) {
    title = "Reading your brain…";
  } else if (error) {
    title = "The brain map could not be read";
    body = error;
    action = "retry";
  } else if (answer?.state === "disconnected") {
    title = "Living Brain is not connected";
    body = "Settings → Connections & secrets → Living Brain → Connect Living Brain. The brain's map appears here once it is connected.";
    action = "connect";
  } else if (answer?.state === "unauthorized") {
    title = "Living Brain refused the token";
    body = "Reconnect Living Brain in Settings to see the map again.";
    action = "connect";
  } else if (answer?.state === "connected" && answer.nodes.length === 0) {
    title = "The brain is empty";
    body = "Pages, decisions and sessions appear here as they are written.";
  }
  if (!title) return null;
  return (
    <div className="absolute inset-0 flex items-center justify-center p-6">
      <div className="max-w-sm rounded-xl border border-border bg-panel p-5 text-center shadow">
        <div className="font-medium">{title}</div>
        {body && <p className="mt-2 text-sm text-muted">{body}</p>}
        {action === "connect" && (
          <button type="button" className="mt-4 rounded-md bg-accent px-3 py-1.5 text-sm text-on-accent" onClick={onConnect}>
            Open Living Brain settings
          </button>
        )}
        {action === "retry" && (
          <button type="button" className="mt-4 rounded-md border border-border px-3 py-1.5 text-sm" onClick={onRetry}>
            Try again
          </button>
        )}
      </div>
    </div>
  );
}
