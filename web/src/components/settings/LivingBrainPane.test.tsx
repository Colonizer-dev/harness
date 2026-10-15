// Settings → Living Brain, rendered to static markup like the other settings tests: the status
// line for each connection state, the device code, a rule row, the queue and the backfill count.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { DeviceCode, QueueStatus, RuleRow, backfillText, livingBrainStatusText, queueText } from "./LivingBrainPane";
import { LivingBrainAnchor } from "../../features/livingBrain/LivingBrainLink";
import type { LivingBrainQueue, LivingBrainState } from "../../types";

const queue = (over: Partial<LivingBrainQueue> = {}): LivingBrainQueue => ({ pending: 0, sent: 0, failed: 0, last_sync: null, last_error: null, items: [], ...over });

const state = (over: Partial<LivingBrainState>): LivingBrainState => ({
  state: "disconnected",
  server_url: "https://app.livingbrain.wiki",
  mcp_url: "https://mcp.livingbrain.wiki/v1/pages/mcp",
  connected_at: null,
  token_prefix: null,
  token_name: null,
  auth_error: null,
  device: null,
  rules: [],
  mcp_agents: ["claude-code", "codex"],
  queue: queue(),
  ...over,
});

describe("livingBrainStatusText", () => {
  it("says who the mothership is connected as, and asks for a reconnect after a 401", () => {
    expect(livingBrainStatusText(state({}))).toBe("Not connected.");
    expect(livingBrainStatusText(state({ state: "connected", token_name: "colonizer", token_prefix: "lbp_01J" }))).toBe("Connected with token “colonizer” (lbp_01J).");
    expect(livingBrainStatusText(state({ state: "connected" }))).toBe("Connected with a personal access token.");
    expect(livingBrainStatusText(state({ state: "unauthorized" }))).toMatch(/refused the saved token/);
  });
});

describe("DeviceCode", () => {
  const device = { state: "pending" as const, user_code: "WDJB-MJHT", verification_uri: "https://brain/device", verification_uri_complete: "https://brain/device?code=WDJB-MJHT", expires_at: "2099-01-01T00:00:00Z" };
  it("shows the code and links the page that takes it", () => {
    const html = renderToStaticMarkup(<DeviceCode device={device} onCancel={() => {}} />);
    expect(html).toContain("WDJB-MJHT");
    expect(html).toContain('href="https://brain/device?code=WDJB-MJHT"');
    expect(html).toContain("Waiting for approval");
  });
  it("says how a sign-in ended", () => {
    expect(renderToStaticMarkup(<DeviceCode device={{ ...device, state: "denied" }} onCancel={() => {}} />)).toContain("denied");
    expect(renderToStaticMarkup(<DeviceCode device={{ ...device, state: "expired" }} onCancel={() => {}} />)).toContain("expired");
  });
});

describe("RuleRow", () => {
  it("offers the send switch, the detail level and the MCP switch, and names the agents that get it", () => {
    const html = renderToStaticMarkup(
      <RuleRow rule={{ target: "World-360", send: true, detail: "messages", mcp: true, since: "2026-10-15T00:00:00Z" }} mcpAgents={["claude-code", "codex"]} busy={false} onChange={() => {}} onRemove={() => {}} />,
    );
    expect(html).toContain("World-360");
    expect(html).toContain(">org<");
    expect(html).toContain("Send colony sessions");
    expect(html).toContain("Give colonies the Living Brain MCP server");
    expect(html).toContain("claude-code or codex");
    expect(html).toContain('value="summary"');
    expect(html).toContain("older ones wait");
  });
});

describe("the queue", () => {
  it("counts, owns up to failures and links what was sent", () => {
    expect(queueText(queue())).toBe("0 sent, 0 pending. Nothing sent yet.");
    expect(queueText(queue({ sent: 3, pending: 1, failed: 2 }))).toMatch(/^3 sent, 1 pending, 2 failed\./);
    const html = renderToStaticMarkup(
      <QueueStatus
        queue={queue({ failed: 1, last_error: "Living Brain answered 413", items: [{ colony: "abc", repo: "World-360/app", state: "sent", reason: "merged", attempts: 0, last_error: null, queued_at: null, sent_at: null, note_url: "https://brain/n/1" }] })}
        busy={false}
        onRetry={() => {}}
      />,
    );
    expect(html).toContain("Retry failed");
    expect(html).toContain("Living Brain answered 413");
    expect(html).toContain('href="https://brain/n/1"');
  });
  it("words the backfill count", () => {
    expect(backfillText({ org: "World-360", dry_run: true, eligible: 5, already_sent: 2, to_send: 3, queued: 0, not_opted_in: 1, still_running: 0 })).toBe(
      "3 of World-360's colonies would be sent (2 already sent, 1 in repositories that do not send).",
    );
  });
});

describe("LivingBrainAnchor", () => {
  it("links a sent colony's note and renders nothing otherwise", () => {
    expect(renderToStaticMarkup(<LivingBrainAnchor colony={null} />)).toBe("");
    expect(renderToStaticMarkup(<LivingBrainAnchor colony={{ state: "pending", note_url: null, sent_at: null, last_error: null, sessions_url: "x" }} />)).toBe("");
    expect(renderToStaticMarkup(<LivingBrainAnchor colony={{ state: "sent", note_url: "https://brain/n/1", sent_at: null, last_error: null, sessions_url: "x" }} />)).toContain('href="https://brain/n/1"');
  });
});
