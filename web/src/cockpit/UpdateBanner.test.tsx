// The update banner (issue #1097): a release flagging a critical fix shows its line and the colonies
// its probe found here; after the update, the affected colonies left on the previous version are
// offered a restart; a routine release or a routine lag shows nothing. Rendered to static markup:
// the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import type { Api } from "../api";
import type { BehindColony, UpdateNotice, UpdateStatus } from "../types";
import { UpdateBanner, behindText, noticeText, restartOnNewVersion } from "./UpdateBanner";

const base: UpdateStatus = {
  enabled: true,
  blocked_by: null,
  installed: { version: "v0.2.6", commit: null, dirty: false, built_at: "2026-10-01T00:00:00Z", release: "v0.2.6" },
  latest: { version: "v0.2.7", url: "https://example.invalid", notes: "", published_at: null },
  available: true,
  last_checked: null,
  error: null,
  can_apply: { ok: true, reason: null },
  apply: { phase: "idle", version: null, started_at: null, error: null, log: "", colonies: [], backup: null },
  notices: [],
  behind: [],
  restarts: { restarting: [], failed: {} },
  switch_to_releases: null,
};

const notice = (overrides: Partial<UpdateNotice> = {}): UpdateNotice => ({
  version: "v0.2.7",
  severity: "critical",
  line: "Fixes colonies failing with UND_ERR_SOCKET (sandbox credential scanner)",
  probe: "msb-body-secret-violation",
  issue: 1096,
  affected: { count: 4, colonies: ["a", "b", "c", "d"] },
  ...overrides,
});

const left = (id: string, affected: boolean): BehindColony => ({
  id,
  repo: "acme/web",
  status: "running",
  slot: "/app-a",
  affected_by: affected ? ["Fixes colonies failing with UND_ERR_SOCKET (sandbox credential scanner)"] : [],
});

const markup = (update: UpdateStatus | null) =>
  renderToStaticMarkup(<UpdateBanner update={update} onOpenUpdates={() => {}} onRestart={() => {}} />);

describe("noticeText", () => {
  it("names the release, its line and the colonies affected here", () => {
    expect(noticeText({ ...base, notices: [notice()] })).toBe(
      "Update to v0.2.7: Fixes colonies failing with UND_ERR_SOCKET (sandbox credential scanner). 4 of your colonies are affected.",
    );
  });

  it("counts a colony two notices matched once, and says when the probe found none", () => {
    const two = [notice({ affected: { count: 1, colonies: ["a"] } }), notice({ line: "Fixes a stuck merge train.", severity: "fixes-running", affected: { count: 1, colonies: ["a"] } })];
    expect(noticeText({ ...base, notices: two })).toBe(
      "Update to v0.2.7: Fixes colonies failing with UND_ERR_SOCKET (sandbox credential scanner); Fixes a stuck merge train. 1 of your colonies is affected.",
    );
    expect(noticeText({ ...base, notices: [notice({ affected: { count: 0, colonies: [] } })] })).toMatch(/None of your colonies is affected right now\.$/);
    // No probe, or one this build lacks: no count either way.
    expect(noticeText({ ...base, notices: [notice({ affected: null })] })).toMatch(/scanner\)\.$/);
  });

  it("is nothing without notices, or on an older mothership", () => {
    expect(noticeText(base)).toBeNull();
    expect(noticeText({ ...base, notices: undefined })).toBeNull();
    expect(noticeText(null)).toBeNull();
  });
});

describe("behindText", () => {
  it("counts only the colonies a probe matched", () => {
    expect(behindText({ ...base, behind: [left("a", true), left("b", false)] })).toBe(
      "1 colony still runs on the previous version and hits a bug it fixes: Fixes colonies failing with UND_ERR_SOCKET (sandbox credential scanner). A restart keeps the worktree and the conversation.",
    );
    expect(behindText({ ...base, behind: [left("b", false)] })).toBeNull();
  });
});

describe("UpdateBanner", () => {
  it("shows a critical notice as an alert with the way to the update", () => {
    const html = markup({ ...base, notices: [notice()] });
    expect(html).toContain('role="alert"');
    expect(html).toContain("4 of your colonies are affected.");
    expect(html).toContain("Review and update");
    expect(html).toContain("border-err");
  });

  it("asks why not, rather than offering an update, where it cannot be applied", () => {
    const html = markup({ ...base, notices: [notice()], can_apply: { ok: false, reason: "development build" } });
    expect(html).toContain("Why not here");
  });

  it("offers the restart for affected colonies left behind, and says when it is under way", () => {
    const after = { ...base, available: false, behind: [left("a", true)] };
    expect(markup(after)).toContain("Restart on the new version");
    expect(markup({ ...after, restarts: { restarting: ["a"], failed: {} } })).toContain("Restarting…");
  });

  it("renders nothing for a routine release or a routine lag", () => {
    expect(markup(base)).toBe("");
    expect(markup({ ...base, behind: [left("b", false)] })).toBe("");
    expect(markup(null)).toBe("");
  });
});

describe("restartOnNewVersion", () => {
  it("posts the ids, reports what started and re-reads the status", async () => {
    const restart = vi.fn(async () => ({ restarting: ["a"], skipped: [{ id: "z", reason: "not running on a previous version" }] }));
    const update = vi.fn(async () => base);
    const api = { restartOnNewVersion: restart, update } as unknown as Api;
    const said: string[] = [];
    const changed = vi.fn();
    await restartOnNewVersion(api, { ids: ["a", "z"] }, (m) => said.push(m), changed);
    expect(restart).toHaveBeenCalledWith({ ids: ["a", "z"] });
    expect(said).toEqual(["Restarting 1 colony on the new version (1 skipped: not running on a previous version)."]);
    expect(changed).toHaveBeenCalledWith(base);
  });

  it("reports a refusal as an error", async () => {
    const api = {
      restartOnNewVersion: async () => {
        throw new Error("an update is being applied");
      },
      update: async () => base,
    } as unknown as Api;
    const said: [string, string | undefined][] = [];
    await restartOnNewVersion(api, { all: true }, (m, tone) => said.push([m, tone]));
    expect(said).toEqual([["an update is being applied", "error"]]);
  });
});
