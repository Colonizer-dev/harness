// The dashboard's realtime chrome (issue #446): the Live indicator plus the tween helpers that
// animate values the stream moves. Everything degrades under `prefers-reduced-motion` (jump
// straight to the value, no pulse) and in static markup (effects never run, so the target shows).
import { useEffect, useRef, useState, type ReactElement } from "react";

import type { LiveConnection } from "../liveStream";
import { formatCost } from "../spend";

/** Pulsing dot + "Live" while the stream is open, "reconnecting…" otherwise (v3: no pill, just the dot and the word). */
export function LiveIndicator({ connection = "connecting" }: { connection?: LiveConnection }): ReactElement {
  const live = connection === "open";
  return (
    <span
      role="status"
      title={live ? "realtime updates from /api/stream" : "the stream dropped — polls cover until it reconnects"}
      className={`inline-flex items-center gap-2 whitespace-nowrap text-[13px] ${live ? "text-muted" : "text-faint"}`}
    >
      <span aria-hidden="true" className={live ? "v3-live-dot" : "inline-block h-[7px] w-[7px] rounded-full bg-faint"} />
      {live ? "Live" : "reconnecting…"}
    </span>
  );
}

const reducedMotion = (): boolean =>
  typeof window === "undefined" ||
  typeof window.matchMedia !== "function" ||
  window.matchMedia("(prefers-reduced-motion: reduce)").matches;

/** Eases from the previous value to `value` over `ms`; the target at once under reduced motion. */
export function useTween(value: number, ms = 450): number {
  const [shown, setShown] = useState(value);
  const from = useRef(value);
  useEffect(() => {
    if (reducedMotion() || from.current === value) {
      from.current = value;
      setShown(value);
      return;
    }
    const start = from.current;
    const delta = value - start;
    let raf = 0;
    const t0 = performance.now();
    const step = (now: number) => {
      const t = Math.min(1, (now - t0) / ms);
      const eased = 1 - (1 - t) * (1 - t);
      const current = start + delta * eased;
      from.current = current;
      setShown(current);
      if (t < 1) raf = requestAnimationFrame(step);
      else from.current = value;
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
  }, [value, ms]);
  return shown;
}

/** A number that tweens between pushes; static markup (no effects) shows the target. */
export function TweenedValue({ value, format }: { value: number; format?: (n: number) => string }): ReactElement {
  const shown = useTween(value);
  return <>{(format ?? ((n) => String(Math.round(n))))(shown)}</>;
}

/** A live colony's running cost, tweening as the stream pushes new totals ("—" when unmeasured). */
export function LiveCost({ value }: { value: number | null }): ReactElement {
  if (value == null) return <>—</>;
  return <TweenedValue value={value} format={(n) => formatCost(n)} />;
}
