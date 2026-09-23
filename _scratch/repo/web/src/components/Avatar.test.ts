// The avatar's initial fallback (issue #176): an org or account without a picture, or whose picture
// fails to load, still shows a character in its tile — never an empty hole.
import { describe, expect, it } from "vitest";

import { initialOf } from "./Avatar";

describe("initialOf", () => {
  it("takes the first letter, uppercased", () => {
    expect(initialOf("acme")).toBe("A");
  });

  it("keeps a login that starts with a digit", () => {
    expect(initialOf("1password")).toBe("1");
  });

  it("skips a leading dash, which carries no picture of the name", () => {
    expect(initialOf("-acme")).toBe("A");
  });

  it("falls back to ? when there is no letter or digit at all", () => {
    expect(initialOf("")).toBe("?");
    expect(initialOf("---")).toBe("?");
  });
});
