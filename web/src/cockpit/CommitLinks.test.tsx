// The colony's commit list: an orphaned link carries its badge and the tooltip that says why it was
// kept, a re-pointed one names the sha it was before. Rendered through react-dom/server.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { CommitLinks, ORPHANED_TOOLTIP } from "./CommitLinks";

const at = "2026-09-29T10:00:00Z";

describe("CommitLinks", () => {
  it("badges an orphaned link with the tooltip that explains it", () => {
    const html = renderToStaticMarkup(
      <CommitLinks
        commits={[
          { sha: "1111111aaaa", previous: [], orphaned: true, recorded_at: at },
          { sha: "2222222bbbb", previous: ["3333333cccc"], orphaned: false, recorded_at: at },
        ]}
      />,
    );
    expect(html).toContain("1111111");
    expect(html).toContain(">orphaned<");
    expect(html).toContain(`title="${ORPHANED_TOOLTIP}"`);
    expect(html).toContain("was 3333333");
    expect(html.match(/>orphaned</g)).toHaveLength(1);
  });

  it("says so when nothing is recorded", () => {
    expect(renderToStaticMarkup(<CommitLinks commits={[]} />)).toContain("no commits recorded yet");
  });
});
