// Autonomy's two loudest settings (issue #776): Full autonomy warns in the pane, and any ceiling
// above `workspace_write` asks once before the save goes out. Rendered to static markup — the test
// environment has no DOM — and the confirmation is checked as the pure function it is.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { AutonomyStatus } from "../../types";
import { AutonomyHealth } from "./AutonomyHealth";
import { FullAutonomyWarning, riskConfirmation } from "./ModulePane";

describe("autonomy above workspace_write asks once", () => {
  it("asks nothing for a ceiling at or below workspace_write, or when autonomy is off", () => {
    expect(riskConfirmation("judge", true, { risk_ceiling: "workspace_write" })).toBeNull();
    expect(riskConfirmation("full_autonomy", true, { risk_ceiling: "read_only" })).toBeNull();
    expect(riskConfirmation("judge", false, { risk_ceiling: "credential_adjacent" })).toBeNull();
    expect(riskConfirmation("off", true, { risk_ceiling: "credential_adjacent" })).toBeNull();
  });

  it("names the risk the judge would answer", () => {
    const judge = riskConfirmation("judge", true, { risk_ceiling: "credential_adjacent" });
    expect(judge).toContain("credential adjacent");
    expect(judge).toContain("near a key");
    expect(riskConfirmation("full_autonomy", true, { risk_ceiling: "publish_affecting" })).toContain("no answer limit");
  });
});

describe("FullAutonomyWarning", () => {
  it("says what it does and which guardrails still hold", () => {
    const out = renderToStaticMarkup(<FullAutonomyWarning />);
    expect(out).toContain("no answer limit");
    expect(out).toContain("never overrides a denial");
    expect(out).toContain("logged as the judge");
    expect(out).toContain("text-err");
  });
});

describe("AutonomyHealth", () => {
  const status = (over: Partial<AutonomyStatus>): AutonomyStatus => ({
    enabled: true,
    model: null,
    fallback_models: [],
    last_success: null,
    last_error: null,
    consecutive_failures: 0,
    alerted: false,
    ...over,
  });

  it("shows the on-with-no-model problem in a warning tone", () => {
    const out = renderToStaticMarkup(<AutonomyHealth status={status({ problem: "Autonomous mode is on but has no model." })} />);
    expect(out).toContain("has no model");
    expect(out).toContain("bg-warn-soft");
  });
});
