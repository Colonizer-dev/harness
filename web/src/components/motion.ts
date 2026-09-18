// How the colony chat moves: what arrives fades in, and the thread follows new content down smoothly instead of
// jumping. A colony's events come in bursts (several deltas in the same millisecond, then hundreds of milliseconds of
// nothing, and a tool card every few seconds), so anything drawn the instant it arrives looks jerky.
import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";

/**
 * True once the thread has drawn what was already there. Items mounted after that fade in; the history a colony
 * replays on connect does not, or opening a colony would animate every message at once.
 */
export const EnterContext = createContext(false);

/** The class for an item that should fade in, decided when it mounts so a re-render never replays it. */
export function useEnter(): string {
  const settled = useContext(EnterContext);
  const [enter] = useState(settled);
  return enter ? "chat-enter" : "";
}

/**
 * Settles `quietMs` after the last event of the replay burst, and at the latest `maxMs` after connecting, so a colony
 * streaming without pause still gets its fade-ins.
 */
export function useSettled(connected: boolean, lastSeq: number, quietMs = 400, maxMs = 2500): boolean {
  const [settled, setSettled] = useState(false);
  const since = useRef<number | null>(null);
  useEffect(() => {
    if (settled || !connected) return;
    since.current ??= Date.now();
    const wait = Math.max(0, Math.min(quietMs, maxMs - (Date.now() - since.current)));
    const timer = setTimeout(() => setSettled(true), wait);
    return () => clearTimeout(timer);
  }, [settled, connected, lastSeq, quietMs, maxMs]);
  return settled;
}

/** How the thread follows new content: a speed, not a jump. Tuned so a line of text nudges the pace, not the view. */
const LEAD_S = 0.3; // aim to be level with the bottom this many seconds from now
const SMOOTH_S = 0.25; // how long the distance behind is averaged over
const MAX_SPEED = 1400; // px per second
const MAX_ACCEL = 2200; // px per second², the cap on how fast the pace may change
const STOP_PX = 0.5;
const COAST_MS = 600; // keep the loop alive this long after catching up

const reducedMotion = () => typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

/**
 * Keeps a scrolling thread at its bottom while content grows, at a steady pace rather than a burst per line.
 *
 * The view moves at a speed in pixels per second, not by a fraction of the distance left: a new line nudges that
 * speed instead of starting a fresh burst that fades. How far behind the bottom is is smoothed over a quarter of a
 * second, and the speed itself may only change so fast, so a colony writing line after line reads as one movement.
 * The position is kept as a float and written once per frame, so slow movement doesn't alternate between 1 and 2 px.
 *
 * Scrolling up stops the following; scrolling back to the bottom resumes it. Attach `viewport` to the scroll
 * container and `content` to the element holding everything inside it.
 */
export function useFollowBottom() {
  const viewportRef = useRef<HTMLElement | null>(null);
  const contentRef = useRef<HTMLElement | null>(null);
  const follow = useRef(true);
  const frame = useRef(0);
  // The glide: where it thinks it is, how fast it is going, and when it last had something to do.
  const position = useRef<number | null>(null);
  const velocity = useRef(0);
  const gap = useRef(0);
  const lastFrame = useRef(0);
  const idleSince = useRef(0);

  const step = useCallback((now: number) => {
    const el = viewportRef.current;
    if (!el || !follow.current) {
      frame.current = 0;
      return;
    }
    // A frame is capped at 50 ms so a stall (a background tab, a long parse) cannot turn into a jump.
    const dt = Math.min(0.05, lastFrame.current ? (now - lastFrame.current) / 1000 : 1 / 60);
    lastFrame.current = now;

    const target = el.scrollHeight - el.clientHeight;
    // Anything else that moved the view — the reader, the browser's scroll anchoring — wins.
    if (position.current === null || Math.abs(el.scrollTop - position.current) > 2) position.current = el.scrollTop;
    const distance = target - position.current;

    // More than a screen behind (a colony just opened, or a long block arrived while the tab was hidden): go there.
    if (distance > el.clientHeight || reducedMotion()) {
      position.current = target;
      velocity.current = 0;
      gap.current = 0;
      el.scrollTop = target;
      frame.current = requestAnimationFrame(step);
      return;
    }

    // The distance a line adds arrives in one frame; smoothing it is what keeps the pace even.
    gap.current += (Math.max(0, distance) - gap.current) * Math.min(1, dt / SMOOTH_S);
    const wanted = Math.min(MAX_SPEED, gap.current / LEAD_S);
    const change = Math.max(-MAX_ACCEL * dt, Math.min(MAX_ACCEL * dt, wanted - velocity.current));
    velocity.current = Math.max(0, velocity.current + change);

    if (distance > STOP_PX) {
      position.current = Math.min(target, position.current + velocity.current * dt);
      el.scrollTop = position.current;
      idleSince.current = 0;
    } else {
      position.current = target;
      velocity.current = Math.min(velocity.current, wanted);
      // Keep gliding for a moment after catching up, so the next line carries on rather than starting again.
      idleSince.current ||= now;
      if (now - idleSince.current > COAST_MS) {
        frame.current = 0;
        return;
      }
    }
    frame.current = requestAnimationFrame(step);
  }, []);

  const kick = useCallback(() => {
    if (!follow.current || frame.current) return;
    lastFrame.current = 0;
    idleSince.current = 0;
    frame.current = requestAnimationFrame(step);
  }, [step]);

  /** Follow again from wherever the reader is, e.g. after they send a message. */
  const stickToBottom = useCallback(() => {
    follow.current = true;
    kick();
  }, [kick]);

  useEffect(() => {
    const el = viewportRef.current;
    const content = contentRef.current;
    if (!el || !content) return;
    const atBottom = () => el.scrollHeight - el.clientHeight - el.scrollTop < 8;
    const stop = () => {
      follow.current = false;
      if (frame.current) cancelAnimationFrame(frame.current);
      frame.current = 0;
      velocity.current = 0;
      gap.current = 0;
      position.current = null;
    };
    // Only a reader moves the thread up. Programmatic scrolls never turn following off, so there is no race with
    // the easing above.
    let touchY: number | null = null;
    const onWheel = (e: WheelEvent) => {
      if (e.deltaY < 0) stop();
    };
    const onTouchStart = (e: TouchEvent) => {
      touchY = e.touches[0]?.clientY ?? null;
    };
    const onTouchMove = (e: TouchEvent) => {
      const y = e.touches[0]?.clientY;
      if (touchY !== null && y !== undefined && y > touchY + 4) stop();
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (["ArrowUp", "PageUp", "Home"].includes(e.key)) stop();
    };
    // Grabbing the scrollbar lands on the container itself rather than on anything inside it.
    const onPointerDown = (e: PointerEvent) => {
      if (e.target === el) stop();
    };
    const onScroll = () => {
      if (!follow.current && atBottom()) follow.current = true;
    };
    el.addEventListener("wheel", onWheel, { passive: true });
    el.addEventListener("touchstart", onTouchStart, { passive: true });
    el.addEventListener("touchmove", onTouchMove, { passive: true });
    el.addEventListener("keydown", onKeyDown);
    el.addEventListener("pointerdown", onPointerDown);
    el.addEventListener("scroll", onScroll, { passive: true });
    const observer = new ResizeObserver(kick);
    observer.observe(content);
    observer.observe(el);
    kick();
    return () => {
      observer.disconnect();
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("touchstart", onTouchStart);
      el.removeEventListener("touchmove", onTouchMove);
      el.removeEventListener("keydown", onKeyDown);
      el.removeEventListener("pointerdown", onPointerDown);
      el.removeEventListener("scroll", onScroll);
      if (frame.current) cancelAnimationFrame(frame.current);
      frame.current = 0;
    };
  }, [kick]);

  return { viewportRef, contentRef, stickToBottom };
}
