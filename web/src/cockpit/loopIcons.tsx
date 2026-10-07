// The small line icons the Loops page's cards wear, one per built-in loop and one for a custom loop.
import type { ReactElement } from "react";

function Svg({ children }: { children: ReactElement | ReactElement[] }): ReactElement {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {children}
    </svg>
  );
}

/** Merge train: two lines joining. */
export const IconMerge = (): ReactElement => (
  <Svg>
    <circle cx="6" cy="5" r="2.2" />
    <circle cx="6" cy="19" r="2.2" />
    <circle cx="18" cy="12" r="2.2" />
    <path d="M6 7.2v9.6M6 9c0 3 4 3 9.8 3" />
  </Svg>
);

/** Dependencies and supply chain: a shield. */
export const IconShield = (): ReactElement => (
  <Svg>
    <path d="M12 3 5 6v5.5c0 4.2 2.9 7.6 7 9.5 4.1-1.9 7-5.3 7-9.5V6z" />
    <path d="m9 12 2.2 2.2L15.2 10" />
  </Svg>
);

/** TypeScript: remove any: angle brackets. */
export const IconBrackets = (): ReactElement => (
  <Svg>
    <path d="m8 7-5 5 5 5M16 7l5 5-5 5M13.5 5l-3 14" />
  </Svg>
);

/** Docs and README: an open book. */
export const IconBook = (): ReactElement => (
  <Svg>
    <path d="M12 6.5C10.5 5.3 8.2 4.7 4 4.8v13c4.2-.1 6.5.5 8 1.7 1.5-1.2 3.8-1.8 8-1.7v-13c-4.2-.1-6.5.5-8 1.7zM12 6.5v13" />
  </Svg>
);

/** Disk cleanup: a disk with a sweep. */
export const IconDisk = (): ReactElement => (
  <Svg>
    <ellipse cx="12" cy="6" rx="8" ry="3" />
    <path d="M4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6" />
  </Svg>
);

/** A custom loop: a loop of arrows. */
export const IconLoop = (): ReactElement => (
  <Svg>
    <path d="M17 2.5 20.5 6 17 9.5" />
    <path d="M3.5 11V10a4 4 0 0 1 4-4H20" />
    <path d="M7 21.5 3.5 18 7 14.5" />
    <path d="M20.5 13v1a4 4 0 0 1-4 4H4" />
  </Svg>
);
