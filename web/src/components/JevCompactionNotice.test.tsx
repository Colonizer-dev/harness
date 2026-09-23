// The Jev compaction warning in Settings → Agent (issue #226): with the switch on, a data-egress
// and untracked-cost notice renders under it. Rendered to static markup: the test environment has
// no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { JevCompactionNotice } from "./SettingsDialog";

describe("JevCompactionNotice", () => {
  it("warns about TypeSafe data egress and untracked cost", () => {
    const out = renderToStaticMarkup(<JevCompactionNotice />);
    expect(out).toContain("api.typesafe.ai");
    expect(out).toContain("TypeSafe bills it directly");
    expect(out).toContain("colony cost");
    expect(out).toContain("text-warn");
  });
});
