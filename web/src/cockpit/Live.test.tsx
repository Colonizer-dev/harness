// The realtime chrome (issue #446): the Live indicator's two states and the tweened values'
// static-markup rendering — effects never run there, so the target shows with no animation.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { LiveCost, LiveIndicator, TweenedValue } from "./Live";

describe("LiveIndicator", () => {
  it("reads Live while the stream is open", () => {
    const html = renderToStaticMarkup(<LiveIndicator connection="open" />);
    expect(html).toContain("Live");
    expect(html).not.toContain("reconnecting");
    expect(html).toContain("live-dot");
  });

  it("reads reconnecting… while connecting, down, or unset", () => {
    for (const html of [
      renderToStaticMarkup(<LiveIndicator connection="connecting" />),
      renderToStaticMarkup(<LiveIndicator connection="reconnecting" />),
      renderToStaticMarkup(<LiveIndicator />),
    ]) {
      expect(html).toContain("reconnecting…");
      expect(html).not.toContain("live-dot");
    }
  });
});

describe("tweened values", () => {
  it("renders the target in static markup", () => {
    expect(renderToStaticMarkup(<TweenedValue value={7} />)).toContain("7");
    expect(renderToStaticMarkup(<LiveCost value={0.87} />)).toContain("$0.87");
    expect(renderToStaticMarkup(<LiveCost value={null} />)).toContain("—");
  });
});
