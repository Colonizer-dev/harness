import { describe, expect, it } from "vitest";
import { taskLine, taskTooltip } from "./summary";

describe("taskLine", () => {
  it("prefers the summary, then the issue title, then the fallback", () => {
    expect(taskLine({ issue_title: "Long issue title", summary: "Fix the login loop" }, "x")).toBe("Fix the login loop");
    expect(taskLine({ issue_title: "Long issue title", summary: null }, "x")).toBe("Long issue title");
    expect(taskLine({ issue_title: "", summary: "  " }, "open session")).toBe("open session");
  });
  it("puts the full issue title in the tooltip only when the summary stands in for it", () => {
    expect(taskTooltip({ issue_title: "Long issue title", summary: "Fix it" })).toBe("Long issue title");
    expect(taskTooltip({ issue_title: "Long issue title" })).toBeUndefined();
  });
});
