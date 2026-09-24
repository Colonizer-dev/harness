// A small account/org avatar with a readable fallback (issue #176): the image when there is one,
// otherwise the name's initial in the same quiet square — never an empty hole, and the initial
// also takes over when the image fails to load. A GitHub avatar is loaded through the
// mothership's cache (`/api/img`, crates/colonizer/src/img_proxy.rs) so faces survive a GitHub
// outage; if that fails the direct URL is tried before the initial.
import { useState, type ReactNode } from "react";
import { cx } from "./ui";

/** The character that stands in for a name: its first letter or digit, uppercased; "?" when there is none. */
export function initialOf(name: string): string {
  const match = /[\p{L}\p{N}]/u.exec(name);
  return match ? match[0].toUpperCase() : "?";
}

/** The mothership's cached copy of a GitHub avatar; `null` for any URL the proxy does not fetch
 *  (it allows exactly https avatars.githubusercontent.com and github.com/<login>.png). */
export function proxiedAvatar(src: string): string | null {
  let url: URL;
  try {
    url = new URL(src);
  } catch {
    return null;
  }
  if (url.protocol !== "https:" || url.port || url.username || url.password) return null;
  const allowed = url.hostname === "avatars.githubusercontent.com" || (url.hostname === "github.com" && /^\/[A-Za-z0-9-]{1,39}\.png$/.test(url.pathname));
  return allowed ? `/api/img?u=${encodeURIComponent(src)}` : null;
}

/** What to load, in order: the proxied copy when there is one, then the direct URL. */
export function avatarSources(src: string | null | undefined): string[] {
  if (!src) return [];
  const proxied = proxiedAvatar(src);
  return proxied ? [proxied, src] : [src];
}

const ROUNDING = { md: "rounded-md", lg: "rounded-lg", xl: "rounded-xl", full: "rounded-full" } as const;

export function Avatar({
  name,
  src,
  size = 32,
  rounded = "lg",
  className,
  fallback,
  title,
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
  /** Hover text on the image. */
  title?: string;
}) {
  // Which of the sources is loading; past the last one the fallback shows.
  const [attempt, setAttempt] = useState(0);
  // A different image arriving is a fresh chance to load (the prompt card moves from one org to the next).
  const [seen, setSeen] = useState(src);
  if (src !== seen) {
    setSeen(src);
    setAttempt(0);
  }
  const sources = avatarSources(src);
  const current = sources[attempt];

  const tile = (children: ReactNode, background: string): ReactNode => (
    <span
      aria-hidden="true"
      style={{ width: size, height: size, fontSize: Math.max(10, Math.round(size * 0.42)) }}
      className={cx("grid shrink-0 select-none place-items-center font-semibold leading-none", ROUNDING[rounded], background, className)}
    >
      {children}
    </span>
  );

  if (!current) return fallback ?? tile(initialOf(name), "bg-panel-3 text-muted");
  return (
    <img
      src={current}
      alt=""
      width={size}
      height={size}
      loading="lazy"
      referrerPolicy="no-referrer"
      title={title}
      onError={() => setAttempt((n) => n + 1)}
      className={cx("shrink-0 select-none object-cover", ROUNDING[rounded], "bg-panel-2", className)}
    />
  );
}
