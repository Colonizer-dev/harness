// The Queues filters' round trip through the address (issue #1127), pinned without a browser.
import { describe, expect, it } from "vitest";

import type { QueuesFilters } from "./types";
import { queuesFiltersFromSearch, queuesSearchFromFilters } from "./url";

describe("queues url state", () => {
  it("round-trips every filter", () => {
    const filters: QueuesFilters = { host: "build-box", reason: "repo_pr_rate_limit", repo: "acme/webshop", agent: "claude-code", q: "checkout", group: "reason" };
    expect(queuesFiltersFromSearch(queuesSearchFromFilters(filters))).toEqual(filters);
  });

  it("drops empty values, so an unfiltered view leaves the address clean", () => {
    expect(queuesSearchFromFilters({})).toBe("");
    expect(queuesSearchFromFilters({ host: "", q: "  ", group: "none" })).toBe("");
    expect(queuesFiltersFromSearch("")).toEqual({ group: "none" });
  });

  it("reads an unknown or bad group as none", () => {
    expect(queuesFiltersFromSearch("?group=everything").group).toBe("none");
    expect(queuesFiltersFromSearch("?group=").group).toBe("none");
    expect(queuesSearchFromFilters({ group: "host" })).toBe("?group=host");
  });

  it("parses the shape the view writes, question mark or not", () => {
    const fromPlain = queuesFiltersFromSearch("host=gpu-lab&q=rotate");
    expect(fromPlain).toEqual({ host: "gpu-lab", q: "rotate", group: "none" });
  });
});
