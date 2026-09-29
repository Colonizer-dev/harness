// The "Provider out of quota" card (issue #767): its header names the provider, the model and the
// reset with a countdown, it lists every blocked colony once, offers only models on healthy
// providers first, and its three actions send the request the API expects. Rendered to static
// markup (the test environment has no DOM), so the actions are pinned through the pure helpers.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import type { QuotaActionReply, QuotaCard } from "../types";
import { InboxView } from "./InboxView";
import {
  ProviderQuotaCard,
  alternativeLabel,
  canRemember,
  defaultAlternative,
  formatCountdown,
  formatResetUtc,
  quotaActionSummary,
  quotaCardColonyIds,
  quotaCardHeader,
  quotaWaitingLine,
  runQuotaAction,
  switchRequest,
} from "./ProviderQuotaCard";

// 2026-09-29T07:00:00Z; the reset below is Oct 1, 16:00 UTC — 2 days 9 hours later.
const NOW = Date.UTC(2026, 8, 29, 7, 0, 0);
const RESET = Date.UTC(2026, 9, 1, 16, 0, 0) / 1000;

function card(overrides: Partial<QuotaCard> = {}): QuotaCard {
  return {
    provider: "bailian",
    provider_name: "Bailian",
    models: ["qwen3.8-max"],
    title: "bailian · qwen3.8-max is out of quota",
    reset_at: "Oct 1, 16:00 UTC",
    reset_unix: RESET,
    colonies: [
      { id: "c1", repo: "acme/webshop", org: "acme", issue: 42, issue_title: "Checkout", status: "running", hits: 3, waiting: false, resume_unix: null },
      { id: "c2", repo: "acme/api", org: "acme", issue: 7, issue_title: "Auth", status: "starting", hits: 5, waiting: false, resume_unix: null },
    ],
    orgs: ["acme"],
    waiting: 0,
    resume_unix: null,
    fallback_model: null,
    alternatives: [
      { id: "zai/glm-5", label: "glm-5 · Z.AI", provider: "zai", failure_pct: 32.5, rated: true, degraded: true, healthy: false },
      { id: "sonnet", label: "Claude Sonnet (latest)", provider: "anthropic", failure_pct: 0, rated: false, degraded: false, healthy: true },
      { id: "deepseek/ds4", label: "ds4 · DeepSeek", provider: "deepseek", failure_pct: 1.2, rated: true, degraded: false, healthy: true },
    ],
    ...overrides,
  };
}

const reply = (overrides: Partial<QuotaActionReply> = {}): QuotaActionReply => ({
  action: "switch",
  provider: "bailian",
  colonies: ["c1", "c2"],
  failed: [],
  ...overrides,
});

describe("quota card words", () => {
  it("names the provider, the model and the reset with a countdown", () => {
    expect(formatResetUtc(RESET)).toBe("Oct 1, 16:00 UTC");
    expect(formatCountdown(RESET, NOW)).toBe("2d 9h");
    expect(quotaCardHeader(card(), NOW)).toBe(
      "bailian · qwen3.8-max is out of quota. Resets Oct 1, 16:00 UTC (in 2d 9h)",
    );
  });

  it("quotes a reset named only in words, and says when there is none", () => {
    expect(quotaCardHeader(card({ reset_unix: null, reset_at: "7am" }), NOW)).toBe(
      "bailian · qwen3.8-max is out of quota. Resets 7am",
    );
    expect(quotaCardHeader(card({ reset_unix: null, reset_at: null, models: [] }), NOW)).toMatch(
      /^bailian is out of quota\. No reset time given/,
    );
  });

  it("counts down hours, minutes and the end", () => {
    expect(formatCountdown(NOW / 1000 + 3 * 3600 + 12 * 60, NOW)).toBe("3h 12m");
    expect(formatCountdown(NOW / 1000 + 4 * 60 + 5, NOW)).toBe("4m");
    expect(formatCountdown(NOW / 1000 - 1, NOW)).toBe("now");
  });

  it("shows the wait's countdown only while colonies wait", () => {
    expect(quotaWaitingLine(card(), NOW)).toBeNull();
    expect(quotaWaitingLine(card({ waiting: 2, resume_unix: RESET }), NOW)).toBe("2 colonies wait — resuming in 2d 9h");
  });

  it("labels the picker with health and defaults to the first healthy model", () => {
    const [degraded, claude, deepseek] = card().alternatives;
    expect(alternativeLabel(degraded)).toBe("glm-5 · Z.AI — degraded, 32.5% failing");
    expect(alternativeLabel(claude)).toBe("Claude Sonnet (latest) — healthy");
    expect(alternativeLabel(deepseek)).toBe("ds4 · DeepSeek — healthy, 1.2% failing");
    expect(defaultAlternative(card())).toBe("sonnet");
  });
});

describe("quota card rendering", () => {
  it("renders one card listing every blocked colony and the three actions", () => {
    const html = renderToStaticMarkup(<ProviderQuotaCard card={card()} onAction={async () => undefined} nowMs={NOW} />);
    expect(html).toContain("Provider out of quota");
    expect(html).toContain("bailian · qwen3.8-max is out of quota. Resets Oct 1, 16:00 UTC (in 2d 9h)");
    expect(html).toContain("acme/webshop#42");
    expect(html).toContain("acme/api#7");
    expect(html).toContain("2 colonies blocked in acme");
    expect(html).toContain("Switch model");
    expect(html).toContain("Wait until reset");
    expect(html).toContain("Stop all 2");
    // A degraded model is shown with its failure rate but cannot be picked.
    expect(html).toMatch(/<option value="zai\/glm-5" disabled="">glm-5 · Z\.AI — degraded, 32\.5% failing<\/option>/);
    expect(html).toContain("these colonies");
    expect(html).toContain("this org (acme)");
  });

  it("puts the card first in the inbox and does not list its colonies again as questions", () => {
    const html = renderToStaticMarkup(
      <InboxView
        sessions={[]}
        onOpenColony={() => undefined}
        onOpenNotificationSettings={() => undefined}
        quotaCards={[card()]}
      />,
    );
    expect(html).toContain("2 need you");
    expect(html).toContain("Provider out of quota");
    expect(html).not.toContain("nothing waits on you");
    expect(quotaCardColonyIds([card()])).toEqual(new Set(["c1", "c2"]));
  });
});

describe("quota card actions", () => {
  it("builds the switch request, remembering only a Claude model", () => {
    expect(switchRequest("sonnet", "colonies", true)).toEqual({ action: "switch", model: "sonnet", scope: "colonies", remember: true });
    expect(switchRequest("deepseek/ds4", "org", true)).toEqual({ action: "switch", model: "deepseek/ds4", scope: "org" });
    expect(canRemember("sonnet")).toBe(true);
    expect(canRemember("deepseek/ds4")).toBe(false);
  });

  it("sends switch, wait and stop to the provider and says what happened", async () => {
    const send = vi.fn(async (_provider: string, body: { action: string }) => reply({ action: body.action }));
    const say = vi.fn();
    await runQuotaAction(send, say, "bailian", switchRequest("sonnet", "colonies", false));
    await runQuotaAction(send, say, "bailian", { action: "wait" });
    await runQuotaAction(send, say, "bailian", { action: "stop" });
    expect(send.mock.calls).toEqual([
      ["bailian", { action: "switch", model: "sonnet", scope: "colonies" }],
      ["bailian", { action: "wait" }],
      ["bailian", { action: "stop" }],
    ]);
    expect(say.mock.calls).toEqual([
      ["bailian: 2 colonies switched", undefined],
      ["bailian: 2 colonies parked until the reset", undefined],
      ["bailian: 2 colonies stopped", undefined],
    ]);
  });

  it("reports partial failures and refusals as errors", async () => {
    const say = vi.fn();
    await runQuotaAction(
      async () => reply({ action: "wait", colonies: ["c1"], failed: [{ id: "c2", ok: false, error: "queued, not started" }] }),
      say,
      "bailian",
      { action: "wait" },
    );
    expect(say).toHaveBeenLastCalledWith("bailian: 1 colony parked until the reset; 1 could not be: c2 (queued, not started)", "error");
    const refused = await runQuotaAction(
      async () => {
        throw new Error("scope \"everything\" is not supported");
      },
      say,
      "bailian",
      { action: "switch", model: "sonnet" },
    );
    expect(refused).toBeNull();
    expect(say).toHaveBeenLastCalledWith("scope \"everything\" is not supported", "error");
    expect(quotaActionSummary(reply({ action: "stop", colonies: ["c1"] }))).toBe("bailian: 1 colony stopped");
  });
});
