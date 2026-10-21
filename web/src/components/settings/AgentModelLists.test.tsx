// @vitest-environment happy-dom
// Settings → Models, the agent module's pane: the checkbox list per module saves the install's list
// with PUT /api/models/enabled, and "Reset to assigned models" sends null.
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { httpApi } from "../../api";
import { ApiContext } from "../../context";
import type { ModelAssignments } from "../../features/models/types";
import { AgentModelLists } from "./AgentModelLists";

(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

const ASSIGNMENTS: ModelAssignments = {
  install: { module: "pi", roles: [], enabled_models: ["claude-opus-5-5"], enabled_models_set: true },
  orgs: [],
  modules: [
    { id: "pi", name: "Pi", roles: [], blocked: null, enabled_models: ["claude-opus-5-5"], enabled_models_set: true },
    { id: "opencode", name: "OpenCode", roles: [], blocked: null, enabled_models: [], enabled_models_set: false },
  ],
  models: [
    { id: "claude-opus-5-5", label: "Claude Opus 5.5", provider: "anthropic", provider_name: "Anthropic", wire: null, failure_pct: 0, rated: false, degraded: false, healthy: true, out_of_quota: false, reset_at: null, reset_unix: null },
    { id: "minimax/MiniMax-M2", label: "MiniMax-M2 · MiniMax", provider: "minimax", provider_name: "MiniMax", wire: null, failure_pct: 0, rated: false, degraded: false, healthy: true, out_of_quota: false, reset_at: null, reset_unix: null },
  ],
};

const json = (body: unknown) => new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });

describe("AgentModelLists (Settings → Models)", () => {
  let container: HTMLDivElement;
  let root: Root;
  let fetchMock: ReturnType<typeof vi.fn>;

  const flush = () => act(async () => new Promise((r) => setTimeout(r, 0)));
  const box = (label: string) => Array.from(container.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')).find((i) => i.labels?.[0]?.textContent === label)!;
  const button = (text: string, within: HTMLElement = container) => Array.from(within.querySelectorAll("button")).find((b) => b.textContent?.trim() === text)!;
  const putCalls = () => fetchMock.mock.calls.filter(([, init]) => (init as RequestInit | undefined)?.method === "PUT");

  beforeEach(async () => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === "PUT") {
        const body = JSON.parse(String(init.body));
        return json({
          scope: "install",
          org: null,
          module: body.module,
          enabled_models: body.models ?? ["claude-opus-5-5"],
          enabled_models_set: body.models !== null,
        });
      }
      if (url === "/api/models/assignments") return json(ASSIGNMENTS);
      return new Response("not found", { status: 404 });
    });
    vi.stubGlobal("fetch", fetchMock);
    await act(async () => {
      root.render(
        <ApiContext.Provider value={httpApi}>
          <AgentModelLists />
        </ApiContext.Provider>,
      );
    });
    await flush();
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    vi.unstubAllGlobals();
  });

  it("lists every model on offer per module, checked when the module has it enabled", () => {
    const pi = container.querySelector("fieldset")!;
    expect(pi.querySelector("legend")?.textContent).toBe("Pi");
    expect(box("Claude Opus 5.5").checked).toBe(true);
    expect(box("MiniMax-M2 · MiniMax").checked).toBe(false);
  });

  it("saves the ticked models with a PUT to the install's list", async () => {
    await act(async () => box("MiniMax-M2 · MiniMax").click());
    await act(async () => button("Save models").click());
    await flush();
    expect(putCalls()).toHaveLength(1);
    const [url, init] = putCalls()[0] as [string, RequestInit];
    expect(url).toBe("/api/models/enabled");
    expect(JSON.parse(String(init.body))).toEqual({ scope: "install", module: "pi", models: ["claude-opus-5-5", "minimax/MiniMax-M2"] });
    expect(container.textContent).toContain("Saved: 2 models enabled for Pi.");
  });

  it("sends models: null when Reset to assigned models is pressed", async () => {
    await act(async () => button("Reset to assigned models").click());
    await flush();
    expect(JSON.parse(String((putCalls()[0][1] as RequestInit).body))).toEqual({ scope: "install", module: "pi", models: null });
    expect(container.textContent).toContain("Reset Pi to its assigned models.");
  });

  it("shows the error text when the save is refused", async () => {
    fetchMock.mockImplementationOnce(async () => json(ASSIGNMENTS));
    fetchMock.mockImplementation(async () => new Response(JSON.stringify({ error: "minimax/x is not a model on offer" }), { status: 400, headers: { "content-type": "application/json" } }));
    await act(async () => root.unmount());
    root = createRoot(container);
    await act(async () => root.render(<ApiContext.Provider value={httpApi}><AgentModelLists /></ApiContext.Provider>));
    await flush();
    await act(async () => box("MiniMax-M2 · MiniMax").click());
    await act(async () => button("Save models").click());
    await flush();
    expect(container.querySelector('[role="status"]')?.textContent).toBe("minimax/x is not a model on offer");
  });
});
