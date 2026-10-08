// Keeps the reader's place in a scrolling thread when older content is added above it (issue #1210).

/** How close to the top of the thread, in px, the reader has to scroll before the next older page loads. */
export const NEAR_TOP_PX = 160;

/** The turns before the loaded part beyond which "Jump to start" is worth showing. */
export const JUMP_AFTER_TURNS = 20;

export interface ScrollBox {
  scrollHeight: number;
  scrollTop: number;
}

/** What to remember before content is prepended: how tall the thread was and where the reader was in it. */
export interface Anchor {
  height: number;
  top: number;
}

export function takeAnchor(box: ScrollBox): Anchor {
  return { height: box.scrollHeight, top: box.scrollTop };
}

/** Where the view has to be so what the reader was looking at has not moved, once the thread is `height` tall. */
export function anchoredTop(anchor: Anchor, height: number): number {
  return anchor.top + (height - anchor.height);
}

/** Puts the view back on the anchor. Returns whether it had to move. */
export function restoreAnchor(box: ScrollBox, anchor: Anchor): boolean {
  const top = anchoredTop(anchor, box.scrollHeight);
  if (Math.abs(top - box.scrollTop) < 1) return false;
  box.scrollTop = top;
  return true;
}

/** Whether the reader is close enough to the top that the next older page should be on its way. */
export function nearTop(scrollTop: number): boolean {
  return scrollTop < NEAR_TOP_PX;
}

/** Whether the thread is too short to scroll, so nothing would ever trigger the next page. */
export function underfilled(box: { scrollHeight: number; clientHeight: number }): boolean {
  return box.scrollHeight <= box.clientHeight + NEAR_TOP_PX;
}
