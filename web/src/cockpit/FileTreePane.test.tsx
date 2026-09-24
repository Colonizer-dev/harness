import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { FileTreePane, ancestorsOf, buildTree } from "./FileTreePane";

describe("FileTreePane", () => {
  it("builds a folders-first, alphabetical tree", () => {
    const tree = buildTree(["src/b.rs", "README.md", "src/a/x.rs", "Cargo.toml"]);
    expect(tree.children.map((c) => c.name)).toEqual(["src", "Cargo.toml", "README.md"]);
    expect(tree.children[0].children.map((c) => c.name)).toEqual(["a", "b.rs"]);
  });

  it("opens every ancestor folder of a marked file", () => {
    expect([...ancestorsOf(["crates/colonizer/src/mesh.rs"])]).toEqual(["crates", "crates/colonizer", "crates/colonizer/src", "crates/colonizer/src/mesh.rs"]);
  });

  it("marks the component's files and flags files a colony is changing", () => {
    const html = renderToStaticMarkup(
      <FileTreePane
        repo="acme/app"
        revision="2882cf1abc"
        paths={["crates/colonizer/src/mesh.rs", "crates/colonizer/src/main.rs", "README.md"]}
        error={null}
        title="Mesh Supervisor"
        marked={new Set(["crates/colonizer/src/mesh.rs"])}
        changing={new Set(["crates/colonizer/src/main.rs"])}
        onClose={() => {}}
      />,
    );
    expect(html).toContain("1 file marked");
    expect(html).toMatch(/aria-selected="true"[^>]*>.*mesh\.rs/s);
    expect(html).toContain("a live colony is changing this file");
    expect(html).toContain("@ 2882cf1");
  });
});
