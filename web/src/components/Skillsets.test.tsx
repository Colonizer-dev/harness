// The downloadable-skillset row in Settings → Skillsets: one row per state a download can be in, and the
// "downloaded" badge once it is an ordinary skillset. Static markup, as the other component tests render.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { DownloadableSkillset, ModuleInfo } from "../types";
import { DownloadableRow, PackModulesLine, PluginBadges, downloadLine, packModuleNames } from "./Skillsets";

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

  // A second downloadable skillset gets a row of its own: the name, the licence, what it adds, and
  // the cost note that keeps an operator from expecting a whole-repo pass on the first colony.
  it("describes the understand-anything skillset, and does not pretend the download switches it on", () => {
    const html = row(graft({ name: "understand-anything", release: "v2.9.0" }));
    expect(html).toContain('aria-label="Download the understand-anything skillset"');
    expect(html).toContain(">Download<");
    expect(html).toContain("knowledge graph");
    expect(html).toContain("what a change would affect");
    expect(html).toContain("onboarding tours");
    expect(html).toContain("MIT, by Egonex");
    expect(html).toContain("multi-agent pass");
    expect(html).toContain("about 3 MB (v2.9.0)"); // its own size, not graft's 80 MB
    // Downloading only puts the plugin on disk; the operator still flips the switch.
    expect(html).not.toContain('aria-label="Load the understand-anything skillset"');
  });

  it("names the skillset's own directory when a local copy is used instead", () => {
    expect(downloadLine(graft({ name: "understand-anything", state: "local" }))).toBe(
      "Your own plugins/understand-anything directory is used instead; remove it to download the pinned bundle.",
    );
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

// What the Skillsets footer says about which modules load the packs (issue #1164): names taken
// from GET /api/modules' agent rows, never hardcoded, and silent while they have not loaded.
const moduleRow = (kind: string, providers: ModuleInfo["providers"]): ModuleInfo => ({
  kind,
  provider: providers[0]?.id ?? "",
  providers,
  enabled: true,
  settings: {},
  schema: null,
});

describe("packModuleNames", () => {
  it("names the agent modules that declare skill_packs, in row order, and ignores the rest", () => {
    const modules = [
      moduleRow("agent", [
        { id: "claude-code", name: "Claude Code", skill_packs: true },
        { id: "opencode", name: "OpenCode", skill_packs: true },
        { id: "codex", name: "Codex" },
      ]),
      // A non-agent row with the flag means nothing: packs are an agent-module capability.
      moduleRow("sandbox", [{ id: "colony-image", name: "Colony image", skill_packs: true }]),
      moduleRow("agent", [{ id: "pi", name: "Pi", skill_packs: true }]),
    ];
    expect(packModuleNames(modules)).toEqual(["Claude Code", "OpenCode", "Pi"]);
  });

  it("comes out empty without agent rows or without the flag, and repeats no name", () => {
    expect(packModuleNames([])).toEqual([]);
    expect(packModuleNames(null)).toEqual([]);
    expect(packModuleNames(undefined)).toEqual([]);
    expect(packModuleNames([moduleRow("agent", [{ id: "codex", name: "Codex" }])])).toEqual([]);
    expect(
      packModuleNames([
        moduleRow("agent", [
          { id: "claude-code", name: "Claude Code", skill_packs: true },
          { id: "claude-code-2", name: "Claude Code", skill_packs: true },
        ]),
      ]),
    ).toEqual(["Claude Code"]);
  });
});

describe("PackModulesLine", () => {
  it("names the modules that load packs and warns about the others", () => {
    const html = renderToStaticMarkup(<PackModulesLine names={["Claude Code", "OpenCode", "Pi"]} />);
    expect(html).toContain("Loaded by Claude Code, OpenCode and Pi.");
    expect(html).toContain("Other modules start without them (the colony log says so at boot).");
  });

  it("reads right with a single name, and renders nothing while the names have not loaded", () => {
    expect(renderToStaticMarkup(<PackModulesLine names={["Claude Code"]} />)).toContain("Loaded by Claude Code.");
    expect(renderToStaticMarkup(<PackModulesLine names={[]} />)).toBe("");
  });
});
