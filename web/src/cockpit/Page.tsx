// The one page container every cockpit view sits in (the Nest excepted: it is a full-bleed canvas).
//
// A view hands its content to <Page> and gets the scroll root, the top and bottom padding and the
// frame: fluid to the window, with clamp()-based side gutters, and a cap only as generous as the
// content can use. The widths live in index.css (`[data-page]`, `.page-frame`) so a view never
// picks its own max width again, and the next page added gets the same frame for free.
//
//   wide      dashboards, tables, lists and grids: they take the width, up to a cap that only
//             stops a 5K screen from stretching a row across the room.
//   readable  forms (Secrets, the launch picker; Settings through `cap`): capped where a label
//             and its control, or a line of text, drift too far apart.
//   full      tools that own their panes edge to edge (chat, the code editor, a colony, memory): no
//             scroll root, no gutters; they keep their own inner reading columns. Chrome that
//             spans the window but centres its content (Settings) marks it `.page-pad`.
import type { ReactElement, ReactNode } from "react";

import { cx } from "../components/ui";

export type PageWidth = "wide" | "readable" | "full";

export function Page({
  width = "wide",
  cap,
  className,
  frameClassName,
  children,
}: {
  width?: PageWidth;
  /** For a full-bleed page: the width its `.page-pad` columns are held to (wide unless set). */
  cap?: "wide" | "readable";
  /** Extra classes on the scroll root (rarely needed). */
  className?: string;
  /** Layout classes for the frame: `flex flex-col gap-10` and the like. */
  frameClassName?: string;
  children: ReactNode;
}): ReactElement {
  if (width === "full") {
    return (
      <div data-page="full" data-cap={cap} className={cx("relative flex min-h-0 min-w-0 flex-1 flex-col", className)}>
        {children}
      </div>
    );
  }
  return (
    <main data-page={width} className={cx("cockpit scroll-thin min-h-0 flex-1 overflow-y-auto pb-24 pt-10", className)}>
      <div className={cx("page-frame", frameClassName)}>{children}</div>
    </main>
  );
}
