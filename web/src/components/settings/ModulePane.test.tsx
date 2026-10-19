// Autonomy's two loudest settings (issue #776): Full autonomy warns in the pane, and any ceiling
// above `workspace_write` asks once before the save goes out. Rendered to static markup — the test
// environment has no DOM — and the confirmation is checked as the pure function it is.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../../api";
import { ApiContext } from "../../context";
import type { AutonomyStatus, ModuleInfo } from "../../types";
import type { ImagePull } from "../../useImagePull";
import { AutonomyHealth } from "./AutonomyHealth";
import { FullAutonomyWarning, ModulePane, riskConfirmation } from "./ModulePane";

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

describe("the agent pane's cheapest-host switch (issue #1039)", () => {
  // Only the fields under test, on a schema the pane is given: the mothership's own claude-code
  // module.json declares the rest. The fetch stub never resolves — static markup runs no effects,
  // so the ranking panel under the switch stays empty, which is the second thing pinned here.
  const agentModule: ModuleInfo = {
    kind: "agent",
    provider: "claude-code",
    providers: [{ id: "claude-code", name: "Claude Code" }],
    enabled: true,
    settings: { model: "deepseek/deepseek-v4-flash", prefer_cheapest_host: true },
    schema: {
      type: "object",
      properties: {
        model: { type: "string", title: "Orchestrator model" },
        prefer_cheapest_host: { type: "boolean", title: "Prefer the cheapest host", default: false },
      },
    },
  };
  const api = { modelHosts: () => new Promise(() => {}) } as unknown as Api;

  it("renders the switch on, and no ranking until the hosts arrive", () => {
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <ModulePane
          module={agentModule}
          draft={{ provider: agentModule.provider, enabled: true, settings: { ...agentModule.settings } }}
          pull={{} as ImagePull}
          onDraft={() => {}}
          onReset={() => {}}
          onSaved={() => {}}
        />
      </ApiContext.Provider>,
    );
    expect(html).toContain("Prefer the cheapest host");
    expect(html).toContain('role="switch"');
    expect(html).toContain('aria-checked="true"');
    expect(html).not.toContain("Hosts for");
  });
});
