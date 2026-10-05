import { describe, expect, it } from "vitest";
import { observabilityLine } from "./ObservabilityRows";

describe("observabilityLine", () => {
  it("says off, and that nothing leaves the machine, until configured", () => {
    const line = observabilityLine({ state: "off", configured: false, reason: "the observability module is not configured" });
    expect(line.tone).toBe("muted");
    expect(line.text).toContain("Nothing leaves this machine");
  });

  it("reports a running exporter and its last failure", () => {
    expect(observabilityLine({ state: "running", configured: true, endpoint: "https://otlp.example.com", exporter: { exported: 12 } }).text).toBe(
      "Exporting to https://otlp.example.com · 12 records sent.",
    );
    const failing = observabilityLine({ state: "running", configured: true, exporter: { last_error: "answered 401" } });
    expect(failing.tone).toBe("warn");
    expect(failing.text).toContain("401");
  });

  it("explains a missing add-on", () => {
    const line = observabilityLine({ state: "no_addon", configured: true, error: "the colonizer-observability add-on is not installed" });
    expect(line).toEqual({ tone: "warn", text: "the colonizer-observability add-on is not installed" });
  });
});
