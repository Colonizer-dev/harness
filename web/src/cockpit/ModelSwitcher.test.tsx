// The header's model switcher (issue #1051): the chip names the main model with its provider's
// health, the popover switches scope (all orgs or one org) and agent module, shows one row per role
// the module declares with its source, keeps out-of-quota models disabled, applies new-only at
// once and running only after a counted confirm, remembers recent models, and opens from ⌘K's
// `/model`. Rendered to static markup (no DOM), so the interactions are pinned through the pure
// helpers and the mock API.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import type { AutonomyStatus, ModelAssignments, ModelProvider, ModuleInfo, ModelProfile, ModelSwitchReply, SwitchableModel } from "../types";
import { ColonizePane, DRAFT_START } from "./Colonize";
import { Header } from "./Header";
import { judgeAlternative, judgeOptions, judgeSaveBody, judgeTone } from "./JudgeModel";
import {
  ACCOUNT_FALLBACK_NOTE,
  EMPTY_DRAFT,
  INHERIT,
  INSTALL_SCOPE,
  ModelSwitcher,
  clearLeftoversRequest,
  confirmLine,
  draftDirty,
  groupModels,
  groupsForRole,
  hasLeftovers,
  inheritOptionLabel,
  leftoverLine,
  isModelCommand,
  modelHealth,
  modelOptionLabel,
  pickModule,
  pickRole,
  profileDraft,
  profileNote,
  pushRecent,
  recentChoices,
  roleQuotaBadge,
  rowSelectValue,
  scopeView,
  selectionRoles,
  shortModelName,
  sourceLabel,
  switchRequest,
  switchSummary,
  type ModelSwitcherProps,
} from "./ModelSwitcher";

const model = (over: Partial<SwitchableModel> & Pick<SwitchableModel, "id" | "label" | "provider">): SwitchableModel => ({
  provider_name: over.provider === "anthropic" ? "Anthropic" : over.provider,
  wire: over.provider === "anthropic" ? null : "anthropic",
  failure_pct: 0,
  rated: false,
  degraded: false,
  healthy: true,
  out_of_quota: false,
  reset_at: null,
  reset_unix: null,
  ...over,
});

const MODELS: SwitchableModel[] = [
  model({ id: "zai/glm-5", label: "glm-5 · Z.AI", provider: "zai", provider_name: "Z.AI", rated: true, failure_pct: 1.5 }),
  model({ id: "claude-opus-5-5", label: "Claude Opus 5.5", provider: "anthropic" }),
  model({ id: "sonnet", label: "Claude Sonnet (latest)", provider: "anthropic" }),
  model({ id: "strix/ds4-flash", label: "ds4-flash · Strix Halo", provider: "strix", provider_name: "Strix Halo", rated: true, failure_pct: 29.4, degraded: true, healthy: false }),
  model({
    id: "bailian/qwen3.8-max",
    label: "qwen3.8-max · Bailian",
    provider: "bailian",
    provider_name: "Bailian",
    out_of_quota: true,
    degraded: true,
    healthy: false,
    reset_at: "Oct 6, 09:00 UTC",
    reset_unix: Date.UTC(2026, 9, 6, 9, 0, 0) / 1000,
  }),
];

const row = (role: string, title: string, value: string, source: "org" | "install" | "default", org_settable = true) => ({ role, title, value, source, org_settable });

const ASSIGNMENTS: ModelAssignments = {
  install: {
    module: "claude-code",
    roles: [
      row("model", "Orchestrator model", "claude-opus-5-5", "install"),
      row("subagent_model", "Subagent model", "", "default"),
      row("background_model", "Background model", "sonnet", "install"),
      row("summary_model", "Summary model", "", "default", false),
      row("model_low", "Model for small tasks", "", "default", false),
      row("model_high", "Model for large tasks", "", "default", false),
    ],
  },
  orgs: [
    {
      org: "acme",
      module: "claude-code",
      module_source: "install",
      roles: [
        row("model", "Orchestrator model", "strix/ds4-flash", "org"),
        row("subagent_model", "Subagent model", "", "default"),
        row("background_model", "Background model", "sonnet", "install"),
        row("summary_model", "Summary model", "", "default", false),
        row("model_low", "Model for small tasks", "", "default", false),
        row("model_high", "Model for large tasks", "", "default", false),
      ],
    },
    { org: "beta", module: "opencode", module_source: "org", roles: [row("model", "Model", "zai/glm-5", "org"), row("small_model", "Small model", "", "default", false)] },
  ],
  modules: [
    { id: "claude-code", name: "Claude Code", roles: [], blocked: null },
    { id: "opencode", name: "OpenCode", roles: [{ role: "model", title: "Model", org_settable: true }, { role: "small_model", title: "Small model", org_settable: false }], blocked: null },
    { id: "pi", name: "Pi", roles: [{ role: "model", title: "Model", org_settable: true }], blocked: null },
    { id: "acp", name: "ACP", roles: [{ role: "model", title: "Model", org_settable: true }], blocked: "agent module `acp` needs the `grok` binary" },
  ],
  models: MODELS,
};
ASSIGNMENTS.modules[0].roles = ASSIGNMENTS.install.roles.map(({ role, title, org_settable }) => ({ role, title, org_settable }));

const render = (props: ModelSwitcherProps = {}, api: Api = {} as Api) =>
  renderToStaticMarkup(
    <ApiContext.Provider value={api}>
      <ModelSwitcher initialAssignments={ASSIGNMENTS} initialRecent={[]} {...props} />
    </ApiContext.Provider>,
  );

describe("the chip", () => {
  it("names the install's main model with its provider's health", () => {
    const html = render();
    expect(html).toContain('aria-label="Models · main model Opus 5.5"');
    expect(html).toContain('data-health="ok"');
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain('role="dialog"');
  });

  it("reads short names and health off the model list", () => {
    expect(shortModelName("claude-opus-5-5", MODELS)).toBe("Opus 5.5");
    expect(shortModelName("sonnet", MODELS)).toBe("Sonnet");
    expect(shortModelName("zai/glm-5", MODELS)).toBe("glm-5");
    expect(shortModelName("lab/unknown", MODELS)).toBe("unknown");
    expect(shortModelName("", MODELS)).toBe("Default");
    expect(modelHealth("strix/ds4-flash", MODELS)).toBe("warn");
    expect(modelHealth("bailian/qwen3.8-max", MODELS)).toBe("err");
    expect(modelHealth("nope/x", MODELS)).toBe("unknown");
  });

  it("renders nothing until the assignments are in", () => {
    expect(renderToStaticMarkup(<ApiContext.Provider value={{} as Api}><ModelSwitcher initialRecent={[]} /></ApiContext.Provider>)).toBe("");
  });

  it("sits in the header's top right, before the bell", () => {
    const html = renderToStaticMarkup(
      <Header orgs={[]} selectedOrg={null} onSelectOrg={() => {}} needByOrg={{}} statusError={false} onOpenRemote={() => {}} models={<span data-testid="models-chip" />} />,
    );
    expect(html).toContain('data-testid="models-chip"');
    expect(html.indexOf("flex-1")).toBeLessThan(html.indexOf("models-chip"));
  });
});

describe("the popover", () => {
  it("lists every role of the install's module with where its value comes from", () => {
    const html = render({ initialOpen: true });
    expect(html).toContain('role="dialog"');
    expect(html).toContain("All orgs (install default)");
    expect(html).toContain('<option value="org:acme">acme · overrides</option>');
    for (const title of ["Orchestrator model", "Subagent model", "Background model", "Summary model", "Model for small tasks", "Model for large tasks"]) {
      expect(html).toContain(`aria-label="${title} model"`);
    }
    expect(html).toContain("set install-wide");
    expect(html).toContain("module default");
  });

  it("switches scope to an org: its overrides, the install's inherited values, and install-only roles locked", () => {
    const view = scopeView(ASSIGNMENTS, { kind: "org", org: "acme" });
    expect(view.module).toBe("claude-code");
    expect(view.rows.find((r) => r.role === "model")).toMatchObject({ value: "strix/ds4-flash", source: "org", editable: true });
    expect(view.rows.find((r) => r.role === "summary_model")?.editable).toBe(false);
    expect(sourceLabel("install", { kind: "org", org: "acme" })).toBe("install default");
    expect(sourceLabel("org", { kind: "org", org: "acme" })).toBe("org override");

    const html = render({ initialOpen: true, initialScope: { kind: "org", org: "acme" } });
    expect(html).toContain("Use install default (Claude Code)");
    expect(html).toContain("org override");
    expect(html).toContain("install-wide only");
    expect(html).toContain("Use install default (Sonnet)");
  });

  it("shows only the roles the selected agent module declares, and marks a module that can't launch", () => {
    const beta = scopeView(ASSIGNMENTS, { kind: "org", org: "beta" });
    expect(beta.module).toBe("opencode");
    expect(beta.rows.map((r) => r.role)).toEqual(["model", "small_model"]);
    expect(scopeView(ASSIGNMENTS, INSTALL_SCOPE, "pi").rows.map((r) => r.role)).toEqual(["model"]);
    const html = render({ initialOpen: true });
    expect(html).toMatch(/<option value="acp" disabled="" title="agent module `acp` needs the `grok` binary">ACP — can&#x27;t launch here<\/option>/);
  });

  it("groups models by provider, Claude first, and disables the out-of-quota ones with their reset", () => {
    expect(groupModels(MODELS).map((g) => g.provider)).toEqual(["anthropic", "zai", "strix", "bailian"]);
    const out = MODELS.find((m) => m.out_of_quota)!;
    expect(modelOptionLabel(out)).toBe("qwen3.8-max · Bailian — out of quota until Oct 6, 09:00 UTC");
    expect(modelOptionLabel(MODELS[3])).toBe("ds4-flash · Strix Halo — degraded, 29.4% failing");
    expect(modelOptionLabel(MODELS[0])).toBe("glm-5 · Z.AI — 1.5% failing");
    const html = render({ initialOpen: true });
    expect(html).toContain('<optgroup label="Anthropic">');
    expect(html).toContain('<option value="bailian/qwen3.8-max" disabled="">qwen3.8-max · Bailian — out of quota until Oct 6, 09:00 UTC</option>');
  });
});

describe("editing and applying", () => {
  const acme = { kind: "org", org: "acme" } as const;

  it("tracks picks against the scope's own values, and clears an org override with inherit", () => {
    const view = scopeView(ASSIGNMENTS, acme);
    const main = view.rows[0];
    expect(rowSelectValue(main, acme, EMPTY_DRAFT)).toBe("strix/ds4-flash");
    let d = pickRole(EMPTY_DRAFT, main, acme, INHERIT);
    expect(d.roles).toEqual({ model: null });
    expect(rowSelectValue(main, acme, d)).toBe(INHERIT);
    d = pickRole(d, main, acme, "strix/ds4-flash");
    expect(draftDirty(d)).toBe(false);
    const sub = view.rows[1];
    expect(rowSelectValue(sub, acme, EMPTY_DRAFT)).toBe(INHERIT);
    expect(pickRole(EMPTY_DRAFT, sub, acme, "sonnet").roles).toEqual({ subagent_model: "sonnet" });
  });

  it("changes an org's module and drops picks the new module lacks", () => {
    let d = pickRole(EMPTY_DRAFT, scopeView(ASSIGNMENTS, acme).rows[1], acme, "sonnet");
    d = pickModule(d, ASSIGNMENTS, acme, "pi");
    expect(d).toEqual({ module: "pi", roles: {} });
    expect(pickModule(EMPTY_DRAFT, ASSIGNMENTS, acme, "")).toEqual({ roles: {} });
    expect(pickModule(EMPTY_DRAFT, ASSIGNMENTS, { kind: "org", org: "beta" }, "")).toEqual({ module: "", roles: {} });
  });

  it("builds new-only and running requests, the running one counted by a dry run first", () => {
    const draft = { roles: { model: "sonnet", background_model: null } };
    expect(switchRequest(INSTALL_SCOPE, draft, "new")).toEqual({ scope: "install", roles: { model: "sonnet", background_model: null }, apply: "new" });
    expect(switchRequest(acme, { module: "pi", roles: {} }, "running", true)).toEqual({ scope: "org", org: "acme", module: "pi", roles: {}, apply: "running", dry_run: true });
    expect(confirmLine(3)).toBe("Restart 3 running colonies on the new models?");
    expect(confirmLine(1)).toBe("Restart 1 running colony on the new models?");
    expect(confirmLine(0)).toMatch(/only new colonies change/);
  });

  it("shows the counted confirm before a running switch, and Apply only once something changed", () => {
    expect(render({ initialOpen: true })).toMatch(/<button type="button" disabled=""[^>]*>Apply<\/button>/);
    const dirty = render({ initialOpen: true, initialDraft: { roles: { model: "sonnet" } } });
    expect(dirty).toMatch(/<button type="button"[^>]*>Apply<\/button>/);
    expect(dirty).not.toMatch(/disabled=""[^>]*>Apply</);
    expect(dirty).toContain(">changed<");
    const confirm = render({ initialOpen: true, initialDraft: { roles: { model: "sonnet" } }, initialApply: "running", initialStage: { step: "confirm", affected: ["c1", "c2"] } });
    expect(confirm).toContain('role="alertdialog"');
    expect(confirm).toContain("Restart 2 running colonies on the new models?");
    expect(confirm).toContain("Switch and restart 2");
  });

  it("says what a switch did", () => {
    const reply: ModelSwitchReply = {
      dry_run: false,
      scope: "org",
      org: "acme",
      module: "claude-code",
      changes: [
        { scope: "org", target: "acme", key: "model", was: "strix/ds4-flash", now: "sonnet" },
        { scope: "colony", target: "c1", key: "model", was: "strix/ds4-flash", now: "sonnet" },
      ],
      affected: ["c1"],
      colonies: ["c1"],
      failed: [],
    };
    expect(switchSummary(reply)).toBe("1 setting switched for acme · 1 colony restarting");
    expect(switchSummary({ ...reply, org: null, changes: [], colonies: [] })).toBe("nothing changed for all orgs");
  });
});

describe("recent", () => {
  it("keeps five model ids, newest first, and offers the ones other than the current main", () => {
    let list: string[] = [];
    for (const id of ["a", "b", "c", "a", "", "d", "e", "f"]) list = pushRecent(list, id);
    expect(list).toEqual(["f", "e", "d", "a", "c"]);
    expect(recentChoices(["claude-opus-5-5", "sonnet"], "claude-opus-5-5")).toEqual(["sonnet"]);
    const html = render({ initialOpen: true, initialRecent: ["sonnet", "claude-opus-5-5", "bailian/qwen3.8-max"] });
    expect(html).toContain("Recent · main model, new colonies");
    expect(html).toMatch(/title="Switch the main model to sonnet"[^>]*>.*Sonnet<\/button>/);
    expect(html).toMatch(/disabled="" title="out of quota until Oct 6, 09:00 UTC"/);
  });
});

describe("the ⌘K command", () => {
  it("recognises /model and \"switch model\"", () => {
    for (const t of ["/model", " /models ", "Switch model", "switch models…", "switch model..."]) expect(isModelCommand(t)).toBe(true);
    for (const t of ["/loop 1h model", "model", "switch the model to opus", ""]) expect(isModelCommand(t)).toBe(false);
  });

  it("offers \"Switch model…\" in the Colonize pane while /model is typed", () => {
    const pane = (text: string) =>
      renderToStaticMarkup(
        <ApiContext.Provider value={{} as Api}>
          <ColonizePane
            repos={[]}
            org={null}
            sessions={[]}
            githubConnected
            autopilotDefault={false}
            onCreated={() => {}}
            onOpenColony={() => {}}
            scope={[]}
            onLoadedCount={() => {}}
            onClose={() => {}}
            preloaded={{}}
            initialDraft={{ ...DRAFT_START, text }}
          />
        </ApiContext.Provider>,
      );
    expect(pane("/model")).toContain(">Switch model…</span>");
    expect(pane("fix the build")).not.toContain("Switch model…");
    expect(pane("")).toContain("switches models");
  });
});

describe("plan usage and quota badges", () => {
  it("badges a role whose model's plan is out, with the countdown", () => {
    const now = Date.UTC(2026, 9, 6, 6, 50, 0);
    expect(roleQuotaBadge("bailian/qwen3.8-max", MODELS, now)).toEqual({
      text: "Bailian out · 2 h 10 min",
      title: "Bailian plan exhausted: out of quota until Oct 6, 09:00 UTC",
    });
    expect(roleQuotaBadge("zai/glm-5", MODELS, now)).toBeNull();
    expect(roleQuotaBadge("", MODELS, now)).toBeNull();
  });

  it("renders the plans section and the badge beside the role on the exhausted plan", () => {
    const html = render({
      initialOpen: true,
      initialScope: { kind: "org", org: "beta" },
      initialDraft: { roles: {} },
      initialPlans: [
        {
          id: "bailian", name: "Bailian", kind: "provider", used_by: ["orchestrator"], exhausted: true, reset_at: null,
          reset_unix: Date.UTC(2026, 9, 6, 9, 0, 0) / 1000, last_limit: null, requests: 12, failures: 0, last_request_at: null, since: null, balance: null,
        },
      ],
    });
    expect(html).toContain('aria-label="plan usage"');
    expect(html).toContain('data-plan="bailian" data-tone="err"');
    const withBadge = render({ initialOpen: true, initialDraft: { roles: { model: "bailian/qwen3.8-max" } } });
    expect(withBadge).toContain("data-quota-badge");
    expect(withBadge).toContain("Bailian out");
    expect(render({ initialOpen: true })).not.toContain("data-quota-badge");
  });
});

describe("profiles", () => {
  const profile = (roles: Record<string, string>, over: Partial<ModelProfile> = {}): ModelProfile => ({ id: "p-1", name: "Night shift", module: "claude-code", roles, builtin: false, ...over });

  it("saves the current selection: draft picks, else what each role resolves to", () => {
    const view = scopeView(ASSIGNMENTS, INSTALL_SCOPE);
    expect(selectionRoles(view, { roles: { subagent_model: "zai/glm-5", background_model: null } })).toEqual({
      model: "claude-opus-5-5",
      subagent_model: "zai/glm-5",
      background_model: "",
      summary_model: "",
      model_low: "",
      model_high: "",
    });
  });

  it("loads a profile into the draft for the install, and Apply sends it as a normal switch", () => {
    const loaded = profileDraft(ASSIGNMENTS, INSTALL_SCOPE, EMPTY_DRAFT, profile({ model: "sonnet", subagent_model: "zai/glm-5", model_high: "" }));
    expect(loaded.skipped).toEqual([]);
    expect(loaded.draft.roles).toEqual({ model: "sonnet", subagent_model: "zai/glm-5" });
    expect(switchRequest(INSTALL_SCOPE, loaded.draft, "running", true)).toEqual({
      scope: "install",
      roles: { model: "sonnet", subagent_model: "zai/glm-5" },
      apply: "running",
      dry_run: true,
    });
  });

  it("applies to one org only the roles an org can set, and names the skipped ones", () => {
    const acme = { kind: "org" as const, org: "acme" };
    const loaded = profileDraft(ASSIGNMENTS, acme, EMPTY_DRAFT, profile({ model: "", subagent_model: "zai/glm-5", model_high: "sonnet" }));
    expect(loaded.draft.roles).toEqual({ model: null, subagent_model: "zai/glm-5" });
    expect(loaded.skipped).toEqual(["Model for large tasks"]);
    expect(profileNote("Night shift", loaded.applied.length, loaded.skipped)).toBe(
      "Loaded “Night shift” — Apply to switch. Skipped (not settable here): Model for large tasks.",
    );
    expect(switchRequest(acme, loaded.draft, "new")).toEqual({ scope: "org", org: "acme", roles: { model: null, subagent_model: "zai/glm-5" }, apply: "new" });
  });

  it("lists saved profiles and starters, with rename and delete only for saved ones", () => {
    const html = render({
      initialOpen: true,
      initialProfiles: [profile({ model: "sonnet" }), profile({ model: "opus" }, { id: "starter-claude", name: "Claude only", builtin: true })],
    });
    expect(html).toContain('aria-label="model profiles"');
    expect(html).toContain('<optgroup label="Saved"><option value="p-1" title="orchestrator sonnet">Night shift</option></optgroup>');
    expect(html).toContain('<optgroup label="Starters"><option value="starter-claude" title="orchestrator opus">Claude only</option></optgroup>');
    expect(html).toContain("Save as profile…");
  });

  it("saves, renames and deletes profiles on the mock install, which the demo shows", async () => {
    const api = createMockApi();
    const { profiles } = await api.modelProfiles();
    expect(profiles.some((p) => !p.builtin)).toBe(true);
    expect(profiles.some((p) => p.builtin && p.name === "Claude only")).toBe(true);
    const saved = await api.createModelProfile({ name: "Cheap crew", module: "claude-code", roles: { model: "sonnet" } });
    await expect(api.createModelProfile({ name: "cheap CREW", roles: { model: "opus" } })).rejects.toThrow(/already exists/);
    expect((await api.updateModelProfile(saved.id, { name: "Cheaper crew" })).name).toBe("Cheaper crew");
    await expect(api.deleteModelProfile("starter-claude")).rejects.toThrow(/starter/);
    await api.deleteModelProfile(saved.id);
    expect((await api.modelProfiles()).profiles.some((p) => p.id === saved.id)).toBe(false);

    const { plans } = await api.modelPlans();
    expect(plans.find((p) => p.kind === "claude")?.used_by).toContain("orchestrator");
    expect(plans.some((p) => p.exhausted), "an exhausted plan to show").toBe(true);
    expect(plans.some((p) => p.balance?.pct_left != null), "a plan with a bar").toBe(true);
  });
});

describe("the demo", () => {
  it("serves the assignments from the mock install and switches them", async () => {
    const api = createMockApi();
    const a = await api.modelAssignments();
    expect(shortModelName(a.install.roles.find((r) => r.role === "model")!.value, a.models)).toBe("Opus 5.5");
    expect(a.install.roles.map((r) => r.role)).toEqual(["model", "subagent_model", "background_model", "summary_model", "model_low", "model_high", "account_fallback_model"]);
    const acme = a.orgs.find((o) => o.org === "acme")!;
    expect(acme.roles.find((r) => r.role === "model")).toMatchObject({ value: "strix/ds4-flash", source: "org" });
    expect(a.models.some((m) => m.out_of_quota), "a disabled model to show").toBe(true);
    expect(a.modules.some((m) => m.blocked), "a module that can't launch").toBe(true);

    await expect(api.switchModels({ scope: "install", roles: { small_model: "sonnet" } })).rejects.toThrow(/not a model role/);
    const out = a.models.find((m) => m.out_of_quota)!;
    await expect(api.switchModels({ scope: "install", roles: { model: out.id } })).rejects.toThrow(/out of quota/);

    const plan = await api.switchModels({ scope: "install", roles: { model: "sonnet" }, apply: "running", dry_run: true });
    expect(plan.affected.length).toBeGreaterThan(0);
    expect((await api.modelAssignments()).install.roles[0].value).toBe("claude-opus-5-5");
    await api.switchModels({ scope: "install", roles: { model: "sonnet" } });
    expect((await api.modelAssignments()).install.roles[0].value).toBe("sonnet");

    await api.switchModels({ scope: "org", org: "acme", roles: { model: null } });
    const after = (await api.modelAssignments()).orgs.find((o) => o.org === "acme")!;
    expect(after.roles.find((r) => r.role === "model")).toMatchObject({ value: "sonnet", source: "install" });

    const html = renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <ModelSwitcher initialAssignments={await api.modelAssignments()} initialOpen initialRecent={[]} />
      </ApiContext.Provider>,
    );
    expect(html).toContain('aria-label="Models · main model Sonnet"');
    expect(html).toContain('role="dialog"');
  });
});

describe("the account fallback role and the leftover Claude report (issue #1130)", () => {
  const withFallback: ModelAssignments = {
    ...ASSIGNMENTS,
    install: { ...ASSIGNMENTS.install, roles: [...ASSIGNMENTS.install.roles, row("account_fallback_model", "If Claude runs out, use", "zai/glm-5", "install", false)] },
  };

  it("offers only models on other providers, with an Off choice and a plain note", () => {
    const providers = groupsForRole("account_fallback_model", groupModels(MODELS)).map((g) => g.provider);
    expect(providers).toEqual(["zai", "strix", "bailian"]);
    expect(groupsForRole("model", groupModels(MODELS)).map((g) => g.provider)).toContain("anthropic");
    expect(inheritOptionLabel("account_fallback_model", "Module default")).toBe("Off — wait for the reset");
    expect(inheritOptionLabel("model", "Module default")).toBe("Module default");

    const html = render({ initialAssignments: withFallback, initialOpen: true });
    expect(html).toContain('aria-label="If Claude runs out, use model"');
    expect(html).toContain(ACCOUNT_FALLBACK_NOTE);
    expect(html).toContain("Off — wait for the reset");
  });

  const left = {
    colonies: [{ id: "c1", role: "model", model: "opus" }],
    orgs: [{ org: "acme", role: "subagent_model", model: "sonnet" }],
    cleared: false,
  };

  it("words the leftovers and clears them with a request for the same scope", () => {
    expect(hasLeftovers(left)).toBe(true);
    expect(hasLeftovers({ ...left, cleared: true })).toBe(false);
    expect(hasLeftovers({ colonies: [], orgs: [], cleared: false })).toBe(false);
    expect(hasLeftovers(undefined)).toBe(false);
    expect(leftoverLine(left)).toBe("1 colony and 1 org override still name a Claude model (opus, sonnet), which uses the Claude plan.");
    expect(clearLeftoversRequest(INSTALL_SCOPE)).toEqual({ scope: "install", roles: {}, apply: "new", clear_leftovers: true });
    expect(clearLeftoversRequest({ kind: "org", org: "acme" })).toEqual({ scope: "org", org: "acme", roles: {}, apply: "new", clear_leftovers: true });
  });

  it("lists them in the popover with the clear option", () => {
    const html = render({ initialOpen: true, initialLeftovers: left });
    expect(html).toContain("data-leftovers");
    expect(html).toContain("colony c1 · model opus");
    expect(html).toContain("org acme · subagent_model sonnet");
    expect(html).toContain("Clear these too");
    expect(render({ initialOpen: true })).not.toContain("Clear these too");
  });
});

describe("the judge row (issue #1201)", () => {
  const provider = (over: Partial<ModelProvider> & Pick<ModelProvider, "id" | "name">): ModelProvider =>
    ({ base_url: "https://example.test/v1", has_key: true, models: [], ...over }) as ModelProvider;
  const PROVIDERS = [provider({ id: "zai", name: "Z.AI", models: ["glm-5"] }), provider({ id: "strix", name: "Strix Halo", models: ["ds4-flash"] })];
  const ANTHROPIC_API = provider({ id: "claude-api", name: "Claude API", base_url: "https://api.anthropic.com", models: ["claude-x"] });
  const AUTONOMY = {
    kind: "autonomy",
    provider: "judge",
    providers: [],
    enabled: true,
    settings: { model: "zai/glm-5", fallback_models: "strix/ds4-flash", answer_limit: 5 },
    schema: null,
  } as ModuleInfo;
  const status = (over: Partial<AutonomyStatus> = {}): AutonomyStatus => ({
    enabled: true,
    model: "zai/glm-5",
    fallback_models: [],
    last_success: null,
    last_error: null,
    consecutive_failures: 0,
    alerted: false,
    ...over,
  });
  const open = (extra: Partial<ModelSwitcherProps> = {}) =>
    render({ initialOpen: true, initialAutonomy: AUTONOMY, initialProviders: PROVIDERS, ...extra });

  it("renders the current model and its health in its own section", () => {
    const html = open({ judge: status() });
    expect(html).toContain('aria-label="judge model"');
    expect(html).toContain("Answers colonies&#x27; questions for you");
    expect(html).toMatch(/<option value="zai\/glm-5" selected="">glm-5<\/option>/);
    expect(html).toContain('data-health="ok"');
    expect(html).toContain("answering");
    expect(html).not.toContain("data-judge-warning");
  });

  it("shows amber after a failure and red with a chip warning once it is failing", () => {
    expect(judgeTone(status({ consecutive_failures: 1 }))).toBe("warn");
    expect(judgeTone(status({ consecutive_failures: 3 }))).toBe("err");
    expect(judgeTone(null)).toBe("unknown");
    const html = open({ judge: status({ consecutive_failures: 4 }) });
    expect(html).toContain("data-judge-warning");
    expect(html).toContain("judge failing");
  });

  it("suggests a healthy alternative while failing", () => {
    const options = judgeOptions(PROVIDERS);
    expect(judgeAlternative(options, "strix/ds4-flash", MODELS)?.id).toBe("zai/glm-5");
    expect(judgeAlternative(options, "zai/glm-5", MODELS)).toBeNull();
  });

  it("choosing a model saves only the model and keeps the other autonomy settings", () => {
    expect(judgeSaveBody(AUTONOMY, "strix/ds4-flash")).toEqual({
      provider: "judge",
      enabled: true,
      settings: { model: "strix/ds4-flash", fallback_models: "strix/ds4-flash", answer_limit: 5 },
    });
    expect(AUTONOMY.settings.model).toBe("zai/glm-5");
  });

  it("offers fable and opus only with an api.anthropic.com provider that has a key", () => {
    expect(judgeOptions(PROVIDERS).map((o) => o.id)).toEqual(["zai/glm-5", "strix/ds4-flash"]);
    expect(judgeOptions([...PROVIDERS, ANTHROPIC_API]).map((o) => o.id)).toEqual(["fable", "opus", "zai/glm-5", "strix/ds4-flash", "claude-api/claude-x"]);
    expect(judgeOptions([...PROVIDERS, { ...ANTHROPIC_API, has_key: false }]).map((o) => o.id)).not.toContain("opus");
    expect(open({ judge: status() })).not.toContain('value="opus"');
    expect(open({ judge: status(), initialProviders: [...PROVIDERS, ANTHROPIC_API] })).toContain('value="opus"');
  });
});
