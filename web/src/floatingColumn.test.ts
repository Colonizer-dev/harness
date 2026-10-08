// floatingColumnClass: the fixed card column must never land on the cockpit inspector's buttons.
import { describe, expect, it } from "vitest";

import { floatingColumnClass } from "./floatingColumn";

// On a phone the cockpit's tab bar runs across the foot; the max-sm tail is the same offset the
// composer keeps, so neither the bar nor a fixed card ever covers the other.
const PHONE = "max-sm:left-3 max-sm:right-3 max-sm:bottom-[calc(4.25rem+env(safe-area-inset-bottom))] max-sm:w-auto";

describe("floatingColumnClass", () => {
  it("keeps the bottom-right corner when no inspector is showing", () => {
    expect(floatingColumnClass(false)).toBe(`bottom-5 right-5 w-[380px] ${PHONE}`);
  });

  it("moves past the 360px inspector, keeping the 20px gap, while it shows", () => {
    expect(floatingColumnClass(true)).toBe(`bottom-5 right-[380px] w-[380px] ${PHONE}`);
  });
});
