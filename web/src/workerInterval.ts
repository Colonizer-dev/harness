// Issue #159: Chrome throttles setInterval to ~1/min in a hidden tab, which delayed the
// 4s session poll — and so notifications for non-open colonies. A Web Worker keeps its
// own setInterval unthrottled, so the session poll ticks from a worker instead.

/**
 * Like setInterval, but ticks from a Web Worker (built from an inline Blob, so no
 * bundler worker config is needed) so hidden tabs are not throttled. Falls back to a
 * plain setInterval when workers are unavailable (SSR, tests, old browsers).
 */
export function startPollInterval(cb: () => void, ms: number): () => void {
  if (typeof Worker === "undefined") {
    const id = setInterval(cb, ms);
    return () => clearInterval(id);
  }
  try {
    const source = `setInterval(function () { postMessage("tick"); }, ${ms});`;
    const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
    const worker = new Worker(url);
    worker.onmessage = () => {
      cb();
    };
    return () => {
      worker.terminate();
      URL.revokeObjectURL(url);
    };
  } catch {
    const id = setInterval(cb, ms);
    return () => clearInterval(id);
  }
}

/** Re-runs cb the moment the tab becomes visible again, so a hidden tab catches up at once. */
export function refreshOnVisible(cb: () => void): () => void {
  if (typeof document === "undefined") return () => {};
  const onChange = () => {
    if (document.visibilityState === "visible") cb();
  };
  document.addEventListener("visibilitychange", onChange);
  return () => document.removeEventListener("visibilitychange", onChange);
}
