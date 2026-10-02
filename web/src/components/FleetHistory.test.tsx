// Settings → Fleet → Fleet history (issue #762): rendered to static markup from a pre-seeded page
// (static markup runs no effects), the helpers pinned directly, and the filters and paging
// exercised on the mock api.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { createMockApi } from "../mock";
import type { Api } from "../api";
import { ApiContext } from "../context";
import type { FleetHistoryDetail, FleetHistoryEntry, FleetHistoryPage } from "../types";
import { FleetHistory, HistoryDrawer, HistoryFilterBar, finishedOn, totalsText } from "./FleetHistory";

const wrap = (node: React.ReactNode) => renderToStaticMarkup(<ApiContext.Provider value={{} as Api}>{node}</ApiContext.Provider>);

const entry = (n: number, member: [string, string, boolean], repo: string, status: "merged" | "failed", cost: number | null): FleetHistoryEntry => ({
  key: `${member[0]}/h:s${n}`,
  member_id: member[0],
  member_name: member[1],
  member_removed: member[2],
  id: `h:s${n}`,
  received_at: "2026-09-20T10:00:00Z",
  record: {
    id: `h:s${n}`, origin_host: "h", original_id: `s${n}`, repo, org: "acme", issue: n, issue_title: `Colony ${n}`, status, branch: `b${n}`,
    pr_url: null, merged_at: null, summary: `summary ${n}`, error: null, cost_usd: cost, agent: "claude",
    created_at: "2026-09-20T09:00:00Z", updated_at: "2026-09-20T09:30:00Z",
  },
  payloads: [{ name: "events.jsonl", sha256: "a".repeat(64), bytes: 2048 }],
});

const PAGE: FleetHistoryPage = {
  colonies: [entry(1, ["mem_1", "studio-2", false], "acme/web", "merged", 1.5), entry(2, ["mem_2", "old-laptop", true], "acme/api", "failed", null)],
  next_cursor: "mem_2/h:s2",
  stats: {
    total: { colonies: 3, merged: 2, cost_usd: 1.5 },
    members: [
      { member_id: "mem_1", name: "studio-2", removed: false, colonies: 2, merged: 2, cost_usd: 1.5 },
      { member_id: "mem_2", name: "old-laptop", removed: true, colonies: 1, merged: 0, cost_usd: null },
    ],
    repos: [{ repo: "acme/web", colonies: 3, merged: 2, cost_usd: 1.5 }],
  },
  members: [{ id: "mem_1", name: "studio-2", removed: false }, { id: "mem_2", name: "old-laptop", removed: true }],
  repos: ["acme/api", "acme/web"],
  retention_days: 90,
};

describe("FleetHistory list", () => {
  it("shows the totals at the top, each colony finished on its member, and Load more while a cursor remains", () => {
    const html = wrap(<FleetHistory initial={PAGE} />);
    expect(html).toContain("Fleet history");
    expect(html).toContain("3 colonies · 2 merged · $1.50");
    expect(html).toContain("studio-2: 2 colonies · 2 merged · $1.50");
    expect(html).toContain("old-laptop (removed): 1 colony · 0 merged");
    expect(html).toContain("Colony 1");
    expect(html).toContain("finished on studio-2");
    expect(html).toContain("finished on old-laptop (removed)");
    expect(html).toContain("Member removed");
    expect(html).toContain("PR merged");
    expect(html).toContain("kept 90 days");
    expect(html).toContain(">Load more</button>");
  });

  it("says nothing is synced yet on an empty page, and renders nothing at all when asked to hide an empty history", () => {
    const empty: FleetHistoryPage = { ...PAGE, colonies: [], next_cursor: null, members: [], stats: { total: { colonies: 0, merged: 0, cost_usd: null }, members: [], repos: [] } };
    expect(wrap(<FleetHistory initial={empty} />)).toContain("Nothing synced yet");
    expect(wrap(<FleetHistory initial={empty} hideWhenEmpty />)).toBe("");
    expect(wrap(<FleetHistory initial={PAGE} hideWhenEmpty />)).toContain("Fleet history");
  });

  it("offers every member (removed ones marked), repository and finished status as filters", () => {
    const html = renderToStaticMarkup(<HistoryFilterBar page={PAGE} filters={{ member: "mem_2" }} onChange={() => {}} />);
    expect(html).toContain('aria-label="Member"');
    expect(html).toContain("old-laptop (removed)");
    expect(html).toMatch(/<option value="mem_2" selected="">/);
    expect(html).toContain('<option value="acme/api">acme/api</option>');
    expect(html).toContain('<option value="merged">PR merged</option>');
    expect(html).toContain('type="date"');
  });

  it("pins the totals and member wording", () => {
    expect(totalsText({ colonies: 1, merged: 0, cost_usd: null })).toBe("1 colony · 0 merged");
    expect(totalsText({ colonies: 4, merged: 1, cost_usd: 0 })).toBe("4 colonies · 1 merged · $0.00");
    expect(finishedOn({ member_name: "a", member_removed: false })).toBe("finished on a");
  });
});

describe("FleetHistory drawer", () => {
  const detail: FleetHistoryDetail = {
    ...entry(1, ["mem_2", "old-laptop", true], "acme/web", "merged", 2),
    logs: [
      { name: "events.jsonl", sha256: "a".repeat(64), bytes: 2048, omitted: false, stored: true },
      { name: "gateway.jsonl", sha256: "", bytes: 64 * 1024 ** 2, omitted: true, stored: false },
    ],
  };

  it("shows the record, the removed member, each log with the unsyncable one disabled, and the opened log", () => {
    const html = renderToStaticMarkup(<HistoryDrawer detail={detail} log={'{"type":"status"}'} logName="events.jsonl" onLog={() => {}} onClose={() => {}} />);
    expect(html).toContain("old-laptop (removed from the fleet)");
    expect(html).toContain("acme/web");
    expect(html).toContain("$2.00");
    expect(html).toContain("summary 1");
    expect(html).toContain("events.jsonl · 2K");
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*title="too large to sync"/);
    expect(html).toContain('aria-label="events.jsonl contents"');
    expect(html).toContain("{&quot;type&quot;:&quot;status&quot;}");
    expect(html).toContain(">Close</button>");
  });
});

describe("FleetHistory on the mock api", () => {
  it("filters, pages by cursor, and reads one colony's record and log", async () => {
    const api = createMockApi();
    const all = await api.fleetHistory();
    expect(all.colonies.length).toBe(4);
    expect(all.members.some((m) => m.removed)).toBe(true);
    const web = await api.fleetHistory({ repo: "acme/web" });
    expect(web.colonies.every((c) => c.record.repo === "acme/web")).toBe(true);
    expect(web.stats.total.colonies).toBe(web.colonies.length);
    const merged = await api.fleetHistory({ status: "merged" });
    expect(merged.colonies.every((c) => c.record.status === "merged")).toBe(true);

    const first = await api.fleetHistory({ limit: 2 });
    expect(first.colonies.length).toBe(2);
    const second = await api.fleetHistory({ limit: 2, cursor: first.next_cursor! });
    expect(second.next_cursor).toBeNull();
    expect(new Set([...first.colonies, ...second.colonies].map((c) => c.key)).size).toBe(4);

    const one = all.colonies[0];
    const detail = await api.fleetHistoryEntry(one.member_id, one.id);
    expect(detail.logs[0].stored).toBe(true);
    expect(await api.fleetHistoryLog(one.member_id, one.id, "events.jsonl")).toContain('"status"');
    await expect(api.fleetHistoryEntry(one.member_id, "nope")).rejects.toThrow();
  });
});
