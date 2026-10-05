// The header model switcher's mock methods (issue #1051): the mock install's agent module settings
// (ms.modules) and org overrides (ms.orgSettings) resolved per role, as the mothership does, so
// `npm run build:demo` shows the switcher and a switch is visible in Settings afterwards.
import { isLive } from "../../components/ui";
import { ApiError } from "../../http";
import { clone, sleep } from "../../mockShared";
import type { MockState } from "../../mockState";
import type { OrgSettings } from "../orgs/types";
import type { ModelsApi } from "./api";
import type { AgentModuleRoles, ModelAssignments, ModelRoleRow, ModelSource, ModelSwitchReply, ModelSwitchRequest, SwitchableModel } from "./types";

const ORG_ROLES = ["model", "subagent_model", "background_model"];

const role = (id: string, title: string) => ({ role: id, title, org_settable: ORG_ROLES.includes(id) });

/** The agent modules a stock install carries, with the roles their module.json declares. */
export const MOCK_AGENT_MODULES: AgentModuleRoles[] = [
  {
    id: "claude-code",
    name: "Claude Code",
    roles: [
      role("model", "Orchestrator model"),
      role("subagent_model", "Subagent model"),
      role("background_model", "Background model"),
      role("summary_model", "Summary model"),
      role("model_low", "Model for small tasks"),
      role("model_high", "Model for large tasks"),
    ],
    blocked: null,
  },
  { id: "codex", name: "Codex", roles: [role("model", "Orchestrator model"), role("subagent_model", "Subagent model"), role("background_model", "Background model")], blocked: null },
  { id: "opencode", name: "OpenCode", roles: [role("model", "Model"), role("small_model", "Small model")], blocked: null },
  { id: "pi", name: "Pi", roles: [role("model", "Model")], blocked: null },
  { id: "hermes", name: "Hermes", roles: [role("model", "Orchestrator model")], blocked: null },
  {
    id: "acp",
    name: "ACP",
    roles: [role("model", "Model")],
    // The ACP-grok binary is not staged into the colony image yet (docs/cockpit.md).
    blocked:
      "agent module `acp` needs the `grok` binary, which the harness does not stage and the stock preset images (node:24-bookworm among them) do not all carry; set the sandbox module's image to one with grok on PATH",
  },
];

/** The demo's lab plan is out until five hours from now, so the pickers show a disabled model. */
const LAB_RESET_UNIX = Math.floor(Date.now() / 1000) + 5 * 3600;

function mockModels(ms: MockState): SwitchableModel[] {
  const claude: SwitchableModel[] = ms.ANTHROPIC_MODELS.map(([id, label]) => ({
    id,
    label,
    provider: "anthropic",
    provider_name: "Anthropic",
    wire: null,
    failure_pct: 0,
    rated: false,
    degraded: false,
    healthy: true,
    out_of_quota: false,
    reset_at: null,
    reset_unix: null,
  }));
  const routed = ms.providers.flatMap((p) => {
    const lab = p.id === "lab";
    const health = p.health ?? ms.zeroHealth();
    const out = !!p.quota_exhausted || lab;
    const reset_unix = p.quota_exhausted?.reset_unix ?? (lab ? LAB_RESET_UNIX : null);
    return p.models.map(
      (m): SwitchableModel => ({
        id: `${p.id}/${m}`,
        label: `${m} · ${p.name}`,
        provider: p.id,
        provider_name: p.name,
        wire: p.wire,
        failure_pct: health.failure_pct,
        rated: health.rated,
        degraded: health.degraded || out,
        healthy: !health.degraded && !out,
        out_of_quota: out,
        reset_at: p.quota_exhausted?.reset_at ?? (lab ? new Date(LAB_RESET_UNIX * 1000).toISOString().slice(5, 16).replace("T", " ") + " UTC" : null),
        reset_unix: out ? reset_unix : null,
      }),
    );
  });
  return [...claude, ...routed];
}

function agentEntry(ms: MockState) {
  const agent = ms.modules.find((m) => m.kind === "agent");
  if (!agent) throw new ApiError("no agent module", 500);
  return agent;
}

function orgOverride(settings: OrgSettings | undefined, role: string): string | null | undefined {
  const agent = settings?.agent;
  if (!agent) return undefined;
  if (role === "model") return agent.model;
  if (role === "subagent_model") return agent.subagent_model;
  if (role === "background_model") return agent.background_model;
  return undefined;
}

function resolve(ms: MockState, org: OrgSettings | null, module: string, roleId: string): [string, ModelSource] {
  const own = org ? orgOverride(org, roleId) : undefined;
  if (own != null) return [own, "org"];
  const agent = agentEntry(ms);
  const value = agent.settings[roleId];
  if (module === agent.provider && typeof value === "string" && value !== "") return [value, "install"];
  return ["", "default"];
}

function rows(ms: MockState, org: OrgSettings | null, module: string): ModelRoleRow[] {
  const spec = MOCK_AGENT_MODULES.find((m) => m.id === module);
  return (spec?.roles ?? []).map((r) => {
    const [value, source] = resolve(ms, org, module, r.role);
    return { ...r, value, source };
  });
}

function orgNames(ms: MockState): string[] {
  const names = new Set<string>(Object.keys(ms.orgSettings));
  for (const s of ms.sessions.values()) names.add(s.session.org || s.session.repo.split("/")[0]);
  return [...names].sort();
}

function moduleOf(ms: MockState, org: OrgSettings | undefined): string {
  return org?.agent?.module || agentEntry(ms).provider;
}

function assignments(ms: MockState): ModelAssignments {
  const install = agentEntry(ms).provider;
  return {
    install: { module: install, roles: rows(ms, null, install) },
    orgs: orgNames(ms).map((org) => {
      const settings = ms.orgSettings[org] ?? {};
      const module = moduleOf(ms, settings);
      return { org, module, module_source: settings.agent?.module ? "org" : "install", roles: rows(ms, settings, module) };
    }),
    modules: MOCK_AGENT_MODULES,
    models: mockModels(ms),
  };
}

function switchModels(ms: MockState, req: ModelSwitchRequest): ModelSwitchReply {
  const agent = agentEntry(ms);
  const org = req.scope === "org" ? (req.org ?? "") : null;
  if (req.scope === "org" && !org) throw new ApiError("an org switch names a valid GitHub org", 400);
  const orgSettings = org ? (ms.orgSettings[org] ?? {}) : null;
  const current = org ? moduleOf(ms, orgSettings ?? undefined) : agent.provider;
  const module = req.module === undefined ? current : req.module === "" ? agent.provider : req.module;
  const spec = MOCK_AGENT_MODULES.find((m) => m.id === module);
  if (!spec) throw new ApiError(`unknown agent module "${module}"`, 400);
  if (module !== current && spec.blocked) throw new ApiError(`${module} can't launch on this install: ${spec.blocked}`, 400);
  const offered = mockModels(ms);
  const roles = Object.entries(req.roles);
  for (const [r, value] of roles) {
    if (!spec.roles.some((x) => x.role === r)) throw new ApiError(`\`${r}\` is not a model role of ${module}`, 400);
    if (org && !ORG_ROLES.includes(r)) throw new ApiError(`\`${r}\` is set install-wide only; switch it under All orgs`, 400);
    if (value) {
      const option = offered.find((m) => m.id === value);
      if (!option) throw new ApiError(`${r}: ${value} is not a model on offer`, 400);
      if (option.out_of_quota) throw new ApiError(`${r}: ${value} is out of quota; pick a model on another provider`, 400);
    }
  }
  if (roles.length === 0 && req.module === undefined) throw new ApiError("nothing to switch: name a module or at least one role", 400);

  const changes: ModelSwitchReply["changes"] = [];
  const before = new Map(roles.map(([r]) => [r, resolve(ms, orgSettings, current, r)[0]]));
  // The colonies a running switch restarts: in play, on the scope's module, in the scope.
  const affected =
    req.apply === "running"
      ? [...ms.sessions.values()]
          .filter((s) => isLive(s.session.status) || s.session.status === "parked" || s.session.status === "queued")
          .filter((s) => (s.session.agent || agent.provider) === module)
          .filter((s) => !org || (s.session.org || s.session.repo.split("/")[0]) === org)
          .map((s) => s.session.id)
      : [];
  if (req.dry_run) return { dry_run: true, scope: req.scope, org, module, changes, affected, colonies: [], failed: [] };

  if (org) {
    const next: OrgSettings = clone(orgSettings ?? {});
    const a = (next.agent ??= {});
    if (req.module !== undefined) a.module = req.module === "" ? null : module;
    for (const [r, value] of roles) {
      if (r === "model") a.model = value || null;
      if (r === "subagent_model") a.subagent_model = value || null;
      if (r === "background_model") a.background_model = value || null;
    }
    ms.orgSettings[org] = next;
  } else {
    agent.provider = module;
    for (const [r, value] of roles) {
      if (value) agent.settings[r] = value;
      else delete agent.settings[r];
    }
  }
  for (const [r] of roles) {
    const now = resolve(ms, org ? ms.orgSettings[org] : null, module, r)[0];
    changes.push({ scope: org ? "org" : "install", target: org ?? module, key: r, was: before.get(r) ?? null, now });
  }
  ms.logActivity({ kind: "settings.save", actor: "you", via: "cockpit", org, target: "models", section: "module:agent" });
  return { dry_run: false, scope: req.scope, org, module, changes, affected, colonies: affected, failed: [] };
}

export function modelsMock(ms: MockState): ModelsApi {
  return {
    modelAssignments: async () => {
      await sleep(120);
      return clone(assignments(ms));
    },
    switchModels: async (body) => {
      await sleep(200);
      return clone(switchModels(ms, body));
    },
  };
}
