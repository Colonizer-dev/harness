// @vitest-environment happy-dom
import { beforeEach, describe, expect, it } from "vitest";

import type { AgentModuleRoles, ModelAssignments, SwitchableModel } from "./types";
import {
  LAUNCH_AGENT_KEY,
  checkedChoice,
  getLaunchSelection,
  launchFields,
  launchGroups,
  launchSummary,
  resetLaunchSelectionCache,
  setLaunchSelection,
} from "./launchAgent";

function model(over: Partial<SwitchableModel> & Pick<SwitchableModel, "id" | "label" | "provider">): SwitchableModel {
  return {
    provider_name: over.provider === "anthropic" ? "Anthropic" : "MiniMax",
    wire: null,
    failure_pct: 0,
    rated: false,
    degraded: false,
    healthy: true,
    out_of_quota: false,
    reset_at: null,
    reset_unix: null,
    ...over,
  };
}

const MODULES: AgentModuleRoles[] = [
  { enabled_models: ["claude-opus-5-5"], enabled_models_set: true, id: "claude-code", name: "Claude Code", roles: [], blocked: null },
  { enabled_models: ["claude-opus-5-5", "claude-sonnet-5", "minimax/MiniMax-M2"], enabled_models_set: true, id: "pi", name: "Pi", roles: [], blocked: null },
  { enabled_models: ["minimax/MiniMax-M2"], enabled_models_set: true, id: "opencode", name: "OpenCode", roles: [], blocked: null },
  { enabled_models: ["claude-opus-5-5"], enabled_models_set: true, id: "acp", name: "ACP", roles: [], blocked: "the grok binary is not staged" },
];

const MODELS: SwitchableModel[] = [
  model({ id: "minimax/MiniMax-M2", label: "MiniMax-M2 · MiniMax", provider: "minimax" }),
  model({ id: "claude-opus-5-5", label: "Claude Opus 5.5", provider: "anthropic" }),
  model({ id: "claude-sonnet-5", label: "Claude Sonnet 5", provider: "anthropic", out_of_quota: true, reset_at: "Oct 22, 09:00 UTC" }),
];

function assignments(over: Partial<ModelAssignments> = {}): ModelAssignments {
  return {
    install: { enabled_models: [], enabled_models_set: false,
      module: "claude-code",
      roles: [{ role: "model", title: "Orchestrator model", value: "claude-opus-5-5", source: "install", org_settable: true }],
    },
    orgs: [],
    modules: MODULES,
    models: MODELS,
    ...over,
  };
}

describe("launchGroups", () => {
  it("groups the models by each launchable agent module, skipping blocked ones", () => {
    const groups = launchGroups(assignments());
    expect(groups.map((g) => g.name)).toEqual(["Claude Code", "Pi", "OpenCode"]);
    expect(groups.every((g) => g.module !== "acp")).toBe(true);
  });

  it("lists Claude's models first in every group, each with the subscription note", () => {
    const pi = launchGroups(assignments()).find((g) => g.module === "pi");
    expect(pi?.choices.map((c) => c.model)).toEqual(["claude-opus-5-5", "claude-sonnet-5", "minimax/MiniMax-M2"]);
    expect(pi?.choices[0].modelLabel).toBe("Claude Opus 5.5 (subscription)");
    expect(pi?.choices[2].modelLabel).toBe("MiniMax-M2 · MiniMax");
  });

  it("keeps an out-of-quota model listed but disabled, with its reason", () => {
    const sonnet = launchGroups(assignments()).find((g) => g.module === "pi")?.choices.find((c) => c.model === "claude-sonnet-5");
    expect(sonnet?.disabled).toBe(true);
    expect(sonnet?.reason).toBe("out of quota");
  });

  it("offers only the models the install enabled for each module", () => {
    const groups = launchGroups(assignments({ modules: MODULES.map((m) => (m.id === "claude-code" ? { ...m, enabled_models: [] } : m)) }));
    // Claude Code has nothing enabled, so it has no heading at all.
    expect(groups.map((g) => g.name)).toEqual(["Pi", "OpenCode"]);
    const opencode = groups.find((g) => g.module === "opencode");
    expect(opencode?.choices.map((c) => c.model)).toEqual(["minimax/MiniMax-M2"]);
  });

  it("labels an enabled model that the models list does not describe with its raw id", () => {
    const pi = launchGroups(assignments({ modules: MODULES.map((m) => (m.id === "pi" ? { ...m, enabled_models: ["claude-haiku-4-5"] } : m)) })).find((g) => g.module === "pi");
    expect(pi?.choices.map((c) => [c.model, c.modelLabel])).toEqual([["claude-haiku-4-5", "claude-haiku-4-5 (subscription)"]]);
  });

  it("lists nothing when no module can launch", () => {
    expect(launchGroups(assignments({ modules: MODULES.filter((m) => m.blocked) }))).toEqual([]);
  });
});

describe("launchFields (the launch body)", () => {
  it("sends a Claude account only for a Claude model on Pi or Claude Code, and only when one is chosen", () => {
    expect(launchFields({ module: "pi", model: "claude-opus-5-5", account: "work" })).toEqual({ agent_module: "pi", model_override: "claude-opus-5-5", claude_account: "work" });
    expect(launchFields({ module: "claude-code", model: "claude-opus-5-5", account: "personal" })).toMatchObject({ claude_account: "personal" });
    // Org default: no account chosen, so nothing is sent.
    expect(launchFields({ module: "pi", model: "claude-opus-5-5", account: null })).not.toHaveProperty("claude_account");
    expect(launchFields({ module: "pi", model: "claude-opus-5-5" })).not.toHaveProperty("claude_account");
    // Not a Claude model, or not a Claude-accepting module: never sent.
    expect(launchFields({ module: "pi", model: "minimax/MiniMax-M2", account: "work" })).not.toHaveProperty("claude_account");
    expect(launchFields({ module: "opencode", model: "claude-opus-5-5", account: "work" })).not.toHaveProperty("claude_account");
  });


  it("sends nothing when no choice is saved, so the org default applies", () => {
    expect(launchFields(null)).toEqual({});
  });

  it("sends the agent module and the model override for a chosen entry", () => {
    expect(launchFields({ module: "pi", model: "claude-opus-5-5" })).toEqual({ agent_module: "pi", model_override: "claude-opus-5-5" });
    expect(launchFields({ module: "pi", model: "minimax/MiniMax-M2" })).toEqual({ agent_module: "pi", model_override: "minimax/MiniMax-M2" });
  });

  it("omits the model override when the choice keeps the default model", () => {
    expect(launchFields({ module: "pi", model: null })).toEqual({ agent_module: "pi" });
  });
});

describe("saved choice", () => {
  beforeEach(() => {
    window.localStorage.clear();
    resetLaunchSelectionCache();
  });

  it("persists the chosen module and model in this browser", () => {
    setLaunchSelection({ module: "pi", model: "claude-opus-5-5" });
    expect(JSON.parse(window.localStorage.getItem(LAUNCH_AGENT_KEY) ?? "null")).toEqual({ module: "pi", model: "claude-opus-5-5" });
    resetLaunchSelectionCache();
    expect(getLaunchSelection()).toEqual({ module: "pi", model: "claude-opus-5-5" });
  });

  it("clears back to the install default", () => {
    setLaunchSelection({ module: "pi", model: null });
    setLaunchSelection(null);
    expect(getLaunchSelection()).toBeNull();
    expect(window.localStorage.getItem(LAUNCH_AGENT_KEY)).toBeNull();
  });

  it("ignores a stored value that is not a choice", () => {
    window.localStorage.setItem(LAUNCH_AGENT_KEY, "{not json");
    expect(getLaunchSelection()).toBeNull();
  });

  it("checks the install default in the menu when nothing is saved, and the saved choice otherwise", () => {
    const a = assignments();
    const groups = launchGroups(a);
    const install = { module: "claude-code", model: "claude-opus-5-5" };
    expect(checkedChoice(groups, null, install)?.module).toBe("claude-code");
    expect(checkedChoice(groups, { module: "pi", model: "minimax/MiniMax-M2" }, install)).toMatchObject({ module: "pi", model: "minimax/MiniMax-M2" });
    expect(launchSummary({ module: "pi", model: "claude-opus-5-5" }, a)).toEqual({ module: "pi", name: "Pi", model: "Opus 5.5" });
    expect(launchSummary(null, a)).toEqual({ module: "claude-code", name: "Claude Code", model: "Opus 5.5" });
  });
});
