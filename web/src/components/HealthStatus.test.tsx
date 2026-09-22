// The provider health line in Settings (issue #359): an anthropic-wire endpoint with no /v1/models
// answers 404 with a note, and reads as healthy rather than as an HTTP warning; a 401 still warns.
// Rendered to static markup: the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { HealthStatus } from "./SettingsDialog";
import type { ProviderHealth } from "../types";

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
});
