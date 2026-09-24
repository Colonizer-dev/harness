// Monorepo packages in the workspace dashboard: the path → package mapping, the package rows, and
// the expanded markup (static only — the detection is pinned, since static markup runs no effects).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { OrgEntry } from "../orgs";
import type { RepoPackages, Session } from "../types";
import { OUTSIDE, UNKNOWN, packageFor, packageRows, packagesTouched } from "./monorepo";
import { OrgDashboard } from "./OrgDashboard";

const detection: RepoPackages = {
  monorepo: true,
  tool: "turbo",
  packages: [
    { name: "pwa", path: "apps/pwa" },
    { name: "sdk", path: "packages/sdk" },
    { name: "sdk-web", path: "packages/sdk/web" },
  ],
};

function colony(id: string, status: string, changed_paths?: string[], cost_usd = 1): Session {
  return { id, repo: "acme/web", org: "acme", issue: 1, issue_title: "t", status, branch: "b", base: "main", parent: null, worktree: "/w", git_admin_dir: null, sandbox: "s", mesh: null, agent: "claude-code", autopilot: false, pr_url: null, error: null, cost_usd, routed_cost_usd: null, cleaned_up: false, keep_worktree: false, created_at: "2026-09-18T09:00:00Z", updated_at: "2026-09-18T09:10:00Z", attention: null, changed_paths } as unknown as Session;
}

describe("monorepo packages", () => {
  it("maps a path to the longest package that prefixes it at a segment", () => {
    expect(packageFor("packages/sdk/web/src/x.ts", detection.packages)).toBe("packages/sdk/web");
    expect(packageFor("packages/sdk/index.ts", detection.packages)).toBe("packages/sdk");
    expect(packageFor("apps/pwa-old/x.ts", detection.packages)).toBeNull();
    expect(packageFor("README.md", detection.packages)).toBeNull();
  });

  it("counts a colony in every package it touched, with outside and not-read rows", () => {
    const touching = colony("a", "merged", ["apps/pwa/a.ts", "packages/sdk/b.ts", "turbo.json"]);
    expect([...packagesTouched(touching, detection.packages)].sort()).toEqual([OUTSIDE, "apps/pwa", "packages/sdk"].sort());
    expect([...packagesTouched(colony("b", "running"), detection.packages)]).toEqual([UNKNOWN]);

    const rows = packageRows([touching, colony("c", "failed", ["apps/pwa/c.ts"], 3), colony("d", "running")], detection);
    expect(rows.map((r) => [r.key, r.colonies, r.merged, r.failed])).toEqual([
      ["apps/pwa", 2, 1, 1],
      ["packages/sdk", 1, 1, 0],
      [OUTSIDE, 1, 1, 0],
      [UNKNOWN, 1, 0, 0],
    ]);
    expect(rows[0].spend).toBe(4);
  });

  it("shows a monorepo row with its packages, and filters to one", () => {
    const org: OrgEntry = { org: "acme", live: 1, queued: 0, total: 3, pending: 0, avatar: null };
    const sessions = [colony("a", "merged", ["apps/pwa/a.ts"]), colony("b", "merged", ["packages/sdk/b.ts"]), colony("c", "running")];
    const html = renderToStaticMarkup(
      <OrgDashboard
        org={org}
        sessions={sessions}
        history={null}
        range={30}
        compare={false}
        onBack={() => {}}
        initialPackages={{ "acme/web": detection }}
        initialRepo="acme/web"
        initialPackage="apps/pwa"
      />,
    );
    expect(html).toContain("monorepo · 3 packages");
    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain(">pwa<");
    expect(html).toContain("(files not read yet)");
    expect(html).toContain("filtered to web / pwa");
  });
});
