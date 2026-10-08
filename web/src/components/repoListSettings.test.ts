// The stored value of every setting RepoMultiSelect replaced, through the mock API the way the
// server stores it: `*`, an org and a repository come back exactly as saved.
import { describe, expect, it } from "vitest";
import { defaultMergeLoopSettings, repoOptIn } from "../cockpit/mergeLoop";
import { createMockApi } from "../mock";
import { pickRow, type Row } from "./repoSelect";

const pick = (value: string[], row: Row) => pickRow(value, row).value;
const all: Row = { kind: "all" };
const orgRow: Row = { kind: "org", org: "acme", count: 2, info: undefined };
const repoRow: Row = { kind: "repo", repo: "globex/app", description: null };

describe("repository list settings round trip", () => {
  it("Docs & README loop: allow", async () => {
    const api = createMockApi();
    const view = await api.docsLoop();
    for (const allow of [pick([], all), pick([], orgRow), pick(pick([], orgRow), repoRow)]) {
      const saved = await api.saveDocsLoop({ ...view.settings, allow });
      expect(saved.settings.allow).toEqual(allow);
      expect((await api.docsLoop()).settings.allow).toEqual(allow);
    }
  });

  it("Supply-chain loop: allow", async () => {
    const api = createMockApi();
    const { settings } = await api.supplyChainLoop();
    for (const allow of [["*"], ["acme"], ["acme", "globex/app"]]) {
      expect((await api.saveSupplyChainLoop({ ...settings, allow })).settings.allow).toEqual(allow);
      expect((await api.supplyChainLoop()).settings.allow).toEqual(allow);
    }
  });

  it("TypeScript any loop: allow", async () => {
    const api = createMockApi();
    const { settings } = await api.tsAnyLoop();
    for (const allow of [["*"], ["acme"], ["acme", "globex/app"]]) {
      expect((await api.saveTsAnyLoop({ ...settings, allow })).settings.allow).toEqual(allow);
      expect((await api.tsAnyLoop()).settings.allow).toEqual(allow);
    }
  });

  it("Merge train loop: allow, never and local_checks", async () => {
    const api = createMockApi();
    const settings = { ...defaultMergeLoopSettings(), allow: ["*"], never: ["qzx/fork"], local_checks: ["acme"] };
    const saved = await api.saveMergeLoop(settings);
    expect(saved.settings.allow).toEqual(["*"]);
    expect(saved.settings.never).toEqual(["qzx/fork"]);
    expect(saved.settings.local_checks).toEqual(["acme"]);
  });

  it("Merge train: a wildcard opts a repository in through the org, and never still wins", () => {
    const s = { ...defaultMergeLoopSettings(), allow: ["*"], never: ["qzx/fork"] };
    expect(repoOptIn(s, "acme/api")).toBe("org");
    expect(repoOptIn(s, "qzx/fork")).toBe("never");
    expect(repoOptIn({ ...s, allow: [] }, "acme/api")).toBe("off");
  });

  it("Org settings: merge_prs and close_superseded_prs, and the hidden switch beside them", async () => {
    const api = createMockApi();
    const saved = await api.saveOrg("acme", { merge_prs: ["*"], close_superseded_prs: ["acme", "acme/api"], hidden: true });
    expect(saved.settings).toMatchObject({ merge_prs: ["*"], close_superseded_prs: ["acme", "acme/api"], hidden: true });
    // A save that does not name hidden keeps it, like the server; naming it false turns it back off.
    const kept = await api.saveOrg("acme", { merge_prs: ["acme"] });
    expect(kept.settings).toMatchObject({ merge_prs: ["acme"], hidden: true });
    const back = await api.saveOrg("acme", { hidden: false });
    expect(back.settings.hidden).toBe(false);
  });
});
