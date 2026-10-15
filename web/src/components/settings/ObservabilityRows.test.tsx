// The observability pane's pure derivations (issues #839 and #855): the status line, the health
// card's signal rows, the test hints, the content confirmation, the degraded badge clock and the
// preset helper. Rendered to static markup: the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import {
  DEGRADED_AFTER_SECS,
  ObservabilityHealthCard,
  PRESETS,
  advanceFailureClock,
  contentConfirmation,
  observabilityDegraded,
  observabilityLine,
  provenanceText,
  signalRows,
  testHint,
} from "./ObservabilityRows";
import type { ObservabilitySignalStatus, ObservabilityStatus } from "../../types";

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

  it("says starting while the exporter comes up", () => {
    expect(observabilityLine({ state: "starting", configured: true, endpoint: "https://otlp.example.com" })).toEqual({
      tone: "muted",
      text: "Starting the exporter to https://otlp.example.com…",
    });
  });

  it("warns through a restart, a refusal, a failure and an invalid config with the backend's reason", () => {
    for (const state of ["restarting", "refused", "failed", "invalid"] as const) {
      const line = observabilityLine({ state, configured: true, error: `the exporter is ${state}` });
      expect(line.tone).toBe("warn");
      expect(line.text).toBe(`the exporter is ${state}`);
    }
  });
});

describe("signalRows", () => {
  const now = 1_000_000;
  const status = (signals: Record<string, ObservabilitySignalStatus>, over: Partial<ObservabilityStatus> = {}): ObservabilityStatus => ({
    state: "running",
    configured: true,
    ...over,
    exporter: { signals, ...over.exporter },
  });

  it("says Sending with the age of the last record for a healthy signal", () => {
    expect(signalRows(status({ logs: { state: "ok", last_success_unix: now - 300 } }), now)).toEqual([
      { signal: "logs", tone: "ok", label: "Sending", detail: "last record 5m ago" },
    ]);
    expect(signalRows(status({ logs: { state: "ok" } }), now)[0].detail).toBeNull();
  });

  it("says Backing off with the retry countdown and the last error", () => {
    // The countdown is the exporter's own next retry; a report without one shows just the error.
    expect(signalRows(status({ traces: { state: "backing_off", last_error: "connection refused" } }, { exporter: { next_retry_unix: now + 20 } }), now)).toEqual([
      { signal: "traces", tone: "warn", label: "Backing off", detail: "retrying in 20s · connection refused" },
    ]);
    expect(signalRows(status({ traces: { state: "backing_off", last_error: "connection refused" } }), now)).toEqual([
      { signal: "traces", tone: "warn", label: "Backing off", detail: "connection refused" },
    ]);
  });

  it("says Unauthorized for a rejected credential, pointing at the secret", () => {
    expect(signalRows(status({ metrics: { state: "auth_failed", last_error: "HTTP 401" } }), now)).toEqual([
      { signal: "metrics", tone: "err", label: "Unauthorized", detail: "the backend rejected the credentials — check the header in Settings → Secrets" },
    ]);
  });

  it("says Off, muted, for a signal that is off or has not reported a state", () => {
    expect(signalRows(status({ profiles: { state: "off" } }), now)).toEqual([{ signal: "profiles", tone: "muted", label: "Off", detail: null }]);
    expect(signalRows(status({ profiles: {} }), now)).toEqual([{ signal: "profiles", tone: "muted", label: "Off", detail: null }]);
  });

  it("warns with the raw state for a state it does not know", () => {
    expect(signalRows(status({ logs: { state: "throttled", last_error: "429" } }), now)).toEqual([
      { signal: "logs", tone: "warn", label: "throttled", detail: "429" },
    ]);
  });

  it("says nothing when not configured, and one muted row when configured with no exporter report yet", () => {
    expect(signalRows({ state: "off", configured: false }, now)).toEqual([]);
    expect(signalRows({ state: "running", configured: true }, now)).toEqual([
      { signal: "exporter", tone: "muted", label: "The exporter has not reported yet", detail: null },
    ]);
  });
});

describe("testHint", () => {
  it("is null for a clean pass", () => {
    expect(testHint("logs", { ok: true, rejected: 0 })).toBeNull();
    expect(testHint("logs", { ok: true })).toBeNull();
  });

  it("names refused records, and carries the backend's message when it sent one", () => {
    expect(testHint("logs", { ok: true, rejected: 3 })).toBe("the backend refused 3 record(s) — see its docs for rejected-content rules");
    expect(testHint("logs", { ok: true, rejected: 1, message: "log body too large" })).toBe(
      "the backend refused 1 record(s) — see its docs for rejected-content rules: log body too large",
    );
  });

  it("points at the saved header when the backend rejected the credential", () => {
    expect(testHint("traces", { ok: false, auth_failed: true, status: 401, error: "HTTP 401 Unauthorized" })).toBe(
      "the backend rejected the saved header — compare the header name and value in Settings → Secrets with what this backend expects",
    );
  });

  it("reads an HTTP answer as an endpoint problem", () => {
    expect(testHint("logs", { ok: false, status: 404, error: "no such path" })).toBe("the backend answered HTTP 404 — check the endpoint path and protocol");
  });

  it("falls back to reachability", () => {
    expect(testHint("logs", { ok: false, error: "connection refused" })).toBe("could not reach the endpoint — check the host and port");
  });

  it("appears beside the failing signal in the health card", () => {
    const html = renderToStaticMarkup(
      <ObservabilityHealthCard
        status={null}
        test={{ ok: false, signals: { traces: { ok: false, auth_failed: true, status: 401, error: "HTTP 401 Unauthorized" } } }}
        nowSecs={1_000_000}
      />,
    );
    expect(html).toContain("traces: HTTP 401 Unauthorized");
    expect(html).toContain("compare the header name and value in Settings → Secrets");
  });
});

describe("contentConfirmation", () => {
  const message = contentConfirmation({}, { conversation_content: true });

  it("states what is sent, the workspace opt-in, the restricted colonies and the cost, and asks", () => {
    expect(message).not.toBeNull();
    expect(message).toContain("what you and the agent actually said");
    expect(message).toContain("must also opt in");
    expect(message).toContain("restricted colonies");
    expect(message).toContain("cost");
    expect(message).toMatch(/Continue\?$/);
  });

  it("asks only for a switch this save turns on", () => {
    expect(contentConfirmation({ conversation_content: true }, { conversation_content: true })).toBeNull();
    expect(contentConfirmation({ conversation_content: true }, { conversation_content: false })).toBeNull();
    expect(contentConfirmation({ conversation_thinking: true }, { conversation_thinking: true })).toBeNull();
  });

  it("counts a switch that was never set as turning on, and the thinking switch on its own", () => {
    expect(contentConfirmation({ conversation_content: false }, { conversation_content: true })).toContain("Continue?");
    expect(contentConfirmation({}, { conversation_thinking: true })).toContain("the model's thinking");
    // Turning content on while thinking is already on is still a new stream: the backend asks too.
    expect(contentConfirmation({ conversation_thinking: true }, { conversation_thinking: true, conversation_content: true })).toContain("Continue?");
  });
});

describe("PRESETS", () => {
  const byId = (id: string) => PRESETS.find((p) => p.id === id);

  it("carries the docs' endpoint patterns and the headers the backend names", () => {
    expect(byId("grafana_cloud")?.endpoint).toContain("otlp-gateway");
    expect(byId("grafana_cloud")?.header).toBe("Authorization");
    expect(byId("datadog")?.header).toBe("DD-API-KEY");
    expect(byId("honeycomb")?.header).toBe("x-honeycomb-team");
    expect(byId("local_collector")?.header).toBeNull();
  });

  it("shows the preset block, header names and where their values go in the health card", () => {
    const html = renderToStaticMarkup(<ObservabilityHealthCard status={null} test={null} nowSecs={0} />);
    expect(html).toContain("Common backends");
    expect(html).toContain("Settings → Secrets");
    expect(html).toContain("Authorization");
    expect(html).toContain("x-honeycomb-team");
  });
});

describe("degraded badge", () => {
  const now = 10_000;
  const failing: ObservabilityStatus = {
    state: "running",
    configured: true,
    exporter: { signals: { logs: { state: "auth_failed" }, traces: { state: "ok" } } },
  };
  const backingOff: ObservabilityStatus = {
    state: "running",
    configured: true,
    exporter: { signals: { traces: { state: "backing_off" } } },
  };
  const healthy: ObservabilityStatus = { state: "running", configured: true, exporter: { signals: { logs: { state: "ok" } } } };

  it("waits the full window before it says anything", () => {
    expect(DEGRADED_AFTER_SECS).toBe(600);
    expect(observabilityDegraded(failing, now, now - 599)).toBeNull();
    expect(observabilityDegraded(failing, now, now - DEGRADED_AFTER_SECS)).toBe("logs have failed for 10 min");
    expect(observabilityDegraded(backingOff, now, now - 660)).toBe("traces have failed for 11 min");
  });

  it("stays quiet when nothing is failing or the clock has not started", () => {
    expect(observabilityDegraded(healthy, now, now - 3_600)).toBeNull();
    expect(observabilityDegraded({ state: "off", configured: false }, now, now - 3_600)).toBeNull();
    expect(observabilityDegraded(failing, now, null)).toBeNull();
  });

  it("starts the clock on the first failing observation and keeps it while the failure lasts", () => {
    expect(advanceFailureClock(null, failing, now)).toBe(now);
    expect(advanceFailureClock(now - 60, failing, now)).toBe(now - 60);
    expect(advanceFailureClock(null, { state: "off", configured: false }, now)).toBeNull();
  });

  it("resets the clock when the exporter is healthy again", () => {
    expect(advanceFailureClock(now - 120, healthy, now)).toBeNull();
    expect(advanceFailureClock(null, healthy, now)).toBeNull();
  });
});

describe("provenanceText", () => {
  it("names the module settings, the environment variable and the built-in default", () => {
    expect(provenanceText("module")).toBe("from the module settings");
    expect(provenanceText("env:OTEL_EXPORTER_OTLP_ENDPOINT")).toBe("from OTEL_EXPORTER_OTLP_ENDPOINT");
    expect(provenanceText("default")).toBe("built-in default");
    expect(provenanceText("somewhere else")).toBe("somewhere else");
  });
});

describe("ObservabilityHealthCard", () => {
  const now = 1_000_000;
  const rich: ObservabilityStatus = {
    state: "running",
    configured: true,
    endpoint: "https://otlp.example.com",
    protocol: "http/protobuf",
    service_name: "colonizer-mothership",
    provenance: { endpoint: "module", protocol: "default", service_name: "env:OTEL_EXPORTER_OTLP_SERVICE_NAME" },
    exporter: {
      state: "running",
      exported: 1284,
      backlog_bytes: 12_300,
      last_success_unix: now - 300,
      next_retry_unix: now + 20,
      signals: {
        logs: { state: "ok", last_success_unix: now - 60 },
        traces: { state: "backing_off", last_error: "connection refused" },
        metrics: { state: "auth_failed", last_error: "HTTP 401" },
        profiles: { state: "off" },
      },
    },
  };

  it("shows one row per signal state and the exporter's summary", () => {
    const html = renderToStaticMarkup(<ObservabilityHealthCard status={rich} test={null} nowSecs={now} />);
    expect(html).toContain("Sending");
    expect(html).toContain("Backing off");
    expect(html).toContain("Unauthorized");
    expect(html).toContain("Off");
    expect(html).toContain("12K behind");
    expect(html).toContain("1284 records sent");
  });

  it("says where each setting came from, over the provenance the backend sent", () => {
    const html = renderToStaticMarkup(<ObservabilityHealthCard status={rich} test={null} nowSecs={now} />);
    expect(html).toContain("endpoint https://otlp.example.com (from the module settings)");
    expect(html).toContain("protocol http/protobuf (built-in default)");
    expect(html).toContain("service name colonizer-mothership (from OTEL_EXPORTER_OTLP_SERVICE_NAME)");
  });

  it("degrades to the presets alone when the exporter has not reported", () => {
    const html = renderToStaticMarkup(<ObservabilityHealthCard status={{ state: "starting", configured: true }} test={null} nowSecs={now} />);
    expect(html).toContain("Common backends");
    expect(html).not.toContain("records sent");
  });
});
