import { describe, expect, it } from "vitest";
import type { Session } from "../types";
import { ciPassRate, cycleTimes, deliveryKpis, formatSpan, leadTimes, median } from "./delivery";

const H = 3_600_000;
const base = Date.parse("2026-09-20T00:00:00Z");
const at = (h: number) => new Date(base + h * H).toISOString();
const s = (over: Partial<Session>): Session => ({ id: Math.random().toString(36).slice(2), repo: "acme/app", status: "merged", created_at: at(0), updated_at: at(0), ...over }) as Session;

describe("delivery KPIs", () => {
  it("median handles odd, even and empty", () => {
    expect(median([3, 1, 2])).toBe(2);
    expect(median([4, 1, 3, 2])).toBe(2.5);
    expect(median([])).toBeNull();
  });

  it("formats spans at the unit that reads best", () => {
    expect(formatSpan(20 * 60_000)).toBe("20m");
    expect(formatSpan(5.25 * H)).toBe("5.3h");
    expect(formatSpan(72 * H)).toBe("3.0d");
  });

  it("reads lead and cycle time from merged colonies only, cycle only with a PR time", () => {
    const list = [
      s({ created_at: at(0), pr_opened_at: at(2), merged_at: at(5) }),
      s({ created_at: at(0), merged_at: at(10) }),
      s({ status: "pr_opened", created_at: at(0), pr_opened_at: at(1) }),
    ];
    expect(leadTimes(list)).toEqual([5 * H, 10 * H]);
    expect(cycleTimes(list)).toEqual([3 * H]);
  });

  it("CI pass rate counts settled verdicts only", () => {
    const list = [
      s({ ci_state: "success", merged_at: at(1) }),
      s({ ci_state: "failure", merged_at: at(1) }),
      s({ ci_state: "success", merged_at: at(1) }),
      s({ ci_state: "pending", merged_at: at(1) }),
      s({ ci_state: "no_checks", merged_at: at(1) }),
    ];
    expect(ciPassRate(list)).toEqual({ rate: 2 / 3, passed: 2, settled: 3 });
  });

  it("builds tiles with real values, and honest empty notes without samples", () => {
    const win = { from: base, to: base + 48 * H };
    const [lead, cycle, ci] = deliveryKpis([s({ created_at: at(0), pr_opened_at: at(1), merged_at: at(3), ci_state: "success" })], win, null, [], false);
    expect(lead.value).toBe("3.0h");
    expect(cycle.value).toBe("2.0h");
    expect(ci.value).toBe("100%");
    expect(ci.unmeasured).toBeUndefined();
    const empty = deliveryKpis([], win, null, [], false);
    expect(empty.map((k) => k.value)).toEqual(["—", "—", "—"]);
    expect(empty[1].emptyNote).toBe("no merged colonies with PR times yet");
  });
});
