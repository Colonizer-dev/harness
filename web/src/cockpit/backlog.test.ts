import { describe, expect, it } from "vitest";

import { backlogBadge, backlogTooltip, clockTime } from "./backlog";
import type { Backlog } from "../features/host/types";

// Built from local-time parts, so the expected HH:MM holds in any time zone.
const at = (h: number, m: number) => new Date(2026, 9, 7, h, m, 30);

const backlog = (as_of: Date): Backlog => ({
  issues: 21,
  repos: 5,
  as_of: as_of.toISOString(),
  by_org: { Acme: { issues: 17, repos: 3 }, tools: { issues: 4, repos: 2 } },
});

describe("the frontier badge", () => {
  it("zero-pads the clock", () => {
    expect(clockTime(at(9, 5))).toBe("09:05");
    expect(clockTime(at(23, 41))).toBe("23:41");
  });

  it("says what the count covers and when it was taken", () => {
    expect(backlogTooltip(21, 5, at(14, 7))).toBe("21 open issues in 5 repositories you colonize · as of 14:07");
  });

  it("uses the singular for one", () => {
    expect(backlogTooltip(1, 1, at(8, 0))).toBe("1 open issue in 1 repository you colonize · as of 08:00");
  });

  it("drops the time rather than printing NaN when as_of is unreadable", () => {
    expect(backlogTooltip(2, 1, new Date("nope"))).toBe("2 open issues in 1 repository you colonize");
  });

  it("shows the install's totals in the all-workspaces view", () => {
    expect(backlogBadge(backlog(at(14, 7)), null)).toEqual({
      count: 21,
      title: "21 open issues in 5 repositories you colonize · as of 14:07",
    });
  });

  it("shows the selected workspace's own numbers, matching the org case-insensitively", () => {
    expect(backlogBadge(backlog(at(14, 7)), "acme")).toEqual({
      count: 17,
      title: "17 open issues in 3 repositories you colonize · as of 14:07",
    });
  });

  it("counts a workspace the mothership did not count as zero", () => {
    expect(backlogBadge(backlog(at(14, 7)), "elsewhere").count).toBe(0);
  });

  it("has no number until the mothership has counted", () => {
    expect(backlogBadge(null, null).count).toBeNull();
    expect(backlogBadge(undefined, "acme").count).toBeNull();
  });
});
