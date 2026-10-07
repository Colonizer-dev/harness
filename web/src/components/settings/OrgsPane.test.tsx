// Settings → Workspaces → Show or hide orgs (issue #1213), rendered to static markup.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ApiContext } from "../../context";
import { createMockApi } from "../../mock";
import type { OrgInfo } from "../../types";
import { OrgsPane, hideableWithoutColonies, listedOrgs } from "./OrgsPane";

const org = (name: string, total: number, settings: OrgInfo["settings"] = {}, extra: Partial<OrgInfo> = {}): OrgInfo =>
  ({ org: name, colonies: { live: 0, total }, pending_memory: 0, settings, ...extra }) as unknown as OrgInfo;

const orgs = [org("globex", 2), org("acme", 0), org("chi-archives", 0, { hidden: true }), org("qzx", 0), org("newbie", 0, {}, { awaiting_decision: true })];
const html = (list: OrgInfo[] = orgs) =>
  renderToStaticMarkup(
    <ApiContext.Provider value={createMockApi()}>
      <OrgsPane orgs={list} />
    </ApiContext.Provider>,
  );

describe("Show or hide orgs", () => {
  it("lists every decided org alphabetically, not the one still awaiting an answer", () => {
    expect(listedOrgs(orgs).map((o) => o.org)).toEqual(["acme", "chi-archives", "globex", "qzx"]);
  });

  it("gives each shown org a Show in Colonizer toggle that is on, and the hidden one a toggle that is off", () => {
    const out = html();
    expect(out).toContain('aria-label="Show acme in Colonizer"');
    expect(out).toMatch(/role="switch"[^>]*aria-checked="true"[^>]*aria-label="Show globex in Colonizer"|aria-label="Show globex in Colonizer"[^>]*aria-checked="true"/);
    expect(out).toContain("Show in Colonizer");
  });

  it("collapses hidden orgs into a Hidden (n) section at the bottom", () => {
    const out = html();
    expect(out).toContain("Hidden (1)");
    expect(out.indexOf("Hidden (1)")).toBeGreaterThan(out.indexOf("Show qzx in Colonizer"));
    // Collapsed: the hidden org's own row is not drawn until the section opens.
    expect(out).not.toContain("Show chi-archives in Colonizer");
  });

  it("offers a bulk hide for the shown orgs without colonies", () => {
    expect(hideableWithoutColonies(orgs).map((o) => o.org)).toEqual(["acme", "qzx"]);
    expect(html()).toContain("Hide all without colonies (2)");
    expect(html([org("globex", 3)])).not.toContain("Hide all without colonies (");
  });

  it("says so when there is nothing to show", () => {
    expect(html([])).toContain("No orgs yet.");
    expect(html([org("a", 0, { hidden: true })])).toContain("Every org is hidden.");
  });
});
