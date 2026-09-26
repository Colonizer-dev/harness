// The nest as a map of the software (issue: archify integration). A repository's architecture —
// drawn by a mapping colony with archify, stored by the mothership (maps.rs) — becomes the nest:
// components are chambers dug into the ground, boundaries are the mounds they sit in, connections
// are tunnels, and each live colony's ants walk the tunnels to the chambers whose files it is
// changing (GET /api/touched). Only real data: a repository with no map says so and offers to draw
// one; a colony with no changes yet waits at the mouth.
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent, type ReactElement, type ReactNode } from "react";

import { AntAvatar } from "../components/AntAvatar";
import { SESSION_STATUS, isLive, store, stored, timeAgo } from "../components/ui";
import { errorMessage, useApi, useToast } from "../context";
import type { RepoMap, Session } from "../types";
import { SURFACE_Y, surfaceGrass, type NestBox } from "./nest";
import { FileTreePane } from "./FileTreePane";
import { RepoCard, RepoPicker } from "./RepoPicker";
import { AntBubble } from "./AntBubble";
import { BUBBLE_TONE, MAX_ANT_BUBBLES } from "./bubbles";
import {
  LABEL_MIN_ZOOM,
  SUBLABEL_MIN_ZOOM,
  antRoute,
  clampView,
  componentForPath,
  componentsForFiles,
  entryComponent,
  fitView,
  layoutMap,
  mouthPath,
  tunnelPaths,
  zoomAt,
  type MapView,
} from "./nestMap";
import { taskLine } from "../summary";

const REPO_KEY = "colonizer.mapRepo";
const TOUCHED_POLL_MS = 5000;
const MAPPING_POLL_MS = 8000;
/** A mapping colony in one of these has finished; a map it left has been picked up by now. */
const ENDED: readonly Session["status"][] = ["no_changes", "pr_opened", "merged", "closed", "stopped", "failed"];

/** The repositories the map can show: the ones this nest's colonies work in, busiest first. */
export function mapRepos(sessions: readonly Session[]): string[] {
  const score = new Map<string, number>();
  for (const s of sessions) score.set(s.repo, (score.get(s.repo) ?? 0) + (isLive(s.status) ? 10 : 1));
  return [...score.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([repo]) => repo);
}

/** How a colony is in a chamber: changing files there, or only reading them so far. */
export type PlaceMode = "changing" | "reading";
export interface Place {
  session: Session;
  files: string[];
  mode: PlaceMode;
  /** Waiting on a person (a question) or idle: its ant stands still. */
  blocked: boolean;
}

/** A colony that is live but not working right now: its ants stand still in the chamber. */
export function isBlocked(s: Session): boolean {
  return s.status === "waiting_for_answer" || s.status === "idle";
}

/**
 * Which colonies are in which chamber: from the files each has changed, else — for a colony still
 * exploring — from the files its recent tool calls read. At most three chambers a colony; a colony
 * with neither waits at the surface.
 */
export function colonyPlaces(
  sessions: readonly Session[],
  touched: Readonly<Record<string, string[]>>,
  map: NonNullable<RepoMap["map"]>["map"],
  reading: Readonly<Record<string, string[]>> = {},
): { byChamber: Map<string, Place[]>; waiting: Session[] } {
  const byChamber = new Map<string, Place[]>();
  const waiting: Session[] = [];
  for (const s of sessions) {
    if (!isLive(s.status)) continue;
    let mode: PlaceMode = "changing";
    let hits = componentsForFiles(touched[s.id] ?? [], map.components).slice(0, 3);
    if (!hits.length) {
      mode = "reading";
      hits = componentsForFiles(reading[s.id] ?? [], map.components).slice(0, 3);
    }
    if (!hits.length) {
      waiting.push(s);
      continue;
    }
    for (const hit of hits)
      byChamber.set(hit.id, [...(byChamber.get(hit.id) ?? []), { session: s, files: hit.files, mode, blocked: isBlocked(s) }]);
  }
  return { byChamber, waiting };
}

export function NestMapView({
  sessions,
  selectedId,
  onSelect,
  onOpen,
  initialMap,
  initialTouched,
  initialReading,
}: {
  /** The nest's colonies (already scoped to the workspace). */
  sessions: Session[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  onOpen: (id: string) => void;
  /** Static markup never runs effects: the tests hand the map and the touched files in directly. */
  initialMap?: RepoMap | null;
  initialTouched?: Record<string, string[]>;
  initialReading?: Record<string, string[]>;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const repos = useMemo(() => mapRepos(sessions), [sessions]);
  const [chosen, setChosen] = useState<string | null>(() => stored(REPO_KEY));
  const repo = chosen && repos.includes(chosen) ? chosen : (initialMap?.repo ?? repos[0] ?? null);
  const [data, setData] = useState<RepoMap | null>(initialMap ?? null);
  const [touched, setTouched] = useState<Record<string, string[]>>(initialTouched ?? {});
  const [reading, setReading] = useState<Record<string, string[]>>(initialReading ?? {});
  const [open, setOpen] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [rawOpen, setRawOpen] = useState(false);
  const plotRef = useRef<HTMLDivElement | null>(null);
  // The viewport the map is shown in, as measured; the plot itself (layout.width × height) can be larger.
  const [viewport, setViewport] = useState<NestBox>({ width: 960, height: 560 });

  useLayoutEffect(() => {
    const el = plotRef.current;
    if (!el) return;
    const measure = () => {
      const next = { width: Math.max(1, el.clientWidth || 960), height: Math.max(1, el.clientHeight || 560) };
      setViewport((cur) => (cur.width === next.width && cur.height === next.height ? cur : next));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // The map for the chosen repository; re-read while a mapping colony is drawing it.
  const mappingStatus = data?.repo === repo ? (data.mapping?.status ?? null) : null;
  const mappingEnded = mappingStatus !== null && ENDED.includes(mappingStatus);
  // Publishing counts as drawing: the map is picked up as the colony's publish finishes.
  const drawing = mappingStatus !== null && !mappingEnded && !data?.map && mappingStatus !== "queued";
  const drawingQueued = mappingStatus === "queued" && !data?.map;
  useEffect(() => {
    if (!repo) return;
    let cancelled = false;
    const load = () =>
      api
        .repoMap(repo)
        .then((next) => !cancelled && setData(next))
        .catch(() => {
          /* the map stays as it was; the next poll tries again */
        });
    void load();
    const timer = drawing || drawingQueued ? setInterval(load, MAPPING_POLL_MS) : undefined;
    return () => {
      cancelled = true;
      if (timer) clearInterval(timer);
    };
  }, [api, repo, drawing, drawingQueued]);

  // Where the ants are: every live colony's changed files, polled while the map is on screen.
  useEffect(() => {
    let cancelled = false;
    const load = () =>
      api
        .touched()
        .then((t) => {
          if (cancelled) return;
          setTouched(t.sessions);
          setReading(t.reading ?? {});
        })
        .catch(() => {
          /* ants stay where they were */
        });
    void load();
    const timer = setInterval(load, TOUCHED_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [api]);

  const pickRepo = (next: string) => {
    setChosen(next);
    store(REPO_KEY, next);
    setOpen(null);
    setData(null);
  };

  const drawMap = async () => {
    if (!repo) return;
    setStarting(true);
    try {
      const next = await api.mapRepo(repo);
      setData(next);
      const colony = next.mapping?.id;
      toast({
        title: `Drawing ${repo}…`,
        body: "A colony is reading the code and drawing it with archify; the map lands here when it is done.",
        kind: "info",
        action: colony ? { label: "Watch it work", onClick: () => onSelect(colony) } : undefined,
      });
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setStarting(false);
    }
  };

  const stored_ = data && data.repo === repo ? data.map : null;
  const map = stored_?.map ?? null;
  const layout = useMemo(() => (map ? layoutMap(map, viewport) : null), [map, viewport]);
  // The plot: the map's own size when there is one, else just the viewport.
  const box: NestBox = layout ? { width: layout.width, height: layout.height } : viewport;
  // Pan and zoom: fitted until the viewer moves it, and fitted again whenever the layout changes.
  const [userView, setUserView] = useState<MapView | null>(null);
  useEffect(() => setUserView(null), [layout]);
  const fitted = fitView(box, viewport);
  const view = userView ? clampView(userView, box, viewport) : fitted;
  const zoomedIn = view.k > fitted.k + 0.001;
  // Text too small to read is left off rather than drawn as a smudge; hovering a chamber still names it.
  const labelsShown = view.k >= LABEL_MIN_ZOOM;
  const sublabelsShown = view.k >= SUBLABEL_MIN_ZOOM;
  const viewRef = useRef({ view, box, viewport });
  viewRef.current = { view, box, viewport };
  const zoomBy = (factor: number, px = viewport.width / 2, py = viewport.height / 2) =>
    setUserView(zoomAt(viewRef.current.view, factor, px, py, viewRef.current.box, viewRef.current.viewport));
  // Pinch (and ctrl/⌘ + wheel) zooms about the pointer; a sideways swipe pans a plot wider than the
  // viewport. A plain vertical wheel is left to scroll the page.
  useEffect(() => {
    const el = plotRef.current;
    if (!el || !layout) return;
    const onWheel = (e: WheelEvent) => {
      const { view: v, box: b, viewport: vp } = viewRef.current;
      const rect = el.getBoundingClientRect();
      if (e.ctrlKey || e.metaKey) {
        e.preventDefault();
        setUserView(zoomAt(v, Math.exp(-e.deltaY * 0.01), e.clientX - rect.left, e.clientY - rect.top, b, vp));
      } else if (Math.abs(e.deltaX) > Math.abs(e.deltaY) && b.width * v.k > vp.width + 1) {
        e.preventDefault();
        setUserView(clampView({ ...v, x: v.x - e.deltaX }, b, vp));
      }
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [layout]);
  // Drag to pan, two fingers to pinch — started only off the chambers, ants and panels.
  const pointers = useRef(new Map<number, { x: number; y: number }>());
  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!layout || (e.target as HTMLElement).closest("button, a, [role=dialog], input")) return;
    pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
    e.currentTarget.setPointerCapture?.(e.pointerId);
  };
  const onPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const pts = pointers.current;
    const prev = pts.get(e.pointerId);
    if (!prev) return;
    const { view: v, box: b, viewport: vp } = viewRef.current;
    if (pts.size === 1) {
      pts.set(e.pointerId, { x: e.clientX, y: e.clientY });
      setUserView(clampView({ ...v, x: v.x + e.clientX - prev.x, y: v.y + e.clientY - prev.y }, b, vp));
      return;
    }
    const other = pts.get([...pts.keys()].find((id) => id !== e.pointerId)!)!;
    const before = Math.hypot(prev.x - other.x, prev.y - other.y);
    pts.set(e.pointerId, { x: e.clientX, y: e.clientY });
    const after = Math.hypot(e.clientX - other.x, e.clientY - other.y);
    if (before < 1) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const mx = (e.clientX + other.x) / 2 - rect.left;
    const my = (e.clientY + other.y) / 2 - rect.top;
    const zoomed = zoomAt(v, after / before, mx, my, b, vp);
    setUserView(clampView({ ...zoomed, x: zoomed.x + (e.clientX - prev.x) / 2, y: zoomed.y + (e.clientY - prev.y) / 2 }, b, vp));
  };
  const onPointerUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    pointers.current.delete(e.pointerId);
  };
  const tunnels = useMemo(() => (map && layout ? tunnelPaths(map, layout) : []), [map, layout]);
  const entry = map && layout ? entryComponent(map, layout) : null;
  const inRepo = sessions.filter((s) => s.repo === repo);
  // The explorer pane's files: fetched once per repository, when a component is first opened.
  const [files, setFiles] = useState<{ repo: string; revision: string | null; paths: string[] | null; error: string | null } | null>(null);
  // Keyed on the repository and whether a component is open, never on `files` itself: setting the
  // loading state must not re-run (and so cancel) the very request that fills it.
  const filesFor = useRef<string | null>(null);
  const anyOpen = open != null;
  useEffect(() => {
    if (!anyOpen || !repo || filesFor.current === repo) return;
    filesFor.current = repo;
    let cancelled = false;
    setFiles({ repo, revision: null, paths: null, error: null });
    api
      .repoMapFiles(repo)
      .then((r) => !cancelled && setFiles({ repo, revision: r.revision, paths: r.paths, error: null }))
      .catch((e) => {
        if (cancelled) return;
        filesFor.current = null;
        setFiles({ repo, revision: null, paths: null, error: errorMessage(e) });
      });
    return () => {
      cancelled = true;
      // A request cut off by a repository switch is asked again when that repository comes back.
      if (filesFor.current === repo) filesFor.current = null;
    };
  }, [anyOpen, repo, api]);
  const openComponent = open && map ? map.components.find((c) => c.id === open) : undefined;
  const markedPaths = useMemo(() => new Set((openComponent?.sources ?? []).map((s) => s.path.replace(/\/+$/, ""))), [openComponent]);
  const changingPaths = useMemo(() => new Set(inRepo.flatMap((s) => touched[s.id] ?? [])), [inRepo, touched]);
  const readingPaths = useMemo(() => new Set(inRepo.flatMap((s) => reading[s.id] ?? [])), [inRepo, reading]);
  const places: ReturnType<typeof colonyPlaces> = map ? colonyPlaces(inRepo, touched, map, reading) : { byChamber: new Map(), waiting: [] };
  const openChamber = open && layout ? layout.byId.get(open) : null;

  return (
    <div className="relative flex min-h-0 flex-col">
      {/* The map's own bar: which repository, what drew it, and the way to draw it again. */}
      <div className="relative z-[5] flex flex-wrap items-center gap-x-4 gap-y-2 px-6 pb-2 pt-3 text-[13px]">
        {repos.length > 0 && <RepoPicker repos={repos} value={repo} onChange={pickRepo} />}
        {stored_ && (
          <span className="text-faint" title={new Date(stored_.generated_at).toLocaleString()}>
            {map?.subtitle ? `${map.subtitle} · ` : ""}drawn {formatWhen(stored_.generated_at)} ({timeAgo(stored_.generated_at)})
            {stored_.revision ? ` at ${stored_.revision.slice(0, 7)}` : ""}
            {stored_.session && (
              <>
                {" · by "}
                <button type="button" onClick={() => onOpen(stored_.session)} className="cursor-pointer border-0 bg-transparent p-0 font-mono text-faint underline decoration-dotted underline-offset-2 hover:text-text">
                  colony {stored_.session.slice(0, 8)}
                </button>
              </>
            )}
          </span>
        )}
        <div className="flex-1" />
        {stored_ && (
          <div className="flex shrink-0 items-center gap-2">
            <button type="button" onClick={() => setRawOpen(true)} className={MAP_BUTTON}>
              <span aria-hidden="true" className="font-mono text-[11px]">{"{ }"}</span>
              Raw JSON
            </button>
            {!drawing && !drawingQueued && (
              <button type="button" disabled={starting} onClick={() => void drawMap()} className={MAP_BUTTON}>
                Redraw map
              </button>
            )}
          </div>
        )}
      </div>
      {repo && (
        <div className="relative z-[4] px-6 pb-2">
          <RepoCard repo={repo} />
        </div>
      )}
      {rawOpen && stored_ && <RawMapDialog value={stored_} onClose={() => setRawOpen(false)} />}

      <div className="relative flex min-h-0 flex-1">
      <div
        ref={plotRef}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        className="map-plot relative h-[clamp(420px,72vh,1000px)] min-w-0 flex-1 overflow-hidden"
        style={{ touchAction: zoomedIn ? "none" : "pan-y", cursor: layout ? "grab" : undefined }}
      >
        {/* The plot, panned and zoomed: everything on it moves together, ants and tunnels included. */}
        <div
          className="map-world absolute left-0 top-0"
          style={{ width: box.width, height: box.height, transform: `translate(${view.x}px, ${view.y}px) scale(${view.k})`, transformOrigin: "0 0" }}
        >
        {/* Sky and soil, as in the nest. */}
        <div
          aria-hidden="true"
          className="absolute inset-x-0 bottom-0"
          style={{ top: SURFACE_Y, background: "linear-gradient(color-mix(in oklab, var(--panel-2) 70%, transparent), transparent 70%)" }}
        />
        {surfaceGrass(box).map((tuft, i) => (
          <span
            key={i}
            aria-hidden="true"
            className="absolute w-0.5 rounded-t-sm bg-ok opacity-40"
            style={{ left: tuft.left, top: tuft.top, height: tuft.height, transform: `rotate(${tuft.rotation}deg)`, transformOrigin: "50% 100%" }}
          />
        ))}
        {/* The mothership's mouth: where every colony's ants come down. */}
        <span
          aria-hidden="true"
          className="map-mouth absolute grid h-11 w-11 -translate-x-1/2 -translate-y-1/2 place-items-center rounded-full text-accent"
          style={{ left: layout?.mouth.x ?? box.width / 2, top: SURFACE_Y }}
        >
          <svg width="20" height="20" viewBox="0 0 24 24">
            <path d="M12 2.8 20 7.4v9.2L12 21.2 4 16.6V7.4z" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round" />
            <circle cx="12" cy="12" r="2.6" fill="currentColor" />
          </svg>
        </span>

        {map && layout && (
            <>
              {/* Mounds: the boundaries archify drew, behind their chambers, titled where no other text is.
                  A title sits above the tunnels (z-[1]), so a tunnel passing it never covers it. */}
              {layout.mounds.map((m, i) => (
                <div key={i} aria-hidden="true" className="map-mound absolute" style={{ left: m.box.x, top: m.box.y, width: m.box.w, height: m.box.h }}>
                  {m.title && labelsShown && (
                    <span
                      title={m.label}
                      className="map-text absolute z-[1] truncate whitespace-nowrap text-[10.5px] uppercase tracking-[0.1em] text-faint"
                      style={{ left: m.title.x - m.box.x, top: m.title.y - m.box.y, width: m.title.w, textAlign: "center" }}
                    >
                      {m.label}
                    </span>
                  )}
                </div>
              ))}

              <svg viewBox={`0 0 ${box.width} ${box.height}`} className="pointer-events-none absolute inset-0 h-full w-full" aria-hidden="true">
                <line x1="0" y1={SURFACE_Y} x2={box.width} y2={SURFACE_Y} stroke="var(--border-strong)" strokeWidth="1.5" />
                {entry && <MapTunnel d={mouthPath(layout, entry)} hot={places.byChamber.size > 0} />}
                {tunnels.map((t) => (
                  <MapTunnel key={t.key} d={t.d} hot={places.byChamber.has(t.from) || places.byChamber.has(t.to)} />
                ))}
              </svg>

              {/* Chambers: holes dug where archify put the components, each named where no other name is. */}
              {layout.chambers.map((c) => {
                const inside = places.byChamber.get(c.id) ?? [];
                const selected = inside.some((p) => p.session.id === selectedId);
                const full = c.component.sublabel ? `${c.component.label} — ${c.component.sublabel}` : c.component.label;
                const toggle = () => setOpen((cur) => (cur === c.id ? null : c.id));
                return (
                  <div key={c.id}>
                    <div className="absolute z-[2]" style={{ left: c.x, top: c.y }}>
                      <button
                        type="button"
                        onClick={toggle}
                        title={full}
                        aria-label={`${c.component.label}${inside.length ? ` · ${inside.length} ${inside.length === 1 ? "colony" : "colonies"} inside` : ""}`}
                        data-active={inside.length > 0}
                        data-selected={selected || open === c.id}
                        data-external={c.component.type === "external"}
                        className="map-hole absolute -translate-x-1/2 -translate-y-1/2 cursor-pointer rounded-full"
                        style={{ width: c.r * 2, height: c.r * 2 }}
                      >
                        {inside.length > 0 && (
                          <span className="absolute -right-1 -top-1 grid h-5 min-w-5 place-items-center rounded-full bg-accent px-1 font-mono text-[10.5px] font-semibold text-on-accent tabular-nums">
                            {inside.length}
                          </span>
                        )}
                        {/* One ant idles in the chamber for the colonies working there. */}
                        {inside.length > 0 && (
                          <span className="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2">
                            <AntAvatar
                              state={inside.every((p) => p.blocked) ? "thinking" : "working"}
                              size={Math.min(26, c.r)}
                              framed={false}
                              ground={false}
                              phase={c.x % 5}
                            />
                          </span>
                        )}
                      </button>
                    </div>
                    {labelsShown && (
                      <span
                        aria-hidden="true"
                        title={full}
                        onClick={toggle}
                        className="map-text absolute z-[2] cursor-pointer"
                        style={{ left: c.label.x, top: c.label.y, width: c.label.w, textAlign: layout.labelSide === "below" ? "center" : "left" }}
                      >
                        <span className="block truncate text-[12px] font-medium leading-[17px] text-text">{c.component.label}</span>
                        {c.component.sublabel && sublabelsShown && !c.compact && (
                          <span className="block truncate text-[10.5px] leading-[15px] text-faint">{c.component.sublabel}</span>
                        )}
                      </span>
                    )}
                  </div>
                );
              })}

              {/* The ants: each colony walks from the mouth, along the tunnels, to the chambers its files are in. */}
              {[...places.byChamber.entries()].flatMap(([id, list], chamberIndex) =>
                list.map(({ session, mode, blocked, files: hereFiles }, k) => {
                  const route = antRoute(map, layout, id);
                  // Readers scout faster and further apart than the ants carrying changes.
                  // A slow walk: a round trip takes the better part of a minute, readers a little quicker.
                  const seconds = (mode === "reading" ? 34 : 46) + ((k * 7 + id.length * 3) % 14);
                  const doing = blocked ? (session.status === "waiting_for_answer" ? "waiting for you" : "idle") : mode === "reading" ? "reading here" : "changing files here";
                  return (
                    <div
                      key={`${session.id}-${id}`}
                      className="map-ant absolute left-0 top-0 z-[3]"
                      data-mode={mode}
                      data-blocked={blocked}
                      style={{
                        offsetPath: `path("${route}")`,
                        offsetRotate: "0deg",
                        offsetDistance: "100%",
                        animation: blocked ? undefined : `ck-carry ${seconds}s ease-in-out ${-((k * 4.3 + id.length) % seconds)}s infinite`,
                      } as CSSProperties}
                    >
                      <button
                        type="button"
                        onClick={() => onSelect(session.id)}
                        title={`${session.repo}${session.issue != null ? ` #${session.issue}` : ""} · ${taskLine(session, "open session")} · ${doing}`}
                        aria-label={`colony ${session.issue_title || session.id} ${doing} in ${id}`}
                        className={`relative block -translate-x-1/2 -translate-y-1/2 cursor-pointer ${mode === "reading" ? "opacity-70" : ""}`}
                      >
                        <AntAvatar state={blocked ? "thinking" : "working"} size={22} phase={k} ground={false} framed={false} />
                        {mode === "reading" && !blocked && (
                          <span aria-hidden="true" className="absolute -inset-1 rounded-full border border-dashed border-accent/60" />
                        )}
                        {blocked && (
                          <span aria-hidden="true" className="absolute -right-2 -top-2 grid size-4 place-items-center rounded-full bg-warn text-[10px] font-bold text-bg">
                            {session.status === "waiting_for_answer" ? "?" : "‖"}
                          </span>
                        )}
                      </button>
                      {/* What the first ant in a chamber is doing, as in the nest: at most four bubbles. */}
                      {k === 0 && chamberIndex < MAX_ANT_BUBBLES && (
                        <span className="pointer-events-none absolute bottom-full left-1/2 mb-3 -translate-x-1/2">
                          <AntBubble
                            text={mapBubble(mode, blocked, session.status, hereFiles)}
                            title={`${taskLine(session, session.id)} · ${doing}`}
                            tone={blocked ? "var(--warn)" : mode === "reading" ? BUBBLE_TONE.thinking : BUBBLE_TONE.working}
                          />
                        </span>
                      )}
                    </div>
                  );
                }),
              )}

              {/* Colonies with nothing changed yet wait by the mouth, on the side with more surface. */}
              {places.waiting.map((s, i) => (
                <button
                  key={`wait-${s.id}`}
                  type="button"
                  onClick={() => onSelect(s.id)}
                  title={`${taskLine(s, s.id)} · ${isBlocked(s) ? (s.status === "waiting_for_answer" ? "waiting for you" : "idle") : "starting — nothing read or changed yet"}`}
                  aria-label={`colony ${s.issue_title || s.id}, nothing read or changed yet`}
                  className="absolute z-[3] -translate-x-1/2 -translate-y-full cursor-pointer"
                  style={{ left: layout.mouth.x + (layout.mouth.x > box.width / 2 ? -1 : 1) * (34 + i * 22), top: SURFACE_Y }}
                >
                  <AntAvatar state="thinking" size={20} phase={i} ground={false} framed={false} />
                </button>
              ))}
            </>
        )}
        </div>

        {!repo ? (
          <MapNote title="No colonies in this workspace yet">A map is drawn per repository; launch a colony and its repository shows up here.</MapNote>
        ) : !map ? (
          drawing || drawingQueued ? (
            <MapNote title={`Drawing ${repo}…`}>
              A colony is reading the code and drawing it with archify{drawingQueued ? " (queued for a free slot)" : ""}.{" "}
              {data?.mapping && (
                <button type="button" onClick={() => onOpen(data.mapping!.id)} className="cursor-pointer border-0 bg-transparent p-0 text-accent underline underline-offset-2">
                  Watch it work
                </button>
              )}
            </MapNote>
          ) : (
            <MapNote title={`No map for ${repo} yet`}>
              {mappingEnded
                ? `The last mapping colony ended (${SESSION_STATUS[mappingStatus!]?.label.toLowerCase() ?? mappingStatus}) without a map it could keep. Its log says why; you can try again.`
                : "A colony reads the repository and draws its architecture — every component tied to the files it lives in — so the ants can walk it."}
              <div className="mt-4">
                <button
                  type="button"
                  disabled={starting || !data}
                  onClick={() => void drawMap()}
                  className="cursor-pointer rounded-md border-0 bg-text px-3.5 py-2 text-[13px] font-medium text-bg transition-opacity hover:opacity-85 disabled:opacity-50"
                >
                  {starting ? "Starting…" : "Map this repo"}
                </button>
              </div>
            </MapNote>
          )
        ) : null}

        {openChamber && (
          <ChamberPanel
            chamber={{ x: openChamber.x * view.k + view.x, y: openChamber.y * view.k + view.y, r: openChamber.r * view.k, component: openChamber.component }}
            box={viewport}
            inside={places.byChamber.get(openChamber.id) ?? []}
            onClose={() => setOpen(null)}
            onSelect={onSelect}
            onOpen={onOpen}
          />
        )}

        {/* Zoom: in, out, and back to the whole map. Pinch or ctrl/⌘ + scroll zooms too; drag to pan. */}
        {map && layout && (
          <div className="absolute bottom-3 right-3 z-[5] flex items-center gap-1" role="group" aria-label="map zoom">
            <button type="button" aria-label="zoom out" onClick={() => zoomBy(1 / 1.25)} className={ZOOM_BUTTON}>
              −
            </button>
            <button type="button" aria-label="zoom in" onClick={() => zoomBy(1.25)} className={ZOOM_BUTTON}>
              +
            </button>
            <button type="button" onClick={() => setUserView(null)} disabled={!userView} className={`${ZOOM_BUTTON} w-auto px-2.5 text-[12px]`}>
              Fit
            </button>
          </div>
        )}
      </div>
      {openChamber && repo && (
        // Beside the map on a wide screen; over it, full width, on a phone.
        <div className="flex max-md:absolute max-md:inset-0 max-md:z-[8] max-md:[&>aside]:w-full">
        <FileTreePane
          repo={repo}
          revision={files?.repo === repo ? files.revision : (stored_?.revision ?? null)}
          paths={files?.repo === repo ? files.paths : null}
          error={files?.repo === repo ? files.error : null}
          title={openChamber.component.label}
          marked={markedPaths}
          changing={changingPaths}
          reading={readingPaths}
          componentOf={(path) => {
            const id = map ? componentForPath(path, map.components) : null;
            return id ? (map?.components.find((c) => c.id === id)?.label ?? null) : null;
          }}
          onOpenColony={onOpen}
          onClose={() => setOpen(null)}
        />
        </div>
      )}
      </div>
    </div>
  );
}

function MapTunnel({ d, hot }: { d: string; hot: boolean }): ReactElement {
  return (
    <g>
      <path d={d} fill="none" stroke="var(--tunnel-wall)" strokeWidth="14" strokeLinecap="round" strokeLinejoin="round" />
      <path d={d} fill="none" stroke="var(--tunnel-floor)" strokeWidth="8" strokeLinecap="round" strokeLinejoin="round" />
      <path
        d={d}
        fill="none"
        stroke={hot ? "var(--accent)" : "var(--border-strong)"}
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeDasharray={hot ? "3 6" : "2 7"}
        opacity={hot ? 0.7 : 0.35}
        style={{ animation: hot ? "ck-flow 1.6s linear infinite" : undefined }}
      />
    </g>
  );
}

function MapNote({ title, children }: { title: string; children: ReactNode }): ReactElement {
  return (
    <div className="absolute inset-x-0 z-[4] flex justify-center px-6" style={{ top: SURFACE_Y + 70 }}>
      <div className="v3-pop max-w-[460px] rounded-xl border border-border-strong px-5 py-4 text-[13.5px] leading-relaxed text-muted shadow-[0_12px_40px_rgb(0_0_0/0.3)]">
        <div className="mb-1 text-[15px] font-semibold text-text">{title}</div>
        {children}
      </div>
    </div>
  );
}

/** What a chamber holds: the component, the files it lives in, and the colonies working in it. */
function ChamberPanel({
  chamber,
  box,
  inside,
  onClose,
  onSelect,
  onOpen,
}: {
  chamber: { x: number; y: number; r: number; component: RepoMapComponent };
  box: NestBox;
  inside: Place[];
  onClose: () => void;
  onSelect: (id: string) => void;
  onOpen: (id: string) => void;
}): ReactElement {
  const width = 300;
  // Beside the chamber, on whichever side has room, and never past the plot's edges.
  const right = chamber.x + chamber.r + 14;
  const left = right + width + 12 <= box.width ? right : Math.max(12, chamber.x - chamber.r - 14 - width);
  const top = Math.max(SURFACE_Y + 8, Math.min(box.height - 280, chamber.y - 40));
  const maxHeight = Math.max(160, box.height - top - 12);
  const hitFiles = new Set(inside.filter((p) => p.mode === "changing").flatMap((p) => p.files));
  const readFiles = new Set(inside.filter((p) => p.mode === "reading").flatMap((p) => p.files));
  const c = chamber.component;
  return (
    <div
      role="dialog"
      aria-label={c.label}
      className="v3-pop scroll-thin absolute z-[6] animate-[ck-in_160ms_ease-out_both] overflow-y-auto rounded-xl border border-border-strong p-3.5 text-[13px] shadow-[0_16px_48px_rgb(0_0_0/0.4)]"
      style={{ left, top, width, maxHeight }}
    >
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <div className="truncate text-[14px] font-semibold text-text">{c.label}</div>
          <div className="truncate text-[12px] text-faint">
            {c.type}
            {c.sublabel ? ` · ${c.sublabel}` : ""}
          </div>
        </div>
        <button type="button" aria-label="close" onClick={onClose} className="grid h-6 w-6 cursor-pointer place-items-center rounded-md border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
          ×
        </button>
      </div>
      {c.sources.length > 0 && (
        <div className="mt-3">
          <div className="mb-1 text-[11.5px] text-faint">Lives in</div>
          <ul className="m-0 list-none space-y-0.5 p-0">
            {c.sources.slice(0, 6).map((s) => (
              <li key={s.path} className="truncate font-mono text-[11.5px] text-muted" title={s.path}>
                {s.path}
                {s.line ? `:${s.line}` : ""}
              </li>
            ))}
          </ul>
        </div>
      )}
      {hitFiles.size > 0 && (
        <div className="mt-3">
          <div className="mb-1 text-[11.5px] text-faint">Being changed</div>
          <ul className="m-0 list-none space-y-0.5 p-0">
            {[...hitFiles].slice(0, 6).map((f) => (
              <li key={f} className="truncate font-mono text-[11.5px] text-accent" title={f}>
                {f}
              </li>
            ))}
          </ul>
        </div>
      )}
      {readFiles.size > 0 && (
        <div className="mt-3">
          <div className="mb-1 text-[11.5px] text-faint">Reading</div>
          <ul className="m-0 list-none space-y-0.5 p-0">
            {[...readFiles].slice(0, 6).map((f) => (
              <li key={f} className="truncate font-mono text-[11.5px] text-muted" title={f}>
                {f}
              </li>
            ))}
          </ul>
        </div>
      )}
      <div className="mt-3 border-t border-border pt-2">
        {inside.length === 0 ? (
          <div className="text-[12.5px] text-faint">No colony is working here.</div>
        ) : (
          inside.map(({ session }) => (
            <div key={session.id} className="flex items-center gap-2 py-1">
              <button type="button" onClick={() => onSelect(session.id)} className="min-w-0 flex-1 cursor-pointer truncate border-0 bg-transparent p-0 text-left text-[13px] text-text hover:underline">
                {taskLine(session, "open session")}
                <span className="ml-1.5 font-mono text-[11px] text-faint">{session.issue != null ? `#${session.issue}` : ""}</span>
              </button>
              <span className="text-[11.5px] text-faint">{SESSION_STATUS[session.status]?.label}</span>
              <button type="button" onClick={() => onOpen(session.id)} className="cursor-pointer rounded-md border-0 bg-text px-2 py-0.5 text-[11.5px] font-medium text-bg hover:opacity-85">
                open
              </button>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

type RepoMapComponent = NonNullable<RepoMap["map"]>["map"]["components"][number];

const ZOOM_BUTTON =
  "grid h-8 w-8 cursor-pointer place-items-center rounded-lg border border-border bg-panel text-[15px] text-muted shadow-[0_4px_14px_rgb(0_0_0/0.25)] transition-colors hover:border-border-strong hover:text-text disabled:cursor-default disabled:opacity-50";

const MAP_BUTTON =
  "inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-lg border border-border bg-panel px-3 text-[12.5px] text-muted transition-colors hover:border-border-strong hover:text-text disabled:cursor-not-allowed disabled:opacity-50";

/** When a map was drawn, as a local date and time ("24 Sep, 15:58"). */
function formatWhen(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString(undefined, { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
}

/** The stored map as the mothership keeps it, pretty-printed, with copy and download. */
function RawMapDialog({ value, onClose }: { value: NonNullable<RepoMap["map"]>; onClose: () => void }): ReactElement {
  const ref = useRef<HTMLDialogElement>(null);
  const toast = useToast();
  const text = useMemo(() => JSON.stringify(value, null, 2), [value]);
  useEffect(() => {
    const dialog = ref.current;
    if (dialog && !dialog.open) dialog.showModal();
  }, []);
  const download = () => {
    const url = URL.createObjectURL(new Blob([text], { type: "application/json" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = `${value.repo.replace("/", "-")}-architecture${value.revision ? `-${value.revision.slice(0, 7)}` : ""}.json`;
    a.click();
    URL.revokeObjectURL(url);
  };
  return (
    <dialog
      ref={ref}
      onClose={onClose}
      aria-labelledby="raw-map-title"
      className="m-auto w-[min(860px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50"
    >
      <div className="flex items-center gap-3 border-b border-border px-5 py-3">
        <h2 id="raw-map-title" className="min-w-0 flex-1 truncate text-[15px] font-semibold">
          Raw map · <span className="font-mono text-[13px] text-muted">{value.repo}</span>
        </h2>
        <button
          type="button"
          className={MAP_BUTTON}
          onClick={() => {
            void navigator.clipboard?.writeText(text).then(
              () => toast("Map JSON copied"),
              () => toast("Could not copy", "error"),
            );
          }}
        >
          Copy
        </button>
        <button type="button" className={MAP_BUTTON} onClick={download}>
          Download
        </button>
        <button type="button" aria-label="Close" className={MAP_BUTTON} onClick={() => ref.current?.close()}>
          ×
        </button>
      </div>
      <pre className="scroll-thin m-0 max-h-[70vh] overflow-auto px-5 py-4 font-mono text-[12px] leading-relaxed text-muted">{text}</pre>
    </dialog>
  );
}

/** A map ant's bubble: what its colony does in this chamber, named by the file it is on. */
export function mapBubble(mode: PlaceMode, blocked: boolean, status: string, files: readonly string[]): string {
  if (blocked) return status === "waiting_for_answer" ? "waiting for you" : "idle";
  const file = files[0]?.split("/").pop();
  if (!file) return mode === "reading" ? "reading…" : "changing files…";
  return mode === "reading" ? `reading ${file}` : `editing ${file}`;
}
