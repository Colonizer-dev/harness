import { describe, expect, it } from "vitest";
import { filterGroups, groupByOrg, languageColor } from "./RepoPicker";
import type { OrgInfo } from "../types";

const org = (name: string, description?: string) => ({ org: name, description, colonies: { live: 0, total: 0 }, pending_memory: 0, settings: {} }) as unknown as OrgInfo;

describe("RepoPicker grouping", () => {
  const repos = ["Colonizer-dev/harness", "CHI-Ecosystem/chi-web", "Colonizer-dev/bench", "CHI-Ecosystem/chi-app"];
  const orgs = [org("Colonizer-dev", "Agent colonies for your backlog"), org("CHI-Ecosystem", "The operating system for events")];

  it("groups repositories under their organization, in first-seen order", () => {
    const groups = groupByOrg(repos, orgs);
    expect(groups.map((g) => g.org)).toEqual(["Colonizer-dev", "CHI-Ecosystem"]);
    expect(groups[0].repos).toEqual(["Colonizer-dev/harness", "Colonizer-dev/bench"]);
    expect(groups[1].info?.description).toBe("The operating system for events");
  });

  it("filters by org name or description (keeping all its repos) and by repo name or description", () => {
    const groups = groupByOrg(repos, orgs);
    const describe = (r: string) => (r === "CHI-Ecosystem/chi-app" ? "React Native mobile app" : null);
    expect(filterGroups(groups, "events", describe).flatMap((g) => g.repos)).toEqual(["CHI-Ecosystem/chi-web", "CHI-Ecosystem/chi-app"]);
    expect(filterGroups(groups, "bench", describe).map((g) => [g.org, g.repos])).toEqual([["Colonizer-dev", ["Colonizer-dev/bench"]]]);
    expect(filterGroups(groups, "mobile", describe).flatMap((g) => g.repos)).toEqual(["CHI-Ecosystem/chi-app"]);
    expect(filterGroups(groups, "nothing-like-this", describe)).toEqual([]);
    expect(filterGroups(groups, "  ", describe)).toHaveLength(2);
  });

  it("colours languages like GitHub and falls back to grey", () => {
    expect(languageColor("Rust")).toBe("#dea584");
    expect(languageColor("TypeScript")).toBe("#3178c6");
    expect(languageColor("Zig")).toBe("#ec915c");
    expect(languageColor("Brainfuck")).toBe("#8b949e");
    expect(languageColor(null)).toBe("#8b949e");
  });
});
