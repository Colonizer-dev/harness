// The Subscriptions group of the Model providers page (issue #1211): Claude keeps its whole connection
// (login, token, reachability, re-check) inside its row, and Codex, Grok and the other agents sit next
// to it with Connected / Not connected and a way to add a key. Nothing shows a token value.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../../api";
import { ApiContext } from "../../context";
import type { AgentLogin, HarnessStatus } from "../../types";
import { ConnectionsPane } from "./ConnectionsPane";
import { AgentSubscriptionRow, ClaudeSubscriptionRow } from "./SubscriptionRows";

const wrap = (node: React.ReactNode) => renderToStaticMarkup(<ApiContext.Provider value={{} as Api}>{node}</ApiContext.Provider>);

const claude = (over: Partial<HarnessStatus["claude"]> = {}): HarnessStatus["claude"] => ({
  configured: true,
  source: "Claude subscription",
  kind: "CLAUDE_CODE_OAUTH_TOKEN",
  account: "me@example.dev",
  health_status: "ok",
  health_checked_at: "2026-10-07T11:00:00Z",
  ...over,
});

const claudeRow = (c: HarnessStatus["claude"] | null, open = true) =>
  wrap(<ClaudeSubscriptionRow claude={c} models={[]} plan={null} open={open} onToggle={() => {}} onStatusChanged={() => {}} />);

const agent = (over: Partial<AgentLogin> = {}): AgentLogin => ({ id: "codex", name: "Codex", signed_in: true, kind: "api_key", account: "OpenAI API key", ...over });
const agentRow = (a: AgentLogin, open = true) =>
  wrap(<AgentSubscriptionRow agent={a} open={open} isOrchestrator={false} onToggle={() => {}} onAddKey={() => {}} onAddProvider={() => {}} />);

describe("the Claude subscription row", () => {
  it("is a collapsed row that says Connected and the account", () => {
    const out = claudeRow(claude(), false);
    expect(out).toContain("Claude");
    expect(out).toContain("Subscription");
    expect(out).toContain("Connected");
    expect(out).toContain('aria-expanded="false"');
    expect(out).not.toContain("Use a token or API key instead");
  });

  it("keeps the whole Claude connection once opened: login, token, reachability and Check again", () => {
    const out = claudeRow(claude());
    expect(out).toContain("me@example.dev");
    expect(out).toContain("Log in with Claude subscription");
    expect(out).toContain("Remove saved token");
    expect(out).toContain("Use a token or API key instead");
    expect(out).toContain("Reachable");
    expect(out).toContain("Check again");
  });

  it("says Not connected and offers the login without a re-check", () => {
    const out = claudeRow(claude({ configured: false, account: null, source: null }));
    expect(out).toContain("Not connected");
    expect(out).toContain("Log in with Claude subscription");
    expect(out).not.toContain("Check again");
  });

  it("shows a rejected token", () => {
    expect(claudeRow(claude({ health_status: "auth_expired" }))).toContain("Token rejected");
  });
});

describe("the other agents' rows", () => {
  it("shows Codex as connected through its API key, never a value", () => {
    const out = agentRow(agent());
    expect(out).toContain("Codex");
    expect(out).toContain("Connected");
    expect(out).toContain("OpenAI API key");
    expect(out).toContain("Use a token or API key instead");
  });

  it("shows Grok as not connected with the way to add a key", () => {
    const out = agentRow(agent({ id: "grok-build", name: "Grok Build", signed_in: false, account: null }));
    expect(out).toContain("Not connected");
    expect(out).toContain("xAI API key");
    expect(out).toContain("Use a token or API key instead");
  });

  it("shows a gateway-only agent as riding on the providers", () => {
    const out = agentRow(agent({ id: "opencode", name: "OpenCode", kind: "gateway", account: "via MiniMax" }));
    expect(out).toContain("via MiniMax");
    expect(out).toContain("Add a provider");
    expect(out).not.toContain("Use a token or API key instead");
  });

  it("marks the agent module the orchestrator runs on", () => {
    const out = wrap(<AgentSubscriptionRow agent={agent()} open={false} isOrchestrator onToggle={() => {}} onAddKey={() => {}} onAddProvider={() => {}} />);
    expect(out).toContain("Active agent");
  });
});

describe("the GitHub page", () => {
  const status = { github: { connected: true, login: "octocat", source: "gh CLI login" }, claude: claude() } as HarnessStatus;
  const out = wrap(<ConnectionsPane status={status} onStatusChanged={() => {}} />);

  it("holds only the code host", () => {
    expect(out).toContain("GitHub");
    expect(out).toContain("@octocat");
    expect(out).not.toContain("Claude");
    expect(out).not.toContain("Log in with Claude");
  });
});

describe("the Model providers page", () => {
  it("lists the providers next to Claude, subscriptions first", async () => {
    const { ProvidersPane } = await import("./ProvidersPane");
    const status = {
      github: { connected: true },
      claude: claude(),
      agents: [
        { id: "claude-code", name: "Claude Code", signed_in: true, kind: "subscription" },
        agent(),
        agent({ id: "opencode", name: "OpenCode", kind: "gateway", account: "via MiniMax" }),
      ],
      orchestrator: { module: "opencode", model: "minimax/MiniMax-M2" },
      model_providers: [{ id: "minimax", name: "MiniMax", has_key: true, keyless: false, requests: 1, failure_pct: 0, avg_latency_ms: 1, degraded: false }],
    } as unknown as HarnessStatus;
    const minimax = { id: "minimax", name: "MiniMax", base_url: "https://api.minimax.io/anthropic", auth: "x-api-key", wire: "anthropic", has_key: true, models: [], preset: "minimax" };
    const out = wrap(
      <ProvidersPane
        providers={[minimax as never]}
        error={null}
        setProviders={() => {}}
        reload={async () => {}}
        status={status}
        models={[]}
        onStatusChanged={() => {}}
      />,
    );
    const at = (text: string) => out.indexOf(text);
    const order = ['id="subscriptions-heading"', 'data-provider="anthropic"', 'data-provider="agent-codex"', 'data-provider="agent-opencode"', 'id="api-providers-heading"', 'data-provider="minimax"'].map(at);
    expect(order.every((n) => n > -1)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
    expect(out).toContain("Colonies can reach a model");
    expect(out).toContain("Switch the orchestrator model");
  });
});
