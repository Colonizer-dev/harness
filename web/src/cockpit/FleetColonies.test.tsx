// The fleet colony view (issue #689), rendered through react-dom/server like the other cockpit
// tests: no jsdom, assertions read the markup string. The view is presentational — rows are built by
// fleetColoniesModel.ts and handed in — so these check the table, the per-host totals, and the two link
// shapes: a local `?colony=` link and a member's own URL.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { FleetHistoryEntry, FleetHost, Session } from "../types";
import { FleetColoniesView } from "./FleetColonies";
import { fromImported, fromSession } from "./fleetColoniesModel";

const session = (o: Partial<Session> = {}): Session => ({
  id: "s1", repo: "acme/webshop", org: "acme", issue: 42, issue_title: "Checkout fails for guest users",
  status: "pr_opened", branch: "b", base: "main", worktree: "/wt", git_admin_dir: null, sandbox: "sb",
  mesh: null, agent: "claude-code", autopilot: false, pr_url: null, error: null, cost_usd: 1.25,
  cleaned_up: false, keep_worktree: false, created_at: "2026-09-18T09:00:00Z", updated_at: "2026-09-18T09:10:00Z",
  ci_state: "pending", ...o,
});

const entry: FleetHistoryEntry = {
  key: "mem_1/host-1:session-7", member_id: "mem_1", member_name: "studio-2", member_removed: false,
  id: "host-1:session-7", received_at: "2026-09-19T00:00:00Z", payloads: [],
  record: {
    id: "host-1:session-7", origin_host: "host-1", original_id: "session-7", repo: "acme/api", org: "acme",
    issue: 7, issue_title: "Rate-limit the search endpoint", status: "merged", branch: "b", pr_url: null,
    merged_at: "2026-09-19T00:00:00Z", summary: null, error: null, cost_usd: 2, agent: "claude",
    created_at: "2026-09-17T09:00:00Z", updated_at: "2026-09-19T00:00:00Z",
  },
};

const host = (id: string, name: string, health: FleetHost["health"]): FleetHost => ({
  id, name, platform: "", os: "", version: null, slots_in_use: 0, slots_ceiling: 3, queue_depth: 0,
  disk_free_bytes: null, last_heartbeat: null, health,
});

const HOSTS = [host("self", "archlinux", "online"), host("peer", "studio-2", "online"), host("quiet", "laptop-x", "unreachable")];
const colonies = [fromSession(session(), "archlinux"), fromImported(entry, new Map([["mem_1", "https://studio.example:7878"]]))];

describe("FleetColoniesView", () => {
  it("lists a row per colony with host, status, what it waits on and cost", () => {
    const html = renderToStaticMarkup(<FleetColoniesView colonies={colonies} hosts={HOSTS} />);
    expect(html).toContain("Checkout fails for guest users");
    expect(html).toContain("Rate-limit the search endpoint");
    expect(html).toContain("PR opened"); // the local row's status label
    expect(html).toContain("CI"); // pr_opened with pending checks
    expect(html).toContain("$1.25");
    expect(html).toContain("$2.00");
  });

  it("links a local colony in place and an imported one on the member's own URL", () => {
    const html = renderToStaticMarkup(<FleetColoniesView colonies={colonies} hosts={HOSTS} />);
    expect(html).toContain('href="?colony=s1"');
    expect(html).toContain('href="https://studio.example:7878/?colony=session-7"');
    expect(html).toContain('target="_blank"');
  });

  it("shows a member that has pushed nothing as zero colonies and an unmeasured cost", () => {
    const html = renderToStaticMarkup(<FleetColoniesView colonies={colonies} hosts={HOSTS} />);
    expect(html).toContain("laptop-x"); // seeded into the per-host totals even with no colonies
    expect(html).toContain("0 · —");
    expect(html).not.toContain("$0.00");
  });
});
