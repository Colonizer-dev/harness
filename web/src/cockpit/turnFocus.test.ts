// Turn focus (issue #739): every request bumps a counter, so picking the same hit twice still
// re-scrolls. The listener/hook side needs React and is exercised by the panel in the browser.
import { describe, expect, it } from "vitest";

import { focusTurn, pendingTurn } from "./turnFocus";

describe("focusTurn", () => {
  it("bumps the counter for the same turn, so a repeat is a fresh request", () => {
    focusTurn("m-1");
    const first = pendingTurn();
    expect(first.id).toBe("m-1");
    focusTurn("m-1");
    const second = pendingTurn();
    expect(second.id).toBe("m-1");
    expect(second.n).toBe(first.n + 1);
  });

  it("carries a null id through, which the panel reads as 'open the colony alone'", () => {
    focusTurn(null);
    expect(pendingTurn().id).toBeNull();
  });
});
