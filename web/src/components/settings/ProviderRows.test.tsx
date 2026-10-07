// The Model providers page (issue #1204): a collapsed row renders only its summary, the gauge copes
// with an unknown balance, the reset time shows when known, the credits chart draws the readings, and
// the picker leads with logo tiles (OpenRouter among them) with the catalogue collapsed.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../../api";
import { ApiContext } from "../../context";
import type { ModelProvider, ProviderUsageReport } from "../../types";
import { AddProvider } from "./AddProvider";
import { BalanceChart } from "./BalanceChart";
import { ProviderForm } from "./ProviderForm";
import { ProviderListRow } from "./ProviderRows";

const wrap = (node: React.ReactNode) => renderToStaticMarkup(<ApiContext.Provider value={{} as Api}>{node}</ApiContext.Provider>);
const NOW = Date.parse("2026-10-07T12:00:00Z");

const provider = (over: Partial<ModelProvider> = {}): ModelProvider => ({
  id: "deepseek",
  name: "DeepSeek",
  base_url: "https://api.deepseek.com/anthropic",
  auth: "x-api-key",
  wire: "anthropic",
  has_key: true,
  models: ["deepseek-flash", "deepseek-v4-pro"],
  discovered_models: ["deepseek-flash", "deepseek-v4-pro", "deepseek-reasoner"],
  preset: "deepseek",
  trusted: false,
  timeout_secs: 600,
  max_concurrent: null,
  queue_timeout_secs: null,
  context_tokens: null,
  fallback_model: null,
  in_flight: 0,
  queued: 0,
  usage: { requests: 10, failures: 0, fallbacks: 0, duration_ms: 100, since: null, last_request_at: null },
  health: { failure_pct: 0, avg_latency_ms: 10, rated: false, degraded: false },
  ...over,
});

const row = (p: ModelProvider, open = false) =>
  wrap(<ProviderListRow provider={p} disabled={false} open={open} onToggle={() => {}} onCheck={() => {}} onEdit={() => {}} />);

describe("a collapsed provider row", () => {
  it("renders only the summary: name, status, gauge and reset", () => {
    const out = row(provider({ balance: { at: "x", remaining: 620, limit: 1000, reset_unix: Math.floor(Date.now() / 1000) + 3 * 3600 + 600 } }));
    expect(out).toContain("DeepSeek");
    expect(out).toContain("62%");
    expect(out).toContain("resets in 3 h");
    expect(out).toContain('aria-expanded="false"');
    for (const detail of ["Credits", "Usage", "Connection", "https://api.deepseek.com", "deepseek-reasoner", "Check"]) expect(out).not.toContain(detail);
  });

  it("says the balance is unknown rather than drawing a gauge", () => {
    const out = row(provider());
    expect(out).toContain("balance unknown");
    expect(out).not.toContain('aria-valuenow');
  });

  it("marks a plan that is out, with the reset when the Mothership has one", () => {
    const out = row(provider({ quota_exhausted: { reset_at: "10-07 14:10 UTC", reset_unix: Math.floor(Date.now() / 1000) + 2 * 3600 + 60 } }));
    expect(out).toContain("Quota out");
    expect(out).toContain("resets in 2 h");
  });

  it("opens to credits, usage, models, wiring and connection, with the one-line help", () => {
    const out = row(provider({ used_by: ["model"] }), true);
    for (const part of ["Credits", "Usage", "Models", "Used by", "Connection", "2 enabled · +1 available", "Orchestrator model", "Balance unknown: add a plan balance reader"]) {
      expect(out).toContain(part);
    }
    expect(out).toContain('aria-expanded="true"');
  });
});

describe("BalanceChart", () => {
  const hours = (h: number) => new Date(NOW - h * 3_600_000).toISOString();
  const report = (over: Partial<ProviderUsageReport> = {}): ProviderUsageReport => ({
    provider: "deepseek",
    days: 7,
    daily: [],
    balance: [
      { at: hours(48), remaining: 1000, limit: 1000 },
      { at: hours(24), remaining: 800, limit: 1000 },
      { at: hours(0), remaining: 600, limit: 1000 },
    ],
    events: [{ at: hours(60), kind: "reset" }],
    has_balance: true,
    ...over,
  });
  it("draws the readings with the reset marker and a projected run-out", () => {
    const out = renderToStaticMarkup(<BalanceChart report={report()} tone="ok" resetUnix={null} nowMs={NOW} />);
    expect(out).toContain("600 of 1K left");
    expect(out).toContain("at this pace, out in");
    expect(out).toContain("<path");
    expect(out).toContain("stroke-dasharray");
    expect(out).toContain("reset");
  });
  it("shows the refill ahead and no run-out when the plan resets first", () => {
    const out = renderToStaticMarkup(<BalanceChart report={report()} tone="ok" resetUnix={NOW / 1000 + 3600} nowMs={NOW} />);
    expect(out).toContain("resets in 1 h");
    expect(out).not.toContain("at this pace");
  });
});

describe("the add picker", () => {
  it("leads with logo tiles including OpenRouter and keeps the catalogue collapsed", () => {
    const out = renderToStaticMarkup(<AddProvider disabled={false} onPick={() => {}} />);
    for (const tile of ["Anthropic API", "OpenRouter", "DeepSeek", "MiniMax", "Z.AI", "Alibaba", "Local server", "Custom"]) expect(out).toContain(tile);
    expect(out).toMatch(/Search \d+ more/);
    expect(out).not.toContain("Search by name or address");
    expect(out).toContain("Why?");
    expect(out).not.toContain("cc-switch");
  });
  it("opens straight into a short form: key and prefilled models, the rest behind details", () => {
    const out = wrap(<ProviderForm preset="openrouter" takenIds={[]} onCancel={() => {}} onSaved={() => {}} />);
    expect(out).toContain("Add OpenRouter");
    expect(out).toContain("anthropic/claude-sonnet-4.5");
    expect(out).toContain("Connection details");
  });
});
