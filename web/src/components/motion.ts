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

const reducedMotion = () => typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

/**
 * Keeps a scrolling thread at its bottom while content grows, easing each change in over a few frames rather than
 * jumping by the height of a new line or card. Scrolling up stops following; scrolling back to the bottom resumes it.
 * Attach `viewport` to the scroll container and `content` to the element holding everything inside it.
 */
export function useFollowBottom() {
  const viewportRef = useRef<HTMLElement | null>(null);
  const contentRef = useRef<HTMLElement | null>(null);
  const follow = useRef(true);
  const frame = useRef(0);

  const step = useCallback(() => {
    frame.current = 0;
    const el = viewportRef.current;
    if (!el || !follow.current) return;
    const target = el.scrollHeight - el.clientHeight;
    const distance = target - el.scrollTop;
    if (distance <= 0.5) return;
    // More than a screen away (a colony just opened, or a long block arrived while the tab was hidden): go there.
    if (distance > el.clientHeight || reducedMotion()) {
      el.scrollTop = target;
      return;
    }
    el.scrollTop += Math.min(distance, Math.max(1.5, distance * 0.2));
    frame.current = requestAnimationFrame(step);
  }, []);

  const kick = useCallback(() => {
    if (follow.current && !frame.current) frame.current = requestAnimationFrame(step);
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
