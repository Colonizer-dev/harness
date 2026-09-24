// The avatar's initial fallback (issue #176): an org or account without a picture, or whose picture
// fails to load, still shows a character in its tile — never an empty hole.
import { describe, expect, it } from "vitest";

import { avatarSources, initialOf, proxiedAvatar } from "./Avatar";

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

describe("proxiedAvatar", () => {
  it("routes GitHub avatars through the mothership's cache", () => {
    const src = "https://avatars.githubusercontent.com/u/583231?v=4&s=64";
    expect(proxiedAvatar(src)).toBe(`/api/img?u=${encodeURIComponent(src)}`);
    expect(proxiedAvatar("https://github.com/octo-cat.png")).toBe(`/api/img?u=${encodeURIComponent("https://github.com/octo-cat.png")}`);
  });

  it("leaves every other URL alone, as the proxy would refuse it", () => {
    for (const src of [
      "http://avatars.githubusercontent.com/u/1",
      "https://avatars.githubusercontent.com:444/u/1",
      "https://evil.example/u/1",
      "https://github.com/acme/repo.png",
      "https://raw.githubusercontent.com/a/b/x.png",
      "not a url",
    ]) {
      expect(proxiedAvatar(src)).toBeNull();
    }
  });

  it("tries the cached copy first, then the direct URL", () => {
    const src = "https://github.com/octocat.png";
    expect(avatarSources(src)).toEqual([proxiedAvatar(src), src]);
    expect(avatarSources("https://example.com/logo.png")).toEqual(["https://example.com/logo.png"]);
    expect(avatarSources(null)).toEqual([]);
  });
});
