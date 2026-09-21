// The glide that carries the colony chat down its thread, judged against the criteria from issue 120: while a
// colony writes, the newest line stays on screen and the movement reads as one steady motion — no jump when a
// card lands, no stall between lines, nothing running past the bottom. These tests drive `advance` directly at
// phone and laptop refresh rates against a content height that grows the way a real thread's does, a wrapped
// line at a time or a card arriving whole, so the law is pinned without a browser — and pinned at every refresh
// rate, since a wrapped line arrives whole whatever the display redraws at.
import { describe, expect, it } from "vitest";

import { advance, isScrollbarGrab, newGlide } from "./motion";

const REFRESH_HZ = [60, 120, 144]; // the displays this runs on: the law must not care which
const STEP_PX = [23, 40]; // what a wrapped line, and a paragraph step, add to the content's height
const PACE = 126; // px per second, a colony writing steadily
const STREAM_S = 8; // seconds of writing in a steady-streaming run
const MAX_ACCEL = 2200; // px per second², matching motion.ts: the cap on how fast the pace may change

/** A wall-clock span as a number of frames on a `hz` display. */
const span = (seconds: number, hz: number): number => Math.round(seconds * hz);

/** Frames between lines on a `hz` display, so lines of `step` px write at ~PACE px per second whatever the rate. */
const lineEvery = (hz: number, step: number): number => Math.max(1, Math.round(step / (PACE / hz)));

/** One frame as the reader lives it: how far the view moved, how far it sat behind the bottom, and what the glide believes. */
type Frame = { moved: number; gap: number; velocity: number; rate: number };

/** Runs the glide at `dt` seconds a frame against a content height given one entry per frame, recording each frame. */
const simulate = (heights: number[], dt: number) => {
  const glide = newGlide(0, 0);
  const record: Frame[] = heights.map((target) => {
    const before = glide.position;
    const position = advance(glide, target, dt);
    return { moved: position - before, gap: target - position, velocity: glide.velocity, rate: glide.rate };
  });
  return { glide, record };
};

/** A colony writing prose: a line lands every so often at ~PACE, and maybe a card arrives whole one frame. */
const streamed = (hz: number, seconds: number, step: number, card?: { at: number; px: number }): number[] => {
  const every = lineEvery(hz, step);
  const heights: number[] = [];
  let height = 0;
  for (let frame = 1; frame <= span(seconds, hz); frame++) {
    if (frame % every === 0) height += step;
    if (card && frame === card.at) height += card.px;
    heights.push(height);
  }
  return heights;
};

/** Silence: the content sits at `height` for `frames` on end. */
const silent = (height: number, frames: number): number[] => Array<number>(frames).fill(height);

const mean = (values: number[]): number => values.reduce((a, b) => a + b, 0) / values.length;

/** The value a fraction `q` of the way through the sorted list, so 0.95 is the p95. */
const quantile = (values: number[], q: number): number =>
  [...values].sort((a, b) => a - b)[Math.min(values.length - 1, Math.floor(q * values.length))];

describe("newGlide", () => {
  it("starts at rest, remembering nothing but where it is and where the bottom is", () => {
    expect(newGlide(12, 40)).toEqual({ position: 12, velocity: 0, gap: 0, rate: 0, target: 40 });
  });
});

describe("advance", () => {
  for (const hz of REFRESH_HZ) {
    const dt = 1 / hz;

    for (const step of STEP_PX) {
      it(`keeps level with a colony writing ${step} px lines on a ${hz} Hz display`, () => {
        const { glide, record } = simulate(streamed(hz, STREAM_S, step), dt);
        const secondHalf = record.slice(record.length / 2);

        // Nothing jerks: no frame moves so far it reads as a step, and the busiest frames are barely quicker than
        // the ordinary one.
        for (const frame of secondHalf) expect(frame.moved).toBeLessThan(12);
        const moving = secondHalf.filter((f) => f.moved > 0).map((f) => f.moved);
        expect(quantile(moving, 0.95)).toBeLessThanOrEqual(2 * quantile(moving, 0.5));

        // It sits level: within about half a step of the bottom on average, where the old purely proportional law
        // trailed speed times its 0.3 s lead — some 38 px at this pace.
        const meanGap = mean(secondHalf.map((f) => f.gap));
        expect(meanGap).toBeLessThan(step / 2);

        // The pace estimator has found the colony's speed and sits near it, which is what pays for the level ride.
        const pace = step / (lineEvery(hz, step) / hz);
        expect(Math.abs(glide.rate - pace)).toBeLessThan(0.3 * pace);

        // And it never stalls: after the first second of spin-up, every 100 ms of the stream moves the view
        // something — unless it is already level, since a paragraph step can be fully caught up before the next
        // one lands. Still while behind would be a stall.
        const window = Math.max(1, Math.round(hz / 10));
        for (let start = hz; start + window <= record.length; start += window) {
          const frames = record.slice(start, start + window);
          if (frames.reduce((total, f) => total + f.moved, 0) <= 0) {
            for (const frame of frames) expect(Math.abs(frame.gap)).toBeLessThan(1);
          }
        }
      });
    }

    it(`settles on the bottom once the colony stops writing (${hz} Hz)`, () => {
      const streaming = streamed(hz, STREAM_S / 2, STEP_PX[0]);
      const { record } = simulate([...streaming, ...silent(streaming[streaming.length - 1], span(3, hz))], dt);
      const after = record.slice(streaming.length);
      // Within a second of the last line the view is on the bottom, and it stays there.
      for (const frame of after.slice(hz)) expect(Math.abs(frame.gap)).toBeLessThan(1);
      expect(after[after.length - 1].gap).toBe(0);
    });

    it(`takes a settler card arriving whole (+105 px) without a jump (${hz} Hz)`, () => {
      const { record } = simulate([...silent(0, 1), ...silent(105, span(4, hz))], dt);
      for (const frame of record) {
        expect(frame.moved).toBeLessThan(12);
        expect(frame.gap).toBeGreaterThanOrEqual(-1e-9); // never past the bottom
      }
      // The card's height is glided over, not stepped, and is spent within a second and a half.
      for (const frame of record.slice(span(1.5, hz) + 1)) expect(Math.abs(frame.gap)).toBeLessThan(1);
    });

    it(`takes a question card arriving whole (+350 px) without a jump (${hz} Hz)`, () => {
      const { record } = simulate([...silent(0, 1), ...silent(350, span(5, hz))], dt);
      // Closing a card this size touches ~13 px on the last frame of the glide; the card's height itself is never
      // one frame's work.
      for (const frame of record) {
        expect(frame.moved).toBeLessThan(13);
        expect(frame.gap).toBeGreaterThanOrEqual(-1e-9);
      }
      for (const frame of record.slice(span(1.5, hz) + 1)) expect(Math.abs(frame.gap)).toBeLessThan(1);
    });

    it(`does not learn a new pace from one big insertion (${hz} Hz)`, () => {
      const { record } = simulate([...silent(0, 1), ...silent(350, span(2, hz))], dt);
      // The frame that saw the card could only be taught a line's worth of growth, so the pace rises to some
      // 49 px per second — an unclamped reading would have taken the card for ~389 px per second and kept the
      // view drifting long after the card was closed. Silence then spends the pace again.
      expect(record[1].rate).toBeLessThan(60);
      expect(record[record.length - 1].rate).toBeLessThan(10);
    });

    it(`never quickens by more than MAX_ACCEL allows in a frame (${hz} Hz)`, () => {
      // Spin-up, steady streaming and a question card landing mid-stream, all in one run: however hard the glide is
      // asked for, the speed only ever *rises* by the cap. Falling is another matter: when a card's height is spent
      // the arrival branch deliberately settles the speed straight down to what is still wanted, unclamped, so only
      // the rise is an invariant.
      const maxRise = MAX_ACCEL * dt;
      const { record } = simulate(streamed(hz, STREAM_S, STEP_PX[0], { at: span(4, hz), px: 350 }), dt);
      let previous = 0;
      for (const frame of record) {
        expect(frame.velocity - previous).toBeLessThanOrEqual(maxRise + 1e-9);
        previous = frame.velocity;
      }
    });

    it(`never moves the view past the bottom, however far behind it fell (${hz} Hz)`, () => {
      const { glide, record } = simulate([...silent(0, 1), ...silent(900, span(4, hz))], dt);
      for (const frame of record) expect(frame.gap).toBeGreaterThanOrEqual(-1e-9);
      expect(glide.position).toBeLessThanOrEqual(900);
      // It still gets there: nothing is left below the fold.
      expect(record[record.length - 1].gap).toBe(0);
    });
  }
});

describe("isScrollbarGrab", () => {
  // A viewport 100 px wide with a 16 px classic scrollbar on the right (LTR).
  const gutterEl = () => {
    const el = {
      offsetWidth: 100,
      clientWidth: 84,
      getBoundingClientRect: () => ({ left: 0, right: 100 }),
    };
    return el;
  };
  const grab = (el: ReturnType<typeof gutterEl>, clientX: number, target: unknown = el) =>
    isScrollbarGrab(el, { target: target as EventTarget, clientX } as PointerEvent);
  const stubDirection = (direction: string) => {
    (globalThis as Record<string, unknown>).getComputedStyle = () => ({ direction });
  };
  const unstubDirection = () => {
    delete (globalThis as Record<string, unknown>).getComputedStyle;
  };

  it("stops for an LTR click in the scrollbar gutter", () => {
    stubDirection("ltr");
    try {
      expect(grab(gutterEl(), 95)).toBe(true);
    } finally {
      unstubDirection();
    }
  });

  it("does not stop for an LTR click in the padding/content area", () => {
    stubDirection("ltr");
    try {
      expect(grab(gutterEl(), 50)).toBe(false);
    } finally {
      unstubDirection();
    }
  });

  it("does not stop for a click on an inner child, even over the gutter", () => {
    stubDirection("ltr");
    try {
      const el = gutterEl();
      expect(grab(el, 95, {})).toBe(false);
    } finally {
      unstubDirection();
    }
  });

  it("does not stop for empty space below the content at a content-box X", () => {
    stubDirection("ltr");
    try {
      expect(grab(gutterEl(), 40)).toBe(false);
    } finally {
      unstubDirection();
    }
  });

  it("finds the scrollbar on the left in RTL", () => {
    stubDirection("rtl");
    try {
      expect(grab(gutterEl(), 5)).toBe(true);
      expect(grab(gutterEl(), 95)).toBe(false);
    } finally {
      unstubDirection();
    }
  });

  it("never stops for an overlay scrollbar with no width", () => {
    stubDirection("ltr");
    try {
      const el = { offsetWidth: 100, clientWidth: 100, getBoundingClientRect: () => ({ left: 0, right: 100 }) };
      expect(grab(el, 99)).toBe(false);
    } finally {
      unstubDirection();
    }
  });
});
