// History's timeline: log lines plus the colony list for what the log is too young to know. The
// duplicates the old page showed came from dating each colony by `updated_at`, which every
// housekeeping write moves; these pin that a logged outcome stays at its own time, once.
import { describe, expect, it } from "vitest";

import type { ActivityEntry, Session } from "../types";
import { buildTimeline, collapse, groupRepos, groupSpan, groupText, matchesHistory, sentence, summarize, toneFor, HISTORY_ALL } from "./history";
import { mergeEntries, targetOf } from "./HistoryView";

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "a",
    repo: "acme/web",
    org: "acme",
    issue: 7,
    issue_title: "Fix checkout",
    status: "running",
    branch: "b",
    base: "main",
    worktree: "",
    git_admin_dir: null,
    sandbox: "s",
    mesh: null,
    agent: "claude-code",
    autopilot: true,
    pr_url: null,
    error: null,
    cost_usd: null,
    cleaned_up: false,
    keep_worktree: false,
    created_at: "2026-09-24T08:00:00Z",
    updated_at: "2026-09-24T08:00:00Z",
    ...overrides,
  } as Session;
}

let seq = 0;
function entry(overrides: Partial<ActivityEntry>): ActivityEntry {
  seq += 1;
  return { seq, ts: "2026-09-24T09:00:00Z", kind: "colony.launch", actor: "you", via: "cockpit", org: "acme", repo: "acme/web", issue: 7, colony: "a", ...overrides };
}

describe("buildTimeline", () => {
  it("keeps a logged outcome at its own time even after a sweep moved the colony's updated_at", () => {
    // The report: a reclaim at 16:15 rewrote colonies that finished days earlier, and the page
    // showed every one of them as finishing at 16:15.
    const done = session({ id: "a", status: "no_changes", cleaned_up: true, updated_at: "2026-09-24T16:15:00Z" });
    const logged = entry({ kind: "outcome.no_changes", actor: "colony", ts: "2026-09-21T13:34:00Z", colony: "a" });
    const items = buildTimeline([logged], [done], true);
    expect(items).toHaveLength(1);
    expect(items[0].at).toBe("2026-09-21T13:34:00Z");
    expect(items[0].approximate).toBe(false);
  });

  it("reads an outcome the log predates off the colony list, once, marked approximate", () => {
    const items = buildTimeline([], [session({ id: "a", status: "stopped", updated_at: "2026-09-24T16:16:00Z" })], true);
    expect(items.map((i) => [i.kind, i.approximate])).toEqual([["outcome.stopped", true]]);
    // A merge has GitHub's own time, so that one is exact.
    const merged = buildTimeline([], [session({ id: "m", status: "merged", merged_at: "2026-09-20T09:44:00Z", updated_at: "2026-09-24T16:14:00Z" })], true);
    expect(merged[0]).toMatchObject({ at: "2026-09-20T09:44:00Z", approximate: false });
  });

  it("leaves a colony older than the loaded pages alone while older pages are unread", () => {
    const old = session({ id: "o", status: "failed", updated_at: "2026-09-01T00:00:00Z" });
    const recent = entry({ ts: "2026-09-24T09:00:00Z", colony: "x" });
    expect(buildTimeline([recent], [old], false).map((i) => i.colonyId)).toEqual(["x"]);
    expect(buildTimeline([recent], [old], true).map((i) => i.colonyId)).toEqual(["x", "o"]);
  });

  it("draws a log line once, however many pages brought it", () => {
    const line = entry({ kind: "outcome.merged", actor: "colony" });
    expect(buildTimeline(mergeEntries([line], [line]), [], true)).toHaveLength(1);
  });

  it("tells three colonies on the same issue apart and folds the run into one row", () => {
    const at = (s: number) => `2026-09-24T16:16:${String(s).padStart(2, "0")}Z`;
    const stops = ["ca04bc02", "d4ed94b7", "c1c9215b", "bc4f8f51", "34418674"].map((id, i) =>
      entry({ kind: "outcome.stopped", actor: "you", colony: id, ts: at(15 - i), repo: i % 2 ? "acme/api" : "acme/web", issue: i % 2 ? 4505 : 3473 }),
    );
    const items = buildTimeline(stops, [], true);
    expect(new Set(items.map((i) => i.colonyId)).size).toBe(5);
    const rows = collapse(items, new Date("2026-09-24T18:00:00Z"));
    expect(rows).toHaveLength(1);
    const group = rows[0];
    expect(group.type).toBe("group");
    if (group.type !== "group") return;
    expect(groupText(group)).toBe("You stopped 5 colonies");
    expect(groupRepos(group.items)).toBe("web ×3, api ×2");
    expect(groupSpan(group.items)).toMatch(/–|^\d\d:\d\d$/);
  });

  it("does not fold a short run, or loud rows", () => {
    const now = new Date("2026-09-24T18:00:00Z");
    const two = [entry({ kind: "outcome.no_changes", actor: "colony", colony: "p" }), entry({ kind: "outcome.no_changes", actor: "colony", colony: "q" })];
    expect(collapse(buildTimeline(two, [], true), now).every((r) => r.type === "item")).toBe(true);
    const failures = [1, 2, 3, 4].map((n) => entry({ kind: "outcome.failed", actor: "colony", colony: `f${n}` }));
    expect(collapse(buildTimeline(failures, [], true), now)).toHaveLength(4);
  });
});

describe("filters and words", () => {
  const items = buildTimeline(
    [
      entry({ kind: "outcome.failed", actor: "colony", colony: "f", detail: "tests failed" }),
      entry({ kind: "settings.save", actor: "you", via: "api", target: "provider openrouter", colony: null, repo: null, issue: null, section: "providers" }),
      entry({ kind: "outcome.question", actor: "colony", colony: "q", repo: "acme/api", issue: 3 }),
      entry({ kind: "colony.launch", colony: "l" }),
    ],
    [],
    true,
  );

  it("filters by kind, actor, repository and text", () => {
    const pick = (f: Partial<typeof HISTORY_ALL>, q = "") => items.filter((i) => matchesHistory(i, q, { ...HISTORY_ALL, ...f })).map((i) => i.kind);
    expect(pick({ kind: "failures" })).toEqual(["outcome.failed"]);
    expect(pick({ kind: "questions" })).toEqual(["outcome.question"]);
    expect(pick({ kind: "launches" })).toEqual(["colony.launch"]);
    expect(pick({ kind: "yours" }).sort()).toEqual(["colony.launch", "settings.save"]);
    expect(pick({ actor: "api" })).toEqual(["settings.save"]);
    expect(pick({ repo: "acme/api" })).toEqual(["outcome.question"]);
    expect(pick({}, "openrouter")).toEqual(["settings.save"]);
  });

  it("says who did it", () => {
    expect(sentence({ kind: "outcome.stopped", actor: "you", repo: "acme/web", issue: 3 })).toBe("You stopped web#3");
    expect(sentence({ kind: "outcome.stopped", actor: "colony", repo: "acme/web", issue: 3 })).toBe("web#3 stopped");
    expect(sentence({ kind: "settings.save", actor: "you", target: "secret provider-keys:x" })).toBe("You saved the secret provider-keys:x");
    expect(sentence({ kind: "loop.pause", actor: "you", target: "Nightly" })).toBe("You paused the loop “Nightly”");
    expect(sentence({ kind: "outcome.question", actor: "colony", repo: "acme/web", issue: 3 }, true)).toBe("web#3 is waiting on your answer");
    expect(sentence({ kind: "colonize.issue", actor: "you", repo: "acme/web", issue: 100 })).toBe("You created web#100 from Colonize");
    expect(sentence({ kind: "colonize.colony", actor: "you", repo: "acme/web", issue: 100 })).toBe("You dispatched a colony on web#100 from Colonize");
    expect(toneFor("colonize.colony")).toBe("launch");
    expect(sentence({ kind: "from.the.future", actor: "you" })).toContain("from.the.future");
  });

  it("counts what matters", () => {
    expect(summarize(items)).toEqual({ prs: 0, merged: 0, failed: 1, questions: 1, yours: 2 });
  });

  it("links a row to its colony while it exists, else to where its target lives", () => {
    const failed = items.find((i) => i.kind === "outcome.failed")!;
    const saved = items.find((i) => i.kind === "settings.save")!;
    expect(targetOf(failed, new Set(["f"]))).toEqual({ kind: "colony", id: "f" });
    expect(targetOf(failed, new Set())).toBeNull();
    expect(targetOf(saved, new Set())).toEqual({ kind: "section", section: "providers" });
  });
});
