// The red team's mark: a hooded figure with a visor, drawn on the same 24px stroke grid as the
// rest of the cockpit's icons so it sits beside them at any size.
import type { SVGProps } from "react";

export function HackerIcon({ size = 16, ...props }: SVGProps<SVGSVGElement> & { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      {/* hood */}
      <path d="M4.5 21c0-5.2 1.4-9.4 3.2-12.1C9 6.9 10.4 3.5 12 3.5s3 3.4 4.3 5.4C18.1 11.6 19.5 15.8 19.5 21" />
      {/* face opening */}
      <path d="M8.3 13.2c0-2.3 1.7-4.2 3.7-4.2s3.7 1.9 3.7 4.2c0 2.2-1.7 3.8-3.7 3.8s-3.7-1.6-3.7-3.8z" />
      {/* visor */}
      <path d="M9.1 12.4h5.8" strokeWidth={2.4} />
    </svg>
  );
}
