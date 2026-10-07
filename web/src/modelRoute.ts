// Which route the orchestrator model takes, and whether a colony can use it (issue #1211). The setup
// checklist's required row, the Models page's status line and the needs-you dot all read this one
// verdict, so they cannot disagree. Pure over GET /api/status: a Claude login, a provider with a key
// and a local server are all the same thing here, a way to a model.
import type { AgentLogin, HarnessStatus, ModelProviderStatus } from "./types";

export interface ModelRoute {
  /** The orchestrator model resolves to a route that works right now. */
  ok: boolean;
  /** What carries it: "Claude", "MiniMax", "Codex", or empty when nothing does. */
  via: string;
  /** One line for the row: "Claude subscription · me@x.dev", "MiniMax · minimax/m2". */
  detail: string;
  /** Why not, in words, when `ok` is false. */
  reason?: string;
  /** Another route exists that the orchestrator could be switched to, when this one fails. */
  alternative?: string;
}

/** A provider that can carry a colony: it has a key (or needs none) and its plan is not out. */
function providerUsable(p: ModelProviderStatus, exhausted: readonly string[]): boolean {
  const reachable = p.has_key !== false || p.keyless === true;
  return reachable && !exhausted.includes(p.id) && !(p.degraded && p.failure_pct >= 25);
}

const claudeDetail = (claude: HarnessStatus["claude"]): string =>
  [claude.source ?? "Configured", claude.account].filter(Boolean).join(" · ");

/** The prefix of a `provider/model` id, or null for a plain model id such as `sonnet`. */
function providerOf(model: string): string | null {
  const at = model.indexOf("/");
  return at > 0 ? model.slice(0, at) : null;
}

/** Another way to a model, named, for the "switch to it" hint; null when there is none. */
function otherRoute(status: HarnessStatus, skip: string): string | null {
  const exhausted = status.quota?.providers ?? [];
  const provider = (status.model_providers ?? []).find((p) => p.id !== skip && p.has_key !== false && providerUsable(p, exhausted));
  if (provider) return provider.name;
  if (status.claude.configured && skip !== "claude") return "Claude";
  const agent = (status.agents ?? []).find((a) => a.signed_in && a.id !== "claude-code");
  return agent ? agent.name : null;
}

/** Older mothership builds send no `agents` or `orchestrator`; they only ever knew Claude. */
export function modelRoute(status: HarnessStatus): ModelRoute {
  const exhausted = status.quota?.providers ?? [];
  const providers = status.model_providers ?? [];
  const orch = status.orchestrator;
  const model = (orch?.model ?? "").trim();
  const moduleId = orch?.module ?? "claude-code";
  const agents: AgentLogin[] = status.agents ?? [];
  const agent = agents.find((a) => a.id === moduleId);

  const fail = (reason: string, skip: string): ModelRoute => {
    const alt = otherRoute(status, skip);
    return { ok: false, via: "", detail: "Not connected", reason, alternative: alt ?? undefined };
  };

  // A named provider route: `minimax/m2` goes to the gateway whatever the agent module is.
  const prefix = providerOf(model);
  if (prefix) {
    const provider = providers.find((p) => p.id === prefix);
    if (!provider) return fail(`The orchestrator model ${model} names a provider that is not set up.`, prefix);
    if (provider.has_key === false && provider.keyless !== true) return fail(`${provider.name} has no key yet.`, provider.id);
    if (exhausted.includes(provider.id)) return fail(`${provider.name}'s plan is out for now.`, provider.id);
    if (provider.degraded && provider.failure_pct >= 25) return fail(`${provider.name} is failing most requests.`, provider.id);
    return { ok: true, via: provider.name, detail: `${provider.name} · ${model}` };
  }

  // A plain model id on Claude Code (or an older mothership) is the Claude login.
  if (moduleId === "claude-code" || (!agent && !status.agents)) {
    if (status.claude.configured) return { ok: true, via: "Claude", detail: claudeDetail(status.claude) };
    return fail("Claude is not signed in.", "claude");
  }

  // Any other agent module: its own credential, or the gateway's providers.
  if (agent?.signed_in) {
    return { ok: true, via: agent.name, detail: [agent.name, agent.account].filter(Boolean).join(" · ") };
  }
  if (agent?.kind === "gateway") {
    return fail(`${agent.name} reaches models through a provider, and none has a key.`, "");
  }
  return fail(`${agent?.name ?? moduleId} has no credential.`, moduleId);
}
