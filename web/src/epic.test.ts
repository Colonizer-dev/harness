import { describe, expect, it } from "vitest";

import { epicMarker, isEpic } from "./api";
import type { Issue } from "./types";

const issue = (title: string, labels: string[] = [], epic: Issue["epic"] = null) => ({
  title,
  labels: labels.map((name) => ({ name, color: "000000" })),
  epic,
});

describe("epics in issue lists (mirrors the mothership's epic.rs)", () => {
  it("marks an issue with sub-issues, an epic label, or a title that says so", () => {
    expect(epicMarker(issue("Remote access", [], { reason: "it has 5 sub-issues", sub_issues: 5 }))).toBe("Epic · 5 sub-issues");
    expect(epicMarker(issue("Remote access", [], { reason: "it has 1 sub-issue", sub_issues: 1 }))).toBe("Epic · 1 sub-issue");
    expect(epicMarker(issue("Planning", ["EPIC"]))).toBe("Epic");
    expect(epicMarker(issue("Remote access (epic)"))).toBe("Epic");
    expect(epicMarker(issue("Epic: billing"))).toBe("Epic");
  });

  it("leaves ordinary issues alone", () => {
    expect(isEpic(issue("Fix the epic loader", ["epic-followup"]))).toBe(false);
    expect(epicMarker(issue("Epics page is slow"))).toBeNull();
  });
});
