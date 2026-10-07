import { describe, expect, it } from "vitest";

import type { OrgEntry } from "../orgs";
import { loadOrgList, pushRecentOrg, togglePinned, workspaceGroups } from "./workspaceGroups";

const org = (name: string): OrgEntry => ({ org: name, live: 0, queued: 0, total: 0, pending: 0, avatar: null });
const orgs = ["acme", "initech", "hooli", "webshop"].map(org);

describe("workspace groups", () => {
  it("lists a workspace once: pinned, then recent, then the rest", () => {
    const g = workspaceGroups(orgs, [org("old")], ["hooli"], ["initech", "hooli"], "");
    expect(g.pinned.map((o) => o.org)).toEqual(["hooli"]);
    expect(g.recent.map((o) => o.org)).toEqual(["initech"]);
    expect(g.all.map((o) => o.org)).toEqual(["acme", "webshop"]);
    expect(g.off.map((o) => o.org)).toEqual(["old"]);
  });
  it("turns into one flat list of matches while searching", () => {
    const g = workspaceGroups(orgs, [org("hooli-old")], ["hooli"], ["initech"], "HOO");
    expect(g.pinned).toEqual([]);
    expect(g.all.map((o) => o.org)).toEqual(["hooli"]);
    expect(g.off.map((o) => o.org)).toEqual(["hooli-old"]);
  });
  it("keeps three recents, newest first, and toggles pins", () => {
    expect(["a", "b", "c", "d"].reduce<string[]>((l, o) => pushRecentOrg(l, o), [])).toEqual(["d", "c", "b"]);
    expect(pushRecentOrg(["a", "b"], "B")).toEqual(["B", "a"]);
    expect(togglePinned(["a"], "b")).toEqual(["a", "b"]);
    expect(togglePinned(["a", "b"], "A")).toEqual(["b"]);
    expect(loadOrgList("not json")).toEqual([]);
    expect(loadOrgList('["x",3]')).toEqual(["x"]);
  });
});
