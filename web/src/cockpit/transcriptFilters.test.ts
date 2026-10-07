import { describe, expect, it } from "vitest";

import {
  EMPTY_TRANSCRIPT_FILTERS,
  activeFilters,
  clearFilters,
  datePreset,
  distinctValues,
  matchValues,
  parseTranscriptFilters,
  searchQuery,
  serializeTranscriptFilters,
} from "./transcriptFilters";

const STATUSES = ["merged", "failed", "stopped"];

describe("URL round-trip", () => {
  it("serializes nothing for empty filters and parses it back", () => {
    expect(serializeTranscriptFilters(EMPTY_TRANSCRIPT_FILTERS)).toBe("");
    expect(parseTranscriptFilters("", STATUSES)).toEqual(EMPTY_TRANSCRIPT_FILTERS);
  });

  it("round-trips every field, including awkward text", () => {
    const f = { q: "a&b = c?", repo: "acme/web shop", agent: "claude-code", status: "failed", since: "2026-10-01", until: "2026-10-07" };
    expect(parseTranscriptFilters(serializeTranscriptFilters(f), STATUSES)).toEqual(f);
  });

  it("keeps parameters that are not ours and removes ours when blank", () => {
    const s = serializeTranscriptFilters({ ...EMPTY_TRANSCRIPT_FILTERS, repo: "a/b" }, "?mock=1&tagent=old");
    expect(s).toBe("?mock=1&trepo=a%2Fb");
  });

  it("drops an unknown status and malformed dates", () => {
    expect(parseTranscriptFilters("?tstatus=bogus&tsince=yesterday&tuntil=2026-1-1", STATUSES)).toEqual(EMPTY_TRANSCRIPT_FILTERS);
  });
});

describe("searchQuery", () => {
  it("trims, drops blanks and adds the workspace", () => {
    expect(searchQuery({ ...EMPTY_TRANSCRIPT_FILTERS, q: " x " }, "acme")).toEqual({
      q: "x", repo: undefined, org: "acme", agent: undefined, status: undefined, since: undefined, until: undefined,
    });
    expect(searchQuery(EMPTY_TRANSCRIPT_FILTERS, null).org).toBeUndefined();
  });
});

describe("values", () => {
  it("lists distinct sorted values and filters them", () => {
    const rows = [{ r: "b/x" }, { r: "a/y" }, { r: "b/x" }, { r: "" }];
    const values = distinctValues(rows, (x) => x.r);
    expect(values).toEqual(["a/y", "b/x"]);
    expect(matchValues(values, " B/")).toEqual(["b/x"]);
    expect(matchValues(values, "")).toEqual(values);
  });
});

describe("datePreset", () => {
  const now = new Date(2026, 9, 6);
  it("spans today, 7 and 30 days ending today", () => {
    expect(datePreset("today", now)).toEqual({ since: "2026-10-06", until: "2026-10-06" });
    expect(datePreset("7d", now)).toEqual({ since: "2026-09-30", until: "2026-10-06" });
    expect(datePreset("30d", now)).toEqual({ since: "2026-09-07", until: "2026-10-06" });
  });
});

describe("active filters", () => {
  it("lists a removable chip per active filter and clears them", () => {
    const f = { q: "bug", repo: "a/b", agent: "", status: "failed", since: "2026-10-01", until: "" };
    const chips = activeFilters(f, (s) => s.toUpperCase());
    expect(chips.map((c) => c.label)).toEqual(["Repository: a/b", "Status: FAILED", "Date: from 2026-10-01"]);
    expect({ ...f, ...chips[2].clear }.since).toBe("");
    expect(clearFilters(f)).toEqual({ ...EMPTY_TRANSCRIPT_FILTERS, q: "bug" });
  });
});
