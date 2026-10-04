// The provider health line in Settings (issue #359): an anthropic-wire endpoint with no /v1/models
// answers 404 with a note, and reads as healthy rather than as an HTTP warning; a 401 still warns.
// Rendered to static markup: the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { AutonomyHealth, HealthStatus } from "./SettingsDialog";
import type { AutonomyStatus, ProviderHealth } from "../types";

const probe = (over: Partial<ProviderHealth>): ProviderHealth => ({
  reachable: true,
  status: 200,
  latency_ms: 12,
  models: [],
  error: null,
  note: null,
  checked_at: "2026-09-22T09:00:00Z",
  ...over,
});

const markup = (result: ProviderHealth, degraded?: boolean) =>
  renderToStaticMarkup(<HealthStatus health={{ state: "done", result }} degraded={degraded} />);

describe("HealthStatus", () => {
  it("shows a 404 carrying a note as reachable, not as an HTTP warning", () => {
    const out = markup(probe({ status: 404, note: "no model list" }));
    expect(out).toContain("Reachable · 12 ms · no model list");
    expect(out).toContain("text-ok");
    expect(out).not.toContain("HTTP 404");
    expect(markup(probe({ status: 404, note: "no model list" }), true)).toContain("no model list · but failing real traffic");
  });

  it("still warns on a 401 with no note", () => {
    const out = markup(probe({ status: 401, error: "invalid x-api-key" }));
    expect(out).toContain("HTTP 401 · 12 ms · invalid x-api-key");
    expect(out).toContain("text-warn");
  });

  // The plan balance rides along on the probe once the provider has a quota URL configured (#199);
  // its own failure is a clause on the same line and never changes the reachability verdict.
  it("shows a quota probe's remaining plan balance, or its error", () => {
    const out = markup(probe({ quota: { remaining: 12_345_678, error: null } }));
    expect(out).toContain("Reachable · 12 ms · 12,345,678 left in plan");
    expect(out).toContain("text-ok");
    expect(markup(probe({ quota: { remaining: null, error: "quota endpoint answered HTTP 401" } }))).toContain(
      "quota endpoint answered HTTP 401",
    );
  });
});

describe("AutonomyHealth (issue #875)", () => {
  const now = new Date("2026-09-24T09:05:00Z");
  const status = (over: Partial<AutonomyStatus> = {}): AutonomyStatus => ({
    enabled: true,
    model: "deepseek/deepseek-flash",
    fallback_models: ["anthropic/claude-haiku-4-5"],
    last_success: { at: "2026-09-24T09:00:00Z", model: "deepseek/deepseek-flash" },
    last_error: null,
    consecutive_failures: 0,
    alerted: false,
    ...over,
  });
  const show = (over: Partial<AutonomyStatus> = {}) => renderToStaticMarkup(<AutonomyHealth status={status(over)} now={now} />);

  it("names the last successful answer and the model that gave it", () => {
    const out = show();
    expect(out).toContain("Last answered 5m ago by deepseek/deepseek-flash");
    expect(out).not.toContain("text-warn");
  });

  it("warns with the run of failures and the provider's error", () => {
    const out = show({
      consecutive_failures: 4,
      last_error: { at: "2026-09-24T09:04:00Z", model: "deepseek/deepseek-flash", kind: "provider_error", status: 402, message: "Insufficient Balance" },
    });
    expect(out).toContain("4 in a row");
    expect(out).toContain("Failed 1m ago by deepseek/deepseek-flash · provider error · HTTP 402 · Insufficient Balance");
    expect(out).toContain("text-warn");
  });

  it("says there is no answer yet before the judge has answered once", () => {
    expect(show({ last_success: null })).toContain("No answer yet");
  });
});
