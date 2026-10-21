// @vitest-environment happy-dom
// The agent and model menu (the cockpit's top-right pill and the launch dialog's row): it lists the
// enabled models in groups per installed module, saves a pick, offers a Claude account on Pi and
// Claude Code, and works from the keyboard.
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiContext } from "../context";
import type { Api } from "../api";
import { LAUNCH_AGENT_KEY, PI_CLAUDE_NOTE, getLaunchSelection, launchFields, resetLaunchSelectionCache, setLaunchSelection } from "../features/models/launchAgent";
import type { AgentModuleRoles, ClaudeAccount, ModelAssignments, SwitchableModel } from "../features/models/types";
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
  { id: "claude-code", name: "Claude Code", roles: [], blocked: null, enabled_models: ["claude-opus-5-5"], enabled_models_set: true },
  {
    id: "pi",
    name: "Pi",
    roles: [],
    blocked: null,
    enabled_models: ["claude-opus-5-5", "claude-sonnet-5", "minimax/MiniMax-M2"],
    enabled_models_set: true,
  },
];
const A: ModelAssignments = {
  install: {
    module: "claude-code",
    roles: [{ role: "model", title: "Orchestrator model", value: "claude-opus-5-5", source: "install", org_settable: true }],
    enabled_models: ["claude-opus-5-5"],
    enabled_models_set: true,
  },
  orgs: [],
  modules: MODULES,
  models: [
    model("claude-opus-5-5", "Claude Opus 5.5", "anthropic"),
    model("claude-sonnet-5", "Claude Sonnet 5", "anthropic"),
    model("minimax/MiniMax-M2", "MiniMax-M2 · MiniMax", "minimax", { out_of_quota: true, reset_at: "Oct 22" }),
    // On offer, but not enabled for any module: never listed in the menu.
    model("deepseek/flash", "flash · DeepSeek", "deepseek"),
  ],
};
const ACCOUNTS: ClaudeAccount[] = [
  { id: "work", label: "Work", is_default: true },
  { id: "personal", label: "Personal", is_default: false },
];

describe("AgentModelMenu", () => {
  let container: HTMLDivElement;
  let root: Root;
  let onManage = vi.fn<() => void>();

  const render = async (variant: "pill" | "inline" = "pill", initialOpen = false, assignments: ModelAssignments = A) => {
    await act(async () => {
      root.render(
        <ApiContext.Provider value={{} as Api}>
          <AgentModelMenu variant={variant} onManage={onManage} initialAssignments={assignments} initialAccounts={ACCOUNTS} initialOpen={initialOpen} />
        </ApiContext.Provider>,
      );
    });
  };
  const trigger = () => container.querySelector<HTMLButtonElement>('button[aria-haspopup="menu"]')!;
  const items = () => Array.from(document.querySelectorAll<HTMLElement>('[role="menuitemradio"], [role="menuitem"]'));
  const picker = () => document.querySelector<HTMLSelectElement>("select");
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

  it("lists only the models enabled for each module, not every model on offer", async () => {
    await render("pill", true);
    const menu = document.querySelector('[role="menu"]')!;
    expect(menu.textContent).not.toContain("flash");
    const piGroup = Array.from(document.querySelectorAll('[role="group"]')).find((g) => g.textContent?.startsWith("Pi"))!;
    expect(Array.from(piGroup.querySelectorAll('[role="menuitemradio"]')).map((el) => el.textContent)).toEqual([
      expect.stringContaining("Claude Opus 5.5"),
      expect.stringContaining("Claude Sonnet 5"),
      expect.stringContaining("MiniMax-M2"),
    ]);
  });

  it("shows no heading for a module with nothing enabled", async () => {
    await render("pill", true, { ...A, modules: [MODULES[0], { ...MODULES[1], enabled_models: [] }] });
    const headings = Array.from(document.querySelectorAll('[role="group"]')).map((g) => document.getElementById(g.getAttribute("aria-labelledby")!)?.textContent);
    expect(headings).toEqual(["Claude Code"]);
  });

  it("shows a hint and the Manage link when nothing is enabled anywhere", async () => {
    await render("pill", true, { ...A, modules: MODULES.map((m) => ({ ...m, enabled_models: [] })) });
    expect(document.querySelector('[role="group"]')).toBeNull();
    expect(document.querySelector('[role="menu"]')?.textContent).toContain("No model is enabled");
    expect(items().map((el) => el.textContent)).toEqual(["Manage models…"]);
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

    // Pi's Claude Opus 5.5 is the first Claude row under the Pi heading.
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

  describe("Claude account picker and the Pi note", () => {
    it("offers the picker for the install's own Claude model on Claude Code, with Org default first", async () => {
      await render("pill", true);
      expect(picker()).toBeTruthy();
      expect(Array.from(picker()!.options).map((o) => o.textContent)).toEqual(["Org default", "Work (default)", "Personal"]);
      expect(picker()!.value).toBe("");
      // Claude Code has no note.
      expect(document.body.textContent).not.toContain(PI_CLAUDE_NOTE);
    });

    it("shows the picker and the exact note for Claude on Pi", async () => {
      setLaunchSelection({ module: "pi", model: "claude-sonnet-5" });
      await render("pill", true);
      expect(picker()).toBeTruthy();
      expect(document.querySelector('[role="menu"]')!.parentElement!.textContent).toContain(
        "Billed as extra usage per token on your Claude subscription, not your plan limits.",
      );
      expect(PI_CLAUDE_NOTE).toBe("Billed as extra usage per token on your Claude subscription, not your plan limits.");
    });

    it("is absent for a non-Claude model on Pi", async () => {
      setLaunchSelection({ module: "pi", model: "minimax/MiniMax-M2" });
      await render("pill", true);
      expect(picker()).toBeNull();
      expect(document.body.textContent).not.toContain(PI_CLAUDE_NOTE);
    });

    it("is shown beside the launch dialog's row for Claude on Pi too", async () => {
      setLaunchSelection({ module: "pi", model: "claude-opus-5-5" });
      await render("inline");
      expect(picker()).toBeTruthy();
      expect(container.textContent).toContain(PI_CLAUDE_NOTE);
    });

    it("saves the chosen account, and Org default clears it so no account is sent", async () => {
      setLaunchSelection({ module: "pi", model: "claude-opus-5-5" });
      await render("pill", true);
      await act(async () => {
        picker()!.value = "personal";
        picker()!.dispatchEvent(new Event("change", { bubbles: true }));
      });
      expect(getLaunchSelection()).toEqual({ module: "pi", model: "claude-opus-5-5", account: "personal" });
      expect(launchFields(getLaunchSelection())).toMatchObject({ agent_module: "pi", model_override: "claude-opus-5-5", claude_account: "personal" });

      await act(async () => {
        picker()!.value = "";
        picker()!.dispatchEvent(new Event("change", { bubbles: true }));
      });
      expect(getLaunchSelection()).toEqual({ module: "pi", model: "claude-opus-5-5" });
      expect(launchFields(getLaunchSelection())).not.toHaveProperty("claude_account");
    });
  });
});
