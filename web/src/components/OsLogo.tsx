// The OS mark the Setup machine row carries (issue #208): a tiny inline glyph for the vendors the
// installer actually sees, the vendor's short name as a text chip for the rest, and nothing at all
// when the mothership did not send an `os` block. Zero dependencies — all inline SVG on the same
// 24 px grid and currentColor stroke/fill conventions as icons.tsx.
import type { ReactNode, SVGProps } from "react";
import type { OsInfo } from "../types";

type OsLogoProps = Omit<SVGProps<SVGSVGElement>, "size"> & { os?: OsInfo | null; size?: number };

/** The shared stroke wrapper: 24 px viewBox, currentColor, 2 px round strokes, hidden from arias. */
function Glyph({ size, children, ...props }: SVGProps<SVGSVGElement> & { size: number; children: ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      {children}
    </svg>
  );
}

/**
 * Vendor → drawing. Anything not keyed here falls back to a text chip below, so a vendor we have
 * never seen still renders something useful instead of breaking.
 */
const GLYPHS: Record<string, ReactNode> = {
  /**
   * Unknown vendors get a neutral mark: a generic desktop monitor rather than pretend the mark is
   * theirs. It stays recognizable at 16 px, and never looks like a broken image.
   */
  unknown: (
    <>
      <rect x="3" y="4" width="18" height="12" rx="1.5" />
      <path d="M12 16v3M12 19H8.5M12 19h3.5" />
    </>
  ),
  /** Apple: the silhouette — rounded body, leaf on top, the bite carved out of one side (even-odd path). */
  apple: (
    <>
      <path
        fillRule="evenodd"
        fill="currentColor"
        stroke="none"
        d="M12 6.4C10.3 4.5 6.4 5.3 5.3 8.3 4.3 11.4 5.4 15.2 8.2 18 9.9 19.6 11.2 20.2 12 20.2 12.8 20.2 14.1 19.6 15.8 18 16.9 17 17.6 15.6 18 14.1 18.4 12.1 18.4 10 17.7 8.2 16.8 5.4 12.9 4.4 12 6.4Z M18.3 13.8a2.9 2.9 0 1 1-5.8 0a2.9 2.9 0 0 1 5.8 0Z"
      />
      <path fill="currentColor" stroke="none" d="M10.9 5.2C10.7 3.4 9.4 2.3 7.6 2.4 9.2 3.8 10.2 4.8 10.9 5.2Z" />
    </>
  ),
  /** Ubuntu's Circle of Friends: the ring, with the three open spots as filled dots. */
  ubuntu: (
    <>
      <circle cx="12" cy="12" r="8" />
      <circle cx="12" cy="4" r="1.8" fill="currentColor" stroke="none" />
      <circle cx="18.9" cy="16" r="1.8" fill="currentColor" stroke="none" />
      <circle cx="5.1" cy="16" r="1.8" fill="currentColor" stroke="none" />
    </>
  ),
  /** Debian: the spiral whorl, stroked from the outside in. */
  debian: (
    <path d="M12 20.5C6.3 20.5 3 16.5 3 12.2 3 7.6 6.5 4 11.2 4 15.8 4 19 6.9 19 11.2 19 15.1 16 17.3 12.9 17.3 10 17.3 8 15.4 8 12.8 8 10.4 9.7 8.9 11.6 8.9 13.3 8.9 14.4 10 14.4 11.4 14.4 12.8 13.4 13.4 12.6 13.4" />
  ),
  /** Arch: the tall peak with the concave notch at the base. */
  arch: <path d="M12 3 19 21h-4.5c-.8-2.8-4.2-2.8-5 0H5L12 3Z" />,
  /** Manjaro's bars: a tall narrow one on the left, two shorter blocks stacked on the right. */
  manjaro: (
    <>
      <rect x="3" y="3" width="4.5" height="18" rx="1" fill="currentColor" stroke="none" />
      <rect x="12.5" y="3" width="6.5" height="8.5" rx="1" fill="currentColor" stroke="none" />
      <rect x="12.5" y="13.5" width="6.5" height="7.5" rx="1" fill="currentColor" stroke="none" />
    </>
  ),
  /** Alpine: a mountain with the flat double peak. */
  alpine: <path d="M4 20h16l-5.4-13.2L12 10.6 9.4 6.4 4 20Z" fill="currentColor" stroke="none" />,
  /** NixOS: the six-pointed snowflake — three crossing strokes with a forked point on each. */
  nixos: (
    <path d="M12 15.5 12 4m0 0 1.5-2.6M12 4l-1.5-2.6M15 13.8l3.9 2.2m0 0 1.5 2.6M18.9 16l3 0M9 13.8 5.1 16m0 0-3 0M5.1 16l1.5 2.6" />
  ),
};

/** The OS mark: a drawn glyph for the vendors above, the vendor's short name as text otherwise,
 *  nothing when the OS is unknown or the mothership sent no `os`. The full "Ubuntu 24.04" lives in
 *  the `title` so hovering any mark — glyph or chip — names the OS. */
export function OsLogo({ os, size = 16, ...props }: OsLogoProps) {
  if (!os) return null;
  const label = `${os.name || os.vendor}${os.version ? ` ${os.version}` : ""}`;
  const glyph = GLYPHS[os.vendor];
  if (glyph) {
    return (
      <span title={label} className="inline-flex shrink-0">
        <Glyph size={size} {...props}>
          {glyph}
        </Glyph>
      </span>
    );
  }
  // A vendor we do not draw: its own short name, kept narrow so a long one cannot stretch the row.
  return (
    <span
      title={label}
      className="inline-block max-w-[7ch] truncate text-[9px] font-semibold uppercase leading-none tracking-wide"
    >
      {os.vendor}
    </span>
  );
}