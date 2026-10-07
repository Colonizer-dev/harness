import { describe, expect, it } from "vitest";
import { groupByOrg, filterGroups } from "../cockpit/RepoPicker";
import type { OrgInfo } from "../types";
import {
  ALL,
  addEntry,
  buildRows,
  chipLabel,
  chipsFor,
  coversOrg,
  coversRepo,
  entryError,
  parseEntries,
  pickRow,
  removeEntry,
  toggleAll,
  toggleOrg,
  toggleRepo,
  visibleOrgs,
  visibleRepos,
} from "./repoSelect";

const org = (name: string, hidden = false) => ({ org: name, colonies: { live: 0, total: 0 }, pending_memory: 0, settings: hidden ? { hidden: true } : {} }) as unknown as OrgInfo;
const repos = ["acme/api", "acme/web", "globex/app", "qzx/old"];
const orgs = [org("acme"), org("globex"), org("qzx", true)];
const rowsFor = (query = "", allowAll = true) => {
  const names = visibleRepos(repos, orgs);
  return buildRows(filterGroups(groupByOrg(names, visibleOrgs(orgs)), query, () => null), query, allowAll, () => null);
};

describe("RepoMultiSelect value edits", () => {
  it("selects all, which stands for every narrower choice, and clears again", () => {
    expect(toggleAll([])).toEqual([ALL]);
    expect(toggleAll(["acme", "globex/app"])).toEqual([ALL]);
    expect(toggleAll([ALL])).toEqual([]);
    expect(coversRepo([ALL], "anything/at-all")).toBe(true);
    expect(coversOrg([ALL], "acme")).toBe(true);
  });

  it("selects an org, dropping its single repositories and the wildcard", () => {
    expect(toggleOrg([], "acme")).toEqual(["acme"]);
    expect(toggleOrg(["acme/api", "globex/app"], "acme")).toEqual(["globex/app", "acme"]);
    expect(toggleOrg([ALL], "acme")).toEqual(["acme"]);
    expect(toggleOrg(["acme"], "ACME")).toEqual([]);
    expect(coversRepo(["acme"], "Acme/web")).toBe(true);
    expect(coversRepo(["acme"], "globex/app")).toBe(false);
  });

  it("selects individual repositories and unselects them", () => {
    const one = toggleRepo([], "acme/api");
    const two = toggleRepo(one, "globex/app");
    expect(two).toEqual(["acme/api", "globex/app"]);
    expect(toggleRepo(two, "ACME/api")).toEqual(["globex/app"]);
  });

  it("narrows the wildcard to the one repository picked under it, and ignores one an org entry covers", () => {
    expect(toggleRepo([ALL], "acme/api")).toEqual(["acme/api"]);
    expect(toggleRepo(["acme"], "acme/api")).toEqual(["acme/api"]);
  });

  it("labels chips: all, all in an org, or the repository, with the rest folded into +n", () => {
    expect(chipLabel(ALL)).toBe("All repositories");
    expect(chipLabel("Keep-Shipping")).toBe("All in Keep-Shipping");
    expect(chipLabel("owlpost-to/backend")).toBe("owlpost-to/backend");
    expect(chipsFor(["a", "b/c", "d", "e/f", "g", "h"], 3)).toEqual({ shown: ["a", "b/c", "d"], more: 3 });
    expect(chipsFor(["a"], 3)).toEqual({ shown: ["a"], more: 0 });
  });

  it("removes a chip by entry, in any case", () => {
    expect(removeEntry(["acme", "globex/app"], "ACME")).toEqual(["globex/app"]);
  });

  it("adds free text the server accepts and says why it refuses the rest", () => {
    expect(addEntry([], "owlpost-to/not-yet-known")).toEqual({ value: ["owlpost-to/not-yet-known"], error: null });
    expect(addEntry(["acme"], " globex ").value).toEqual(["acme", "globex"]);
    expect(addEntry(["acme/api"], "ACME/API").value).toEqual(["acme/api"]);
    expect(addEntry(["acme"], "*").value).toEqual([ALL]);
    expect(addEntry(["acme"], "not a repo").error).toContain("is not an owner or owner/name");
    expect(addEntry(["acme"], "a/b/c").error).toContain("is not an owner or owner/name");
    expect(addEntry(["acme"], "  ").error).toContain("Name a repository");
    expect(entryError("acme/*")).not.toBeNull();
  });

  it("reads a typed list, a comma or space or newline apart, once each", () => {
    expect(parseEntries("acme, globex/app\nAcme  *")).toEqual(["acme", "globex/app", "*"]);
  });
});

describe("RepoMultiSelect lines", () => {
  it("lists All, each org with its repositories, and leaves out a hidden org everywhere", () => {
    const rows = rowsFor();
    expect(rows[0]).toEqual({ kind: "all" });
    expect(rows.filter((r) => r.kind === "org").map((r) => (r.kind === "org" ? r.org : ""))).toEqual(["acme", "globex"]);
    expect(rows.filter((r) => r.kind === "repo").map((r) => (r.kind === "repo" ? r.repo : ""))).toEqual(["acme/api", "acme/web", "globex/app"]);
    expect(JSON.stringify(rows)).not.toContain("qzx");
  });

  it("offers no All where the setting has no wildcard", () => {
    expect(rowsFor("", false).some((r) => r.kind === "all")).toBe(false);
  });

  it("filters by what is typed and offers the typed text as a new entry", () => {
    const rows = rowsFor("globex/new");
    expect(rows.at(-1)).toEqual({ kind: "add", text: "globex/new" });
    const found = rowsFor("web");
    // A partial name still offers itself as an org entry, after the matches.
    expect(found.map((r) => r.kind)).toEqual(["org", "repo", "add"]);
    expect(rowsFor("acme").some((r) => r.kind === "add")).toBe(false);
  });

  it("applies a picked row to the value, round trip through the lines", () => {
    let value: string[] = [];
    const rows = rowsFor();
    value = pickRow(value, rows.find((r) => r.kind === "org" && r.org === "acme")!).value;
    value = pickRow(value, rows.find((r) => r.kind === "repo" && r.repo === "globex/app")!).value;
    expect(value).toEqual(["acme", "globex/app"]);
    expect(pickRow(value, { kind: "add", text: "bad entry" }).error).not.toBeNull();
    expect(pickRow(value, { kind: "all" }).value).toEqual([ALL]);
  });

  it("keeps a repository whose org the list does not know", () => {
    expect(visibleRepos(["zed/one", "qzx/old"], orgs)).toEqual(["zed/one"]);
    expect(visibleRepos(["zed/one"], [])).toEqual(["zed/one"]);
  });
});
