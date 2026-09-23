// A small account/org avatar with a readable fallback (issue #176): the image when there is one,
// otherwise the name's initial in the same quiet square — never an empty hole, and the initial
// also takes over when the image fails to load.
import { useState, type ReactNode } from "react";
import { cx } from "./ui";

/** The character that stands in for a name: its first letter or digit, uppercased; "?" when there is none. */
export function initialOf(name: string): string {
  const match = /[\p{L}\p{N}]/u.exec(name);
  return match ? match[0].toUpperCase() : "?";
}

const ROUNDING = { md: "rounded-md", lg: "rounded-lg", xl: "rounded-xl" } as const;

export function Avatar({
  name,
  src,
  size = 32,
  rounded = "lg",
  className,
  fallback,
}: {
  /** What the initial falls back to; the login or org name. */
  name: string;
  /** Absent means no image was ever known; a broken one falls back the same way. */
  src?: string | null;
  /** Square edge in pixels, the way LogoMark and the provider tiles take one. */
  size?: number;
  rounded?: keyof typeof ROUNDING;
  className?: string;
  /** Rendered instead of the grey initial tile when there is no image or it fails to load —
   *  the cockpit org tiles pass their coloured lettermark, so the fallback keeps its hue. */
  fallback?: ReactNode;
}) {
  const [broken, setBroken] = useState(false);
  // A different image arriving is a fresh chance to load (the prompt card moves from one org to the next).
  const [seen, setSeen] = useState(src);
  if (src !== seen) {
    setSeen(src);
    setBroken(false);
  }

  const tile = (children: ReactNode, background: string): ReactNode => (
    <span
      aria-hidden="true"
      style={{ width: size, height: size, fontSize: Math.max(10, Math.round(size * 0.42)) }}
      className={cx("grid shrink-0 select-none place-items-center font-semibold leading-none", ROUNDING[rounded], background, className)}
    >
      {children}
    </span>
  );

  if (!src || broken) return fallback ?? tile(initialOf(name), "bg-panel-3 text-muted");
  return (
    <img
      src={src}
      alt=""
      width={size}
      height={size}
      loading="lazy"
      referrerPolicy="no-referrer"
      onError={() => setBroken(true)}
      className={cx("shrink-0 select-none object-cover", ROUNDING[rounded], "bg-panel-2", className)}
    />
  );
}
