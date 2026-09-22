// floatingColumnClass: the fixed card column must never land on the cockpit inspector's buttons.
import { describe, expect, it } from "vitest";

import { floatingColumnClass } from "./floatingColumn";

describe("floatingColumnClass", () => {
  it("keeps the bottom-right corner when no inspector is showing", () => {
    expect(floatingColumnClass(false, false)).toBe("bottom-5 right-5 w-[380px]");
  });

  it("moves past the 360px inspector, keeping the 20px gap, while it shows", () => {
    expect(floatingColumnClass(false, true)).toBe("bottom-5 right-[380px] w-[380px]");
  });

  it("spans a narrow window, where the cockpit and its inspector are not rendered", () => {
    expect(floatingColumnClass(true, false)).toBe("inset-x-3 bottom-3");
    expect(floatingColumnClass(true, true)).toBe("inset-x-3 bottom-3");
  });
});
