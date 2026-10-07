// Settlers are the colony's subagents, and the cards show each one's name and ant. These tests pin the naming rules.
import { describe, expect, it } from "vitest";

import { settlerName, settlerRole } from "./settlers";

describe("settlerName", () => {
  it("names the agent types Claude Code ships after their roles", () => {
    const roles: [string, string][] = [
      ["explore", "Scout"],
      ["general-purpose", "Builder"],
      ["plan", "Surveyor"],
      ["claude", "Pioneer"],
      ["statusline-setup", "Signwright"],
      ["code-reviewer", "Inspector"],
      ["reviewer", "Inspector"],
      ["security-reviewer", "Warden"],
      ["test-runner", "Tester"],
      ["tester", "Tester"],
      ["architect", "Cartographer"],
      ["debugger", "Tracker"],
      ["docs-writer", "Scribe"],
      ["doc-updater", "Scribe"],
      ["build-error-resolver", "Mender"],
      ["refactor-cleaner", "Mason"],
    ];
    for (const [type, role] of roles) {
      expect(settlerName(type)).toBe(`${role} Settler`);
    }
  });

  it("treats no type as the default general-purpose Builder", () => {
    expect(settlerName(null)).toBe("Builder Settler");
    expect(settlerName(undefined)).toBe("Builder Settler");
    expect(settlerName("")).toBe("Builder Settler");
    expect(settlerName("   ")).toBe("Builder Settler");
  });

  it("matches the table whatever the case, and around whitespace", () => {
    expect(settlerName("Explore")).toBe("Scout Settler");
    expect(settlerName("  EXPLORE ")).toBe("Scout Settler");
  });

  it("title-cases an unknown type on its dashes, underscores and spaces", () => {
    expect(settlerName("api-designer")).toBe("Api Designer Settler");
    expect(settlerName("mesh_scout")).toBe("Mesh Scout Settler");
    expect(settlerName("town planner")).toBe("Town Planner Settler");
  });

  it("numbers the second and later settlers of a role, and only those", () => {
    expect(settlerName("explore")).toBe("Scout Settler");
    expect(settlerName("explore", 1)).toBe("Scout Settler");
    expect(settlerName("explore", 2)).toBe("Scout Settler 2");
    expect(settlerName("api-designer", 12)).toBe("Api Designer Settler 12");
  });
});

describe("settlerRole", () => {
  it("picks the ant drawn for a known role, named in lower case", () => {
    expect(settlerRole("explore")).toBe("scout");
    expect(settlerRole("general-purpose")).toBe("builder");
    expect(settlerRole("plan")).toBe("surveyor");
    expect(settlerRole("claude")).toBe("pioneer");
    expect(settlerRole("code-reviewer")).toBe("inspector");
    expect(settlerRole("security-reviewer")).toBe("warden");
    expect(settlerRole("test-runner")).toBe("tester");
    expect(settlerRole("architect")).toBe("cartographer");
    expect(settlerRole("debugger")).toBe("tracker");
    expect(settlerRole("docs-writer")).toBe("scribe");
    expect(settlerRole("build-error-resolver")).toBe("mender");
    expect(settlerRole("refactor-cleaner")).toBe("mason");
  });

  it("falls back to a Pioneer when the ant has no accessory for the role", () => {
    // The Signwright has no drawn ant, so a statusline-setup settler still goes out as a Pioneer.
    expect(settlerRole("statusline-setup")).toBe("pioneer");
  });

  it("sends unknown types out as Pioneers, but a missing type as a Builder", () => {
    expect(settlerRole("api-designer")).toBe("pioneer");
    // No type is the default general-purpose Builder, and there is an ant for that one.
    expect(settlerRole(null)).toBe("builder");
  });
});
