// The "Subscriptions" group of the Model providers page (issue #1211): every AI account a colony
// can sign in with, one collapsed row each in the same style as the API providers below. Claude is
// the subscription with a real login; Codex and Grok run on the vendor's API key; OpenCode, Pi,
// Hermes and ACP have no sign-in of their own and ride on the providers. Each row says Connected or
// Not connected, the account, and where known the usage window, then opens to its sign-in.
import { useState } from "react";

import type { AgentLogin, HarnessStatus, ModelOption, PlanUsage, ProviderPreset } from "../../types";
import { IconChevron } from "../icons";
import { ClaudeLoginSection } from "../Connections";
import { ProviderMark } from "../providerMark";
import { Badge, Button, Spinner, timeAgo } from "../ui";
import { Block, Chips, Facts, Shell, useUsage } from "./ProviderRows";
import { claudePlanText, type StatusTone } from "./providerOverview";

// ---------------------------------------------------------------------------
// Claude's health badge: the background per-account check's verdict (issue #983), with when it ran.
// No badge for an older mothership that sends none.
// ---------------------------------------------------------------------------

const CLAUDE_HEALTH: Record<NonNullable<HarnessStatus["claude"]["health_status"]>, { tone: "ok" | "err" | "warn" | "neutral"; label: string }> = {
  ok: { tone: "ok", label: "Reachable" },
  auth_expired: { tone: "err", label: "Token rejected" },
  unreachable: { tone: "warn", label: "Unreachable" },
  unchecked: { tone: "neutral", label: "Not checked yet" },
};

export function ClaudeHealth({ status, checkedAt }: { status: HarnessStatus["claude"]["health_status"]; checkedAt: string | null | undefined }) {
  if (!status) return null;
  const health = CLAUDE_HEALTH[status];
  return (
    <p className="flex flex-wrap items-center gap-2 text-small-lg text-muted">
      <Badge tone={health.tone}>{health.label}</Badge>
      {checkedAt ? <span>checked {timeAgo(checkedAt)}</span> : null}
    </p>
  );
}

/**
 * Claude: the default the harness falls back to. Its limit state and reset come from the account
 * quota in /api/models/plans; its sign-in (subscription login, token or API key, health re-check)
 * is the whole Claude connection that used to live under Connections.
 */
export function ClaudeSubscriptionRow({
  claude,
  models,
  plan,
  open,
  onToggle,
  onStatusChanged,
}: {
  claude: HarnessStatus["claude"] | null;
  models: ModelOption[];
  plan: PlanUsage | null;
  open: boolean;
  onToggle: () => void;
  onStatusChanged: (fresh?: boolean) => Promise<void> | void;
}) {
  const [checking, setChecking] = useState(false);
  const nowMs = Date.now();
  const own = models.filter((m) => m.provider === "anthropic");
  const state = claudePlanText(plan, nowMs);
  const connected = claude?.configured;
  const tone: StatusTone = connected === false ? "err" : state.tone === "idle" ? (connected ? "ok" : "idle") : state.tone;
  const label = connected === false ? "Not connected" : state.status === "Limit reached" ? state.status : "Connected";
  const { report } = useUsage("anthropic", open);
  const last = plan?.last_limit;
  const recheck = async () => {
    setChecking(true);
    try {
      await onStatusChanged(true);
    } finally {
      setChecking(false);
    }
  };
  return (
    <Shell
      id="anthropic"
      mark={<ProviderMark preset="anthropic" name="Claude" />}
      name="Claude"
      tag={<Badge>Subscription</Badge>}
      statusLabel={label}
      tone={tone}
      gauge={{ pct: state.tone === "err" ? 0 : null, text: state.tone === "err" ? "0%" : connected ? (claude?.account ?? "subscription") : "not signed in", tone: state.tone === "err" ? "err" : "idle" }}
      reset={state.reset}
      open={open}
      onToggle={onToggle}
    >
      <Block title="Usage window" help="Claude reports its session and weekly limits only once one is hit.">
        <p className="text-body-sm">
          {plan?.exhausted ? <span className="text-err">Limit reached{state.reset ? `, ${state.reset}` : ""}.</span> : "Within its limits right now."}
        </p>
        <p className="mt-1 text-small text-faint">
          {last ? `Last limit hit ${timeAgo(last.at) ?? "recently"}.` : "No limit hit since this Mothership started keeping track."}
          {report && report.events.length > 0 ? ` ${report.events.filter((e) => e.kind === "exhausted").length} in the last ${report.days} days.` : ""}
        </p>
      </Block>
      <Block title="Models" help="The default. A model id with no provider prefix, and a provider's fallback, go here.">
        {own.length === 0 ? <p className="text-small text-faint">Model list not loaded.</p> : <Chips items={own.map((m) => m.id)} limit={8} />}
      </Block>
      <Block
        title="Sign-in"
        help="Log in runs claude setup-token on the Mothership and saves a 1-year token. microVMs only see a placeholder; the real token is swapped in for requests to api.anthropic.com."
      >
        <Facts rows={[["Account", connected ? [claude?.account ?? "account not identified", claude?.source].filter(Boolean).join(" · ") : "Not connected"]]} />
        {connected && (
          <div className="mt-2.5 flex flex-wrap items-center gap-2">
            <ClaudeHealth status={claude?.health_status} checkedAt={claude?.health_checked_at} />
            <Button size="sm" disabled={checking} onClick={() => void recheck()} title="Ask the Mothership to look again">
              {checking && <Spinner className="size-3" />} Check again
            </Button>
          </div>
        )}
        <div className="mt-3 space-y-3">
          <ClaudeLoginSection claude={claude} onStatusChanged={onStatusChanged} />
        </div>
      </Block>
    </Shell>
  );
}

// ---------------------------------------------------------------------------
// The other agents
// ---------------------------------------------------------------------------

/** Where an agent's credential comes from, in a few words (the collapsed row's gauge column). */
function kindText(agent: AgentLogin): string {
  if (!agent.signed_in) return "not connected";
  if (agent.kind === "gateway") return "via providers";
  return agent.kind === "subscription" ? "subscription" : "API key";
}

const MARK_PRESET: Record<string, string> = { codex: "openai", "grok-build": "xai-grok" };
/** The provider an agent's own key is added as: the one the runner reads. */
const KEY_PRESET: Record<string, string> = { codex: "openai", "grok-build": "xai-grok" };

/** What the person should know about how this agent signs in, from its module's README. */
const SIGN_IN_HELP: Record<string, string> = {
  codex: "Codex runs on an OpenAI API key: a ChatGPT plan sign-in is not a credential a colony can use. Add the key as a provider, or set CODEX_API_KEY on the Mothership.",
  "grok-build": "Grok Build runs on an xAI API key and never signs in interactively. Add the key as a provider, or set XAI_API_KEY on the Mothership.",
};

export function AgentSubscriptionRow({
  agent,
  open,
  isOrchestrator,
  onToggle,
  onAddKey,
  onAddProvider,
}: {
  agent: AgentLogin;
  open: boolean;
  /** This is the agent module the orchestrator runs on. */
  isOrchestrator: boolean;
  onToggle: () => void;
  /** Open the add-provider form for this agent's own key (Codex, Grok). */
  onAddKey: (preset: string) => void;
  onAddProvider: () => void;
}) {
  const keyPreset = KEY_PRESET[agent.id];
  return (
    <Shell
      id={`agent-${agent.id}`}
      mark={<ProviderMark preset={(MARK_PRESET[agent.id] ?? "custom") as ProviderPreset} name={agent.name} />}
      name={agent.name}
      tag={isOrchestrator ? <Badge tone="info">Active agent</Badge> : <Badge>{agent.kind === "gateway" ? "Via providers" : "Agent login"}</Badge>}
      statusLabel={agent.signed_in ? "Connected" : "Not connected"}
      tone={agent.signed_in ? "ok" : "idle"}
      gauge={{ pct: null, text: kindText(agent), tone: "idle" }}
      reset={null}
      open={open}
      onToggle={onToggle}
    >
      <Block
        title="Sign-in"
        help={SIGN_IN_HELP[agent.id] ?? "No sign-in of its own: this agent reaches models only through the API providers below, so any provider with a key (or a local server) connects it."}
      >
        <Facts
          rows={[
            ["Account", agent.signed_in ? (agent.account ?? "Connected") : "Not connected"],
            ...(agent.checked_at ? ([["Checked", timeAgo(agent.checked_at) ?? "just now"]] as [string, string][]) : []),
          ]}
        />
        <div className="mt-3">
          {keyPreset ? (
            <Button size="sm" onClick={() => onAddKey(keyPreset)}>
              Use a token or API key instead <IconChevron size={13} />
            </Button>
          ) : (
            <Button size="sm" onClick={onAddProvider}>
              Add a provider <IconChevron size={13} />
            </Button>
          )}
        </div>
      </Block>
    </Shell>
  );
}
