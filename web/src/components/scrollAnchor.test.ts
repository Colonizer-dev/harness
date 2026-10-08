// Prepending older messages must not move what the reader is looking at (issue #1210).
import { describe, expect, it } from "vitest";

import { NEAR_TOP_PX, anchoredTop, nearTop, restoreAnchor, takeAnchor, underfilled } from "./scrollAnchor";

describe("scroll anchor", () => {
  it("moves the view down by exactly the height that was added above it", () => {
    const box = { scrollHeight: 5000, scrollTop: 120 };
    const anchor = takeAnchor(box);
    box.scrollHeight = 8200; // an older page of 3200 px went in above
    expect(anchoredTop(anchor, box.scrollHeight)).toBe(3320);
    expect(restoreAnchor(box, anchor)).toBe(true);
    expect(box.scrollTop).toBe(3320);
    // What was at the reader's eye line (offset 120 in the old layout) is at 3320 in the new one.
    expect(box.scrollTop - 120).toBe(box.scrollHeight - 5000);
  });

  it("leaves the view alone when nothing was added, or when it already sits there", () => {
    const box = { scrollHeight: 4000, scrollTop: 90 };
    const anchor = takeAnchor(box);
    expect(restoreAnchor(box, anchor)).toBe(false);
    expect(box.scrollTop).toBe(90);
    box.scrollHeight = 4500;
    box.scrollTop = 590;
    expect(restoreAnchor(box, anchor)).toBe(false);
  });

  it("can be applied again after the content settles its height, without drifting", () => {
    const box = { scrollHeight: 3000, scrollTop: 40 };
    const anchor = takeAnchor(box);
    box.scrollHeight = 5000;
    restoreAnchor(box, anchor);
    expect(box.scrollTop).toBe(2040);
    box.scrollHeight = 5100; // markdown settled 100 px taller
    restoreAnchor(box, anchor);
    expect(box.scrollTop).toBe(2140);
    restoreAnchor(box, anchor);
    expect(box.scrollTop).toBe(2140);
  });

  it("asks for the next page near the top, and when the thread is too short to scroll", () => {
    expect(nearTop(0)).toBe(true);
    expect(nearTop(NEAR_TOP_PX - 1)).toBe(true);
    expect(nearTop(NEAR_TOP_PX)).toBe(false);
    expect(underfilled({ scrollHeight: 500, clientHeight: 600 })).toBe(true);
    expect(underfilled({ scrollHeight: 4000, clientHeight: 600 })).toBe(false);
  });
});
