// @vitest-environment happy-dom
// The agent and model menu (the cockpit's top-right pill and the launch dialog's row): it lists the
// installed modules' models in groups, saves a pick, and works from the keyboard.
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiContext } from "../context";
import type { Api } from "../api";
import { LAUNCH_AGENT_KEY, getLaunchSelection, resetLaunchSelectionCache } from "../features/models/launchAgent";
import type { AgentModuleRoles, ModelAssignments, SwitchableModel } from "../features/models/types";
import { AgentModelMenu } from "./AgentModelMenu";

(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

const model = (id: string, label: string, provider: string, over: Partial<SwitchableModel> = {}): SwitchableModel => ({
  id,
  label,
  provider,
  provider_name: provider === "anthropic" ? "Anthropic" : "MiniMax",
  wire: null,
  failure_pct: 0,
  rated: false,
  degraded: false,
  healthy: true,
  out_of_quota: false,
  reset_at: null,
  reset_unix: null,
  ...over,
});

const MODULES: AgentModuleRoles[] = [
  { id: "claude-code", name: "Claude Code", roles: [], blocked: null },
  { id: "pi", name: "Pi", roles: [], blocked: null },
];
const A: ModelAssignments = {
  install: { module: "claude-code", roles: [{ role: "model", title: "Orchestrator model", value: "claude-opus-5-5", source: "install", org_settable: true }] },
  orgs: [],
  modules: MODULES,
  models: [model("claude-opus-5-5", "Claude Opus 5.5", "anthropic"), model("minimax/MiniMax-M2", "MiniMax-M2 · MiniMax", "minimax", { out_of_quota: true, reset_at: "Oct 22" })],
};

describe("AgentModelMenu", () => {
  let container: HTMLDivElement;
  let root: Root;
  let onManage = vi.fn<() => void>();

  const render = async (variant: "pill" | "inline" = "pill", initialOpen = false) => {
    await act(async () => {
      root.render(
        <ApiContext.Provider value={{} as Api}>
          <AgentModelMenu variant={variant} onManage={onManage} initialAssignments={A} initialOpen={initialOpen} />
        </ApiContext.Provider>,
      );
    });
  };
  const trigger = () => container.querySelector<HTMLButtonElement>('button[aria-haspopup="menu"]')!;
  const items = () => Array.from(document.querySelectorAll<HTMLElement>('[role="menuitemradio"], [role="menuitem"]'));
  const key = async (el: Element, k: string) => {
    await act(async () => {
      el.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true }));
    });
  };

  beforeEach(() => {
    window.localStorage.clear();
    resetLaunchSelectionCache();
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    onManage = vi.fn<() => void>();
  });
  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
  });

  it("is a button that reports its menu state", async () => {
    await render();
    expect(trigger().getAttribute("aria-expanded")).toBe("false");
    await act(async () => trigger().click());
    expect(trigger().getAttribute("aria-expanded")).toBe("true");
    expect(document.querySelector('[role="menu"]')).toBeTruthy();
  });

  it("groups the enabled models under each module's heading and marks the install default", async () => {
    await render("pill", true);
    const headings = Array.from(document.querySelectorAll('[role="group"]')).map((g) => g.getAttribute("aria-labelledby"));
    expect(headings.map((id) => document.getElementById(id!)?.textContent)).toEqual(["Claude Code", "Pi"]);
    const checked = document.querySelectorAll('[role="menuitemradio"][aria-checked="true"]');
    expect(checked).toHaveLength(1);
    expect(checked[0].textContent).toContain("Claude Opus 5.5");
  });

  it("moves focus with the arrow keys and selects with Enter, then returns focus to the trigger", async () => {
    await render("pill", true);
    expect(document.activeElement).toBe(items()[0]);
    await key(items()[0], "ArrowDown");
    expect(document.activeElement).toBe(items()[1]);
    await key(items()[1], "ArrowDown");
    expect(document.activeElement).toBe(items()[2]);
    await key(items()[2], "ArrowDown");
    expect(document.activeElement).toBe(items()[3]);
    await key(items()[items().length - 1], "ArrowDown");
    expect(document.activeElement).toBe(items()[0]);
    await key(items()[0], "ArrowUp");
    expect(document.activeElement).toBe(items()[items().length - 1]);
    await key(items()[items().length - 1], "Home");
    expect(document.activeElement).toBe(items()[0]);

    // Pi's Claude Opus 5.5 is the second Claude row under the Pi heading.
    const piOpus = items().find((el) => el.textContent?.includes("Claude Opus 5.5") && el.closest('[role="group"]')?.textContent?.startsWith("Pi"));
    expect(piOpus).toBeTruthy();
    await key(piOpus!, "Enter");
    expect(getLaunchSelection()).toEqual({ module: "pi", model: "claude-opus-5-5" });
    expect(JSON.parse(window.localStorage.getItem(LAUNCH_AGENT_KEY) ?? "null")).toEqual({ module: "pi", model: "claude-opus-5-5" });
    expect(document.querySelector('[role="menu"]')).toBeNull();
    expect(document.activeElement).toBe(trigger());
    expect(trigger().getAttribute("aria-label")).toContain("Pi · Opus 5.5");
  });

  it("closes on Escape and returns focus to the trigger without choosing", async () => {
    await render("pill", true);
    await key(items()[0], "Escape");
    expect(document.querySelector('[role="menu"]')).toBeNull();
    expect(document.activeElement).toBe(trigger());
    expect(getLaunchSelection()).toBeNull();
  });

  it("does not pick an out-of-quota model", async () => {
    await render("pill", true);
    const out = items().find((el) => el.textContent?.includes("MiniMax-M2"))!;
    expect(out.getAttribute("aria-disabled")).toBe("true");
    await key(out, "Enter");
    expect(getLaunchSelection()).toBeNull();
    expect(document.querySelector('[role="menu"]')).toBeTruthy();
  });

  it("opens Settings → Models from Manage models…", async () => {
    await render("pill", true);
    const manage = items().find((el) => el.textContent === "Manage models…")!;
    await key(manage, "Enter");
    expect(onManage).toHaveBeenCalledTimes(1);
  });

  it("the launch dialog's row shows the choice and a Change button that opens the same menu", async () => {
    await render("inline");
    expect(container.textContent).toContain("Runs on");
    await act(async () => trigger().click());
    expect(document.querySelector('[role="menu"]')).toBeTruthy();
    expect(trigger().textContent).toBe("Change");
  });
});
