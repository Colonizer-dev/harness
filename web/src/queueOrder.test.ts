import { describe, expect, it } from "vitest";
import { nextUp, queueOrder, shortName } from "./queueOrder";
import type { Session } from "./types";

function queued(id: string, createdAt: string, priority?: number | null, status: Session["status"] = "queued"): Session {
  return { id, repo: "acme/api", issue: 7, status, created_at: createdAt, priority } as Session;
}

describe("queue order", () => {
  it("is oldest first when nobody has a priority", () => {
    const list = [queued("b", "2026-10-07T10:00:00Z"), queued("a", "2026-10-07T09:00:00Z")];
    expect(queueOrder(list).map((s) => s.id)).toEqual(["a", "b"]);
  });

  it("puts a higher priority first and a lower one last, whatever their age", () => {
    const list = [
      queued("old", "2026-10-07T08:00:00Z"),
      queued("low", "2026-10-07T07:00:00Z", -10),
      queued("front", "2026-10-07T11:00:00Z", 11),
    ];
    expect(queueOrder(list).map((s) => s.id)).toEqual(["front", "old", "low"]);
    expect(nextUp(list)?.id).toBe("front");
  });

  it("ignores colonies that are not queued and names the next one", () => {
    const list = [queued("run", "2026-10-07T07:00:00Z", 50, "running"), queued("q", "2026-10-07T08:00:00Z")];
    expect(nextUp(list)?.id).toBe("q");
    expect(nextUp([queued("run", "2026-10-07T07:00:00Z", 0, "running")])).toBeNull();
    expect(shortName(list[1])).toBe("api#7");
  });
});
