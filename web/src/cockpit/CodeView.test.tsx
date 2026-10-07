import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import type { OrgInfo, Repo } from "../types";
import { CodeView, type CodeLayout } from "./CodeView";

const org = (name: string): OrgInfo => ({ org: name, colonies: { live: 0, total: 0 }, pending_memory: 0, settings: {}, description: `${name} description` }) as OrgInfo;
const repo = (full_name: string): Repo => ({ full_name, description: null, private: false, fork: false, archived: false, open_issues_count: 0, pushed_at: null, has_issues: true }) as unknown as Repo;

function render(selectedOrg: string | null, initialLayout?: CodeLayout) {
  return renderToStaticMarkup(
    <ApiContext.Provider value={createMockApi()}>
      <CodeView orgs={[org("Acme"), org("Beta")]} repos={[repo("Acme/api"), repo("Acme/web"), repo("Beta/app")]} sessions={[]} selectedOrg={selectedOrg} onSelectOrg={() => {}} onCreated={() => {}} onOpenColony={() => {}} initialLayout={initialLayout} />
    </ApiContext.Provider>,
  );
}

describe("CodeView", () => {
  it("asks for a workspace when none is chosen", () => {
    const html = render(null);
    expect(html).toContain("The Code page is per workspace");
    expect(html).toContain("Acme") ;
    expect(html).toContain("Beta");
  });

  it("shows one card per repository of the chosen workspace, with the workspace totals", () => {
    const html = render("Acme");
    expect(html).toContain("Code · Acme");
    expect(html).toContain("Acme description");
    expect(html.match(/Open editor/g)?.length).toBe(2);
    expect(html).toContain(">api<");
    expect(html).not.toContain(">app<");
    for (const label of ["Lines", "Coverage", "Commits · 52w", "Branches", "Open PRs", "Release", "Lines of code"]) expect(html).toContain(label);
  });

  it("starts in the grid and offers a list layout with the same facts, one row per repository", () => {
    const grid = render("Acme");
    expect(grid).toContain('aria-label="layout"');
    expect(grid).toMatch(/aria-pressed="true"[^>]*>.*?Grid/);
    const list = render("Acme", "list");
    expect(list).toContain("<table");
    expect(list).toMatch(/aria-pressed="true"[^>]*>.*?List/);
    expect(list.match(/<tbody>.*<\/tbody>/s)?.[0].match(/<tr /g)?.length).toBe(2);
    expect(list.match(/Open editor/g)?.length).toBe(2);
    for (const label of ["Repository", "Languages", "Lines", "Coverage", "Commits · 52w", "Branches", "Open PRs", "Release", "Pushed"]) expect(list).toContain(`>${label}<`);
  });
});
