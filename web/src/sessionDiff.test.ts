// The pull request card's changed files (issue #611): GET /api/sessions/{id}/diff is remembered per
// colony and stamped with the session's `updated_at`, so an unchanged card never refetches but a
// colony that pushed more does, two quick opens share one request, and a long list folds after a few.
import { describe, expect, it, vi } from "vitest";

import type { Api } from "./api";
import { PR_FILES_SHOWN, currentFiles, loadSessionDiff, prFileRows } from "./sessionDiff";
import type { SessionDiffFile } from "./types";

const file = (path: string, added = 1, removed = 0): SessionDiffFile => ({ path, added, removed });

/** A stand-in for the client with only the one route these tests exercise. */
function apiWith(sessionDiff: Api["sessionDiff"]): Api {
  return { sessionDiff } as unknown as Api;
}

const diffOf = (files: SessionDiffFile[]) => ({
  id: "s1",
  repo: "acme/webshop",
  base: "main",
  files,
  added: 0,
  removed: 0,
  diff: "",
  truncated: false,
});

// Distinct ids per test: the cache is module-wide and, with no clear helper, a shared id would leak.
describe("loadSessionDiff", () => {
  it("fetches once and answers later calls from the cache at the same updated_at", async () => {
    const sessionDiff = vi.fn(async () => diffOf([file("a.ts", 3, 1)]));
    const api = apiWith(sessionDiff);
    expect(await loadSessionDiff(api, "same", "T1")).toEqual([file("a.ts", 3, 1)]);
    expect(await loadSessionDiff(api, "same", "T1")).toEqual([file("a.ts", 3, 1)]);
    expect(sessionDiff).toHaveBeenCalledTimes(1);
  });

  it("a newer updated_at refetches instead of serving the stale counts", async () => {
    let calls = 0;
    const sessionDiff = async () => {
      calls += 1;
      return diffOf([file("a.ts", calls === 1 ? 1 : 5)]);
    };
    const api = apiWith(sessionDiff);
    expect(await loadSessionDiff(api, "moved", "T1")).toEqual([file("a.ts", 1)]);
    expect(await loadSessionDiff(api, "moved", "T2")).toEqual([file("a.ts", 5)]);
    expect(calls).toBe(2);
  });

  it("two opens before the first lands share one request", async () => {
    const sessionDiff = vi.fn(async () => diffOf([file("a.ts")]));
    const api = apiWith(sessionDiff);
    const [first, second] = await Promise.all([loadSessionDiff(api, "shared", "T1"), loadSessionDiff(api, "shared", "T1")]);
    expect(first).toEqual(second);
    expect(sessionDiff).toHaveBeenCalledTimes(1);
  });

  it("in-flight sharing is per updated_at, so a newer session is its own request", async () => {
    const sessionDiff = vi.fn(async () => diffOf([file("a.ts")]));
    const api = apiWith(sessionDiff);
    await Promise.all([loadSessionDiff(api, "perkey", "T1"), loadSessionDiff(api, "perkey", "T2")]);
    expect(sessionDiff).toHaveBeenCalledTimes(2);
  });

  it("a failed fetch is not cached, so a later open tries again", async () => {
    let calls = 0;
    const sessionDiff = async () => {
      calls += 1;
      if (calls === 1) throw new Error("409");
      return diffOf([file("a.ts")]);
    };
    const api = apiWith(sessionDiff);
    await expect(loadSessionDiff(api, "retry", "T1")).rejects.toThrow("409");
    expect(await loadSessionDiff(api, "retry", "T1")).toEqual([file("a.ts")]);
    expect(calls).toBe(2);
  });
});

describe("currentFiles", () => {
  const tagged = { id: "a", files: [file("a.ts")] };

  it("hands back the files when the tagged state is for the colony being drawn", () => {
    expect(currentFiles(tagged, "a")).toEqual([file("a.ts")]);
  });

  it("returns null when the tagged state belongs to another colony, so A's files never bleed onto B", () => {
    expect(currentFiles(tagged, "b")).toBeNull();
    expect(currentFiles(tagged, null)).toBeNull();
    expect(currentFiles(null, "a")).toBeNull();
  });
});

describe("prFileRows", () => {
  const files = [file("a"), file("b"), file("c"), file("d"), file("e"), file("f"), file("g")];

  it("a short list draws in full, with nothing hidden", () => {
    const short = files.slice(0, PR_FILES_SHOWN);
    expect(prFileRows(short, false)).toEqual({ shown: short, hidden: 0 });
  });

  it("a long list folds the rows past the limit until expanded", () => {
    const { shown, hidden } = prFileRows(files, false);
    expect(shown).toHaveLength(PR_FILES_SHOWN);
    expect(hidden).toBe(2);
    expect(prFileRows(files, true)).toEqual({ shown: files, hidden: 0 });
  });
});
