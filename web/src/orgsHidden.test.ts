// Hidden orgs (issue #1213): out of the workspace switcher, kept for Settings → Workspaces, and
// back in the moment the toggle is turned off. Their colonies are still counted.
import { describe, expect, it } from "vitest";
import { orgEntries, orgHidden, reconcileSelectedOrg } from "./orgs";
import type { OrgInfo, Session } from "./types";

const org = (name: string, settings: OrgInfo["settings"] = {}, total = 0): OrgInfo =>
  ({ org: name, colonies: { live: 0, total }, pending_memory: 0, settings }) as unknown as OrgInfo;
const colony = (orgName: string) => ({ id: `c-${orgName}`, org: orgName, repo: `${orgName}/app`, status: "running" }) as unknown as Session;

describe("hidden orgs in the workspace list", () => {
  it("reads hidden only when it is explicitly true", () => {
    expect(orgHidden(undefined)).toBe(false);
    expect(orgHidden({})).toBe(false);
    expect(orgHidden({ hidden: false })).toBe(false);
    expect(orgHidden({ hidden: true })).toBe(true);
  });

  it("leaves a hidden org out of the switcher's lists but returns it as concealed", () => {
    const entries = orgEntries([org("acme"), org("qzx", { hidden: true }), org("old", { enabled: false })], []);
    expect(entries.visible.map((e) => e.org)).toEqual(["acme"]);
    expect(entries.hidden.map((e) => e.org)).toEqual(["old"]);
    expect(entries.concealed.map((e) => e.org)).toEqual(["qzx"]);
  });

  it("keeps a hidden org's running colonies counted, and does not let a colony bring it back", () => {
    const entries = orgEntries([org("acme"), org("qzx", { hidden: true }, 1)], [colony("qzx"), colony("qzx"), colony("acme")]);
    expect(entries.visible.map((e) => e.org)).toEqual(["acme"]);
    expect(entries.concealed).toMatchObject([{ org: "qzx", total: 2, live: 2 }]);
  });

  it("clears a stored selection of an org that has since been hidden", () => {
    const entries = orgEntries([org("acme"), org("qzx", { hidden: true })], []);
    expect(reconcileSelectedOrg("qzx", entries, true)).toBeNull();
    expect(reconcileSelectedOrg("acme", entries, false)).toBe("acme");
  });

  it("restores the org when the toggle goes back", () => {
    const entries = orgEntries([org("acme"), org("qzx", { hidden: false })], []);
    expect(entries.visible.map((e) => e.org)).toEqual(["acme", "qzx"]);
    expect(entries.concealed).toEqual([]);
  });
});
