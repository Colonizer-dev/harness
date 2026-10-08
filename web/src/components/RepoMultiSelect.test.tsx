// The closed state of the repository multi-select, rendered to static markup: chips, the +n fold,
// the remove buttons, and the empty prompt. The dropdown's lines and edits are in repoSelect.test.ts.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { RepoMultiSelect } from "./RepoMultiSelect";

const html = (value: string[], extra: Partial<Parameters<typeof RepoMultiSelect>[0]> = {}) =>
  renderToStaticMarkup(<RepoMultiSelect value={value} onChange={() => {}} label="repositories to merge in" repos={["acme/api"]} orgs={[]} {...extra} />);

describe("RepoMultiSelect chips", () => {
  it("shows the prompt when nothing is chosen", () => {
    const out = html([]);
    expect(out).toContain("Choose repositories");
    expect(out).toContain('aria-label="repositories to merge in"');
    expect(out).toContain('aria-haspopup="listbox"');
  });

  it("renders All, an org and a repository as chips, each with a way to remove it", () => {
    expect(html(["*"])).toContain("All repositories");
    const out = html(["Keep-Shipping", "owlpost-to/backend"]);
    expect(out).toContain("All in Keep-Shipping");
    expect(out).toContain("owlpost-to/backend");
    expect(out).toContain('aria-label="remove All in Keep-Shipping"');
    expect(out).toContain('aria-label="remove owlpost-to/backend"');
  });

  it("folds the chips past the limit into +n", () => {
    const out = html(["a/1", "a/2", "a/3", "a/4", "a/5", "a/6"]);
    expect(out).toContain("a/3");
    expect(out).not.toContain(">a/4<");
    expect(out).toContain("+3");
  });

  it("disables the controls when the setting is locked", () => {
    expect(html(["acme/api"], { disabled: true })).toContain('disabled=""');
    expect(html(["acme/api"])).not.toContain('disabled=""');
  });
});
