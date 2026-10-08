// The disk-cleanup card and its dry-run preview, rendered to static markup (the test environment has
// no DOM): the switch, the preview's paths and sizes, what it keeps and why, and the attention item.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Api } from "../api";
import { ApiContext } from "../context";
import { DiskCleanupCard, DiskCleanupReportView, DiskCleanupSettingsForm } from "./DiskCleanupLoop";
import { CLEANUP_LOOP, PREVIEW } from "./testFixtures";
import { DEFAULT_DISK_CLEANUP_SETTINGS } from "./diskCleanup";

const row = (loop = CLEANUP_LOOP) =>
  renderToStaticMarkup(
    <ApiContext.Provider value={{} as Api}>
      <DiskCleanupCard loop={loop} onChanged={() => {}} />
    </ApiContext.Provider>,
  );

describe("DiskCleanupLoop", () => {
  it("shows the built-in loop off, with its switch and its place among the built-in loops", () => {
    const out = row();
    expect(out).toContain("Disk cleanup");
    expect(out).toContain("Built-in");
    expect(out).toContain('role="switch"');
    expect(out).toContain('aria-checked="false"');
    expect(out).toContain("Off · every hour");
    expect(out).toContain("Not run yet");
  });

  it("shows the switch on, the last run and a standing attention item", () => {
    const out = row({
      ...CLEANUP_LOOP,
      enabled: true,
      next_run_at: "2026-09-30T11:00:00Z",
      disk_cleanup: { ...CLEANUP_LOOP.disk_cleanup!, history: [{ ...PREVIEW, dry_run: false }], attention: "Disk still 94% full after cleanup — 40G in live colonies" },
    });
    expect(out).toContain('aria-checked="true"');
    expect(out).toContain("Disk still 94% full after cleanup — 40G in live colonies");
  });

  it("previews what a run would remove, with sizes, and says nothing was removed", () => {
    const out = renderToStaticMarkup(<DiskCleanupReportView report={PREVIEW} />);
    expect(out).toContain("would free 3G");
    expect(out).toContain("Nothing has been removed.");
    expect(out).toContain("/data/worktrees/acme/web/abc/target");
    expect(out).toContain("3G");
    expect(out).toContain("kept: /data/worktrees/acme/web/def (unpushed commits)");
    expect(out).toContain("Host build dirs");
    expect(out).toContain(">off<");
  });

  it("offers every category's switch, and the extra paths only with the host category on", () => {
    const form = (settings = DEFAULT_DISK_CLEANUP_SETTINGS) => renderToStaticMarkup(<DiskCleanupSettingsForm settings={settings} minutes={60} onChange={() => {}} onMinutes={() => {}} />);
    const out = form();
    for (const label of ["Build output", "Worktrees", "MicroVMs", "Session archives", "Host build dirs"]) expect(out).toContain(label);
    expect(out).toContain('value="60"');
    expect(out).toContain('value="15"');
    expect(out).not.toContain("Directories you build in");
    expect(form({ ...DEFAULT_DISK_CLEANUP_SETTINGS, host_paths: true })).toContain("Directories you build in");
  });
});
