import { describe, expect, it } from "vitest";
import { CLEANUP_LOOP, PREVIEW } from "./testFixtures";
import { DEFAULT_DISK_CLEANUP_SETTINGS, describeDiskCleanup, diskCleanupBody, isDiskCleanup, parseExtraPaths, reportSummary, settingsOf, toggleAction } from "./diskCleanup";

describe("disk cleanup loop", () => {
  it("is recognised by kind or its fixed id", () => {
    expect(isDiskCleanup(CLEANUP_LOOP)).toBe(true);
    expect(isDiskCleanup({ id: "loop_a", kind: "colony" })).toBe(false);
  });

  it("defaults to hourly, a 15% trigger, and the host category off", () => {
    expect(describeDiskCleanup(CLEANUP_LOOP)).toBe("Built in · every hour · early under 15% free · build output, worktrees, microvms");
    const s = settingsOf(CLEANUP_LOOP);
    expect(s.host_paths).toBe(false);
    expect(s.archives).toBe(false);
    expect(s.extra_paths).toEqual([]);
  });

  it("previews before the first enable, and only then", () => {
    expect(toggleAction(CLEANUP_LOOP, true)).toBe("preview");
    expect(toggleAction(CLEANUP_LOOP, false)).toBe("save");
    const seen = { ...CLEANUP_LOOP, disk_cleanup: { ...CLEANUP_LOOP.disk_cleanup!, previewed_at: "2026-09-30T09:00:00Z" } };
    expect(toggleAction(seen, true)).toBe("save");
  });

  it("builds the PUT body with the switch, cadence and every setting", () => {
    const body = diskCleanupBody(CLEANUP_LOOP, { enabled: true });
    expect(body).toMatchObject({ kind: "disk_cleanup", enabled: true, cadence: { every: "interval", minutes: 60 } });
    expect(body.disk_cleanup).toEqual(DEFAULT_DISK_CLEANUP_SETTINGS);
    expect(diskCleanupBody(CLEANUP_LOOP, { minutes: 15 }).cadence).toEqual({ every: "interval", minutes: 15 });
  });

  it("summarises a report and reads the extra-paths box", () => {
    expect(reportSummary(PREVIEW)).toBe("would free 3G · 1 item");
    expect(reportSummary({ ...PREVIEW, dry_run: false })).toBe("freed 3G · 1 item");
    expect(reportSummary({ ...PREVIEW, categories: [] })).toBe("nothing to clean");
    expect(parseExtraPaths(" /home/me/code \n\n/srv/build\n")).toEqual(["/home/me/code", "/srv/build"]);
  });
});
