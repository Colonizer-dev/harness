// The nest dashboard: the panel beside the nest computes its whole reading — bucket counts, spend,
// the needs-you queue, the last moves — from the scoped colony list alone. Rendered through
// react-dom/server, because this codebase keeps tests off jsdom.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { Session } from "../types";
import { NestDashboard, nestDashboard } from "./NestDashboard";
import { session } from "./testFixtures";

const OLDER = "2026-09-20T08:00:00Z";
const NEWER = "2026-09-20T09:00:00Z";

const working = session({ id: "w1", status: "running", cost_usd: 1, updated_at: NEWER, last_activity_at: NEWER });
const asking = session({
  id: "q1",
  status: "waiting_for_answer",
  cost_usd: 0.5,
  updated_at: OLDER,
  attention: { reason: "waiting_for_answer", since: OLDER, nudges: 0 },
});
const queuedOne = session({ id: "qu1", status: "queued" });
const returnedOne = session({ id: "r1", status: "pr_opened" });
const stoppedOne = session({ id: "s9", status: "stopped", cost_usd: 0.25 });

// Handed over in the sidebar's order, not the buckets' — the summary must not care.
const nest = [working, asking, queuedOne, returnedOne, stoppedOne];

describe("nestDashboard", () => {
  it("counts the state buckets the overview names, plus failed/stopped, and sums the spend", () => {
    const summary = nestDashboard(nest);
    // waiting_for_answer is live and needs you: both tiles read it, like the cockpit's own counts.
    expect(summary.live).toBe(2);
    expect(summary.needYou).toBe(1);
    expect(summary.queued).toBe(1);
    expect(summary.returned).toBe(1);
    expect(summary.ended).toBe(1);
    expect(summary.total).toBe(5);
    expect(summary.spend).toBeCloseTo(1.75);
  });

  it("lists the needs-you colonies longest-waiting first, by the flag's own since", () => {
    const late = session({
      id: "late",
      status: "idle",
      attention: { reason: "stalled", since: "2026-09-20T10:00:00Z", nudges: 2 },
    });
    const early = session({
      id: "early",
      status: "waiting_for_answer",
      attention: { reason: "waiting_for_answer", since: "2026-09-20T07:00:00Z", nudges: 0 },
    });
    expect(nestDashboard([late, early]).attention.map((s) => s.id)).toEqual(["early", "late"]);
    // A terminal colony never needs anyone, even with a stale flag left over.
    expect(nestDashboard([session({ status: "stopped", attention: { reason: "stalled", since: OLDER, nudges: 1 } })]).attention).toEqual([]);
  });

  it("reads recent activity by last_activity_at, falling back to updated_at, newest first", () => {
    const stale = session({ id: "stale", updated_at: "2026-09-01T00:00:00Z" });
    const moved = session({ id: "moved", updated_at: "2026-09-21T00:00:00Z" });
    const active = session({ id: "active", updated_at: "2026-09-01T00:00:00Z", last_activity_at: "2026-09-22T00:00:00Z" });
    expect(nestDashboard([stale, moved, active]).recent.map((s) => s.id)).toEqual(["active", "moved", "stale"]);
    expect(nestDashboard([...nest, ...nest, ...nest]).recent).toHaveLength(5);
  });
});

describe("NestDashboard", () => {
  const markup = (sessions: Session[], org: string | null = "acme") =>
    renderToStaticMarkup(
      <NestDashboard org={org} avatar={null} sessions={sessions} maxParallel={5} onSelect={() => {}} onHide={() => {}} />,
    );

  it("names the workspace and shows the tiles, the needs-you reason and the spend", () => {
    const out = markup(nest);
    expect(out).toContain("acme");
    expect(out).toContain("2 / 5");
    expect(out).toContain("$1.75");
    expect(out).toContain("acme/webshop #42");
    expect(out).toContain("Waiting for your answer");
  });

  it("titles the unfiltered scope All workspaces, mirroring whatever the nest shows", () => {
    expect(markup(nest, null)).toContain("All workspaces");
  });

  it("says something instead of rendering an empty panel when the workspace has no colonies", () => {
    const out = markup([]);
    expect(out).toContain("no colonies here yet");
    expect(out).not.toContain("RECENT");
  });
});
