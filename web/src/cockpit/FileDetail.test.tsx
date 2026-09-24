import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import { FileDetail, diffStats, parseDiff } from "./FileDetail";
import { FileTreePane } from "./FileTreePane";
import type { MapFileDetail } from "../types";

const DIFF = `diff --git a/x.rs b/x.rs
--- a/x.rs
+++ b/x.rs
@@ -10,4 +10,5 @@ fn reserve()
     let a = 1;
-    let b = 2;
+    let b = 3;
+    let c = 4;
     done();
`;

describe("parseDiff", () => {
  it("numbers lines from the hunk header and keeps old and new apart", () => {
    const rows = parseDiff(DIFF).filter((l) => l.kind !== "meta");
    expect(rows[0]).toEqual({ kind: "hunk", text: "@@ -10,4 +10,5 @@ fn reserve()" });
    expect(rows[1]).toEqual({ kind: "ctx", text: "    let a = 1;", old: 10, new: 10 });
    expect(rows[2]).toEqual({ kind: "del", text: "    let b = 2;", old: 11, new: null });
    expect(rows[3]).toEqual({ kind: "add", text: "    let b = 3;", old: null, new: 11 });
    expect(rows[5]).toEqual({ kind: "ctx", text: "    done();", old: 12, new: 13 });
    expect(diffStats(parseDiff(DIFF))).toEqual({ add: 2, del: 1 });
  });
});

describe("FileDetail", () => {
  const detail: MapFileDetail = {
    repo: "acme/app",
    path: "src/x.rs",
    colonies: [
      {
        id: "c1",
        title: "Harden the budget",
        issue: 409,
        status: "running",
        mode: "changing",
        activity: [{ ts: new Date().toISOString(), tool: "Edit", summary: "Edit (−1 +2)", agent: "Builder Settler" }],
        diff: DIFF,
        diff_truncated: false,
      },
    ],
  };
  const render = (d: MapFileDetail) =>
    renderToStaticMarkup(
      <ApiContext.Provider value={createMockApi()}>
        <FileDetail repo="acme/app" path="src/x.rs" component="Provider Gateway" onBack={() => {}} onOpenColony={() => {}} initial={d} />
      </ApiContext.Provider>,
    );

  it("shows each colony's calls on the file and its diff", () => {
    const html = render(detail);
    expect(html).toContain("Harden the budget");
    expect(html).toContain("Edit (−1 +2)");
    expect(html).toContain("Builder Settler");
    expect(html).toContain("changing");
    expect(html).toContain("let c = 4;");
    expect(html).toContain("Open colony");
    expect(html).toContain("Provider Gateway");
  });

  it("says so when no colony is on the file", () => {
    const html = render({ ...detail, colonies: [] });
    expect(html).toContain("No colony is on this file right now.");
    expect(html).toContain("It belongs to Provider Gateway.");
  });

  it("opens from the tree in place of it, and marks reading files with a dot", () => {
    const tree = (initialDetail: string | null) =>
      renderToStaticMarkup(
        <ApiContext.Provider value={createMockApi()}>
          <FileTreePane
            repo="acme/app"
            revision={null}
            paths={["src/x.rs", "src/y.rs"]}
            error={null}
            title="Gateway"
            marked={new Set(["src/x.rs"])}
            changing={new Set()}
            reading={new Set(["src/y.rs"])}
            initialDetail={initialDetail}
            onClose={() => {}}
          />
        </ApiContext.Provider>,
      );
    expect(tree(null)).toContain("a live colony is reading this file");
    const open = tree("src/y.rs");
    expect(open).toContain("back to files");
    expect(open).not.toContain('role="tree"');
  });
});
