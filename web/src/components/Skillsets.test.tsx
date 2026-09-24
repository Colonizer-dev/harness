// The downloadable-skillset row in Settings → Skillsets: one row per state a download can be in, and the
// "downloaded" badge once it is an ordinary skillset. Static markup, as the other component tests render.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { DownloadableSkillset } from "../types";
import { DownloadableRow, PluginBadges, downloadLine } from "./Skillsets";

const graft = (over: Partial<DownloadableSkillset> = {}): DownloadableSkillset => ({
  name: "graft",
  release: "0.19.0-1",
  installed_release: null,
  state: "idle",
  bytes: 0,
  total: null,
  started_at: null,
  finished_at: null,
  error: null,
  ...over,
});

const row = (item: DownloadableSkillset, starting = false) => renderToStaticMarkup(<DownloadableRow item={item} onDownload={() => {}} starting={starting} />);

describe("DownloadableRow", () => {
  it("offers a Download button before anything is on disk, and says what it costs", () => {
    const html = row(graft());
    expect(html).toContain('aria-label="Download the graft skillset"');
    expect(html).toContain(">Download<");
    expect(html).toContain("about 80 MB (0.19.0-1)");
    expect(html).toContain("downloadable");
  });

  it("shows progress while downloading, with the button disabled", () => {
    const html = row(graft({ state: "downloading", bytes: 20 * 1_048_576, total: 80 * 1_048_576 }));
    expect(html).toContain("Downloading… 20 MB of 80 MB");
    expect(html).toContain('aria-valuenow="25"');
    expect(html).toMatch(/<button[^>]*disabled/);
  });

  it("disables the button the moment it is pressed, before the first status arrives", () => {
    expect(row(graft(), true)).toMatch(/<button[^>]*disabled/);
  });

  it("names the failure and offers a retry", () => {
    const html = row(graft({ state: "failed", error: "checksum mismatch" }));
    expect(html).toContain("Download failed: checksum mismatch");
    expect(html).toContain(">Retry<");
  });

  it("offers nothing to press when no bundle is published, and says so", () => {
    const html = row(graft({ state: "unavailable", release: null }));
    expect(html).not.toContain("<button");
    expect(html).toContain("Not published for this machine yet");
  });

  it("says an old version on disk is replaced by a new download", () => {
    expect(downloadLine(graft({ installed_release: "0.18.0-1" }))).toBe("Version 0.18.0-1 is on disk; 0.19.0-1 is a new download.");
    expect(downloadLine(graft({ state: "unpacking" }))).toBe("Unpacking…");
    expect(downloadLine(graft({ state: "installed" }))).toBeNull();
  });
});

describe("PluginBadges", () => {
  const plugin = { name: "graft", description: null, version: "0.19.0", source: "local" as const, shadows_vendored: false, skills: 1, agents: 0, commands: 0 };
  it("marks a downloaded skillset as downloaded rather than local", () => {
    expect(renderToStaticMarkup(<PluginBadges plugin={plugin} downloaded />)).toContain("downloaded 0.19.0");
    expect(renderToStaticMarkup(<PluginBadges plugin={plugin} />)).toContain("local 0.19.0");
  });
});
