import { useCallback, useEffect, useId, useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import type { ObservabilityStatus, ObservabilityTest } from "../../types";
import { formatBytes } from "../../cockpit/host";
import { Badge, Button, Spinner, cx, inputClass, timeAgo, type Tone } from "../ui";

/** One line on what the exporter is doing, from GET /api/observability/status. Pure for the test. */
export function observabilityLine(status: ObservabilityStatus | null): { tone: "ok" | "warn" | "muted"; text: string } {
  if (!status) return { tone: "muted", text: "Checking…" };
  if (!status.configured) return { tone: "muted", text: `Off: ${status.reason ?? "not configured"}. Nothing leaves this machine.` };
  const where = status.endpoint ? ` to ${status.endpoint}` : "";
  switch (status.state) {
    case "running": {
      const sent = status.exporter?.exported;
      const err = status.exporter?.last_error;
      if (err) return { tone: "warn", text: `Exporting${where}, but the last request failed: ${err}` };
      return { tone: "ok", text: `Exporting${where}${typeof sent === "number" ? ` · ${sent} records sent` : ""}.` };
    }
    case "no_addon":
    case "refused":
    case "failed":
    case "restarting":
    case "invalid":
      return { tone: "warn", text: status.error ?? status.reason ?? status.state };
    default:
      return { tone: "muted", text: `Starting the exporter${where}…` };
  }
}

/** Which add-on actions the pane offers (issue #1060): install when the status says the exporter
 *  add-on is missing, remove when one is installed. Pure for the test. */
export function addonActions(status: ObservabilityStatus | null): { install: boolean; remove: boolean } {
  return { install: status?.state === "no_addon", remove: status?.addon != null };
}

/** One signal's row in the health card: the badge's tone and label, and one detail line. */
export interface SignalRow {
  signal: string;
  tone: "ok" | "warn" | "err" | "muted";
  label: string;
  detail: string | null;
}

/** An age for a unix-seconds timestamp, in timeAgo's shape ("just now", "5m ago"); "never" without one. */
export function unixAge(unix: number | null | undefined, nowSecs: number): string {
  if (unix == null) return "never";
  return timeAgo(new Date(unix * 1000).toISOString(), new Date(nowSecs * 1000));
}

/** The health card's one row per signal (logs, traces, metrics…), from the exporter's own report.
 *  Pure, and takes `nowSecs`, so the test can pin the retry countdown and the ages. */
export function signalRows(status: ObservabilityStatus | null, nowSecs: number): SignalRow[] {
  if (!status?.configured) return [];
  const exporter = status.exporter;
  const signals = exporter?.signals;
  if (!signals) return [{ signal: "exporter", tone: "muted", label: "The exporter has not reported yet", detail: null }];
  return Object.entries(signals).map(([signal, s]): SignalRow => {
    const state = s?.state ?? "off";
    switch (state) {
      case "ok":
        return {
          signal,
          tone: "ok",
          label: "Sending",
          detail: s.last_success_unix != null ? `last record ${unixAge(s.last_success_unix, nowSecs)}` : null,
        };
      case "backing_off": {
        // The countdown is the exporter's own next retry: the per-signal report carries no clock.
        const retry = exporter?.next_retry_unix != null ? Math.max(0, exporter.next_retry_unix - nowSecs) : null;
        const detail = [retry != null ? `retrying in ${retry}s` : null, s.last_error ?? null].filter(Boolean).join(" · ");
        return { signal, tone: "warn", label: "Backing off", detail: detail || null };
      }
      case "auth_failed":
        return {
          signal,
          tone: "err",
          label: "Unauthorized",
          detail: "the backend rejected the credentials — check the header in Settings → Secrets",
        };
      case "off":
        return { signal, tone: "muted", label: "Off", detail: null };
      default:
        return { signal, tone: "warn", label: state, detail: s?.last_error ?? null };
    }
  });
}

/** Where a setting came from, as GET /api/observability/status reports it: the module, an
 *  environment variable, or the built-in default. Pure for the test. */
export function provenanceText(source: string): string {
  if (source === "module") return "from the module settings";
  const env = /^env:(.+)$/.exec(source);
  if (env) return `from ${env[1]}`;
  if (source === "default") return "built-in default";
  return source;
}

/** The settings the provenance line names: the three people look for when export surprises them. */
const PROVENANCE_KEYS = ["endpoint", "protocol", "service_name"] as const;

/** "endpoint https://… (from the module settings) · …", over the keys above; [] with no provenance. */
function provenanceParts(status: ObservabilityStatus | null): string[] {
  const provenance = status?.provenance;
  if (!provenance) return [];
  const values: Record<string, string | undefined> = {
    endpoint: status?.endpoint,
    protocol: status?.protocol,
    service_name: status?.service_name,
  };
  return PROVENANCE_KEYS.filter((key) => values[key] && provenance[key]).map(
    (key) => `${key.replace(/_/g, " ")} ${values[key]} (${provenanceText(provenance[key])})`,
  );
}

/** One sentence for a test signal that needs a word (issue #855), in the order the causes are most
 *  often found; null when the signal passed clean. `_signal` names the row the caller prepends. */
export function testHint(
  _signal: string,
  result: { ok: boolean; rejected?: number; message?: string; status?: number; auth_failed?: boolean; error?: string },
): string | null {
  if (result.ok && !(result.rejected ?? 0)) return null;
  if (result.auth_failed)
    return "the backend rejected the saved header — compare the header name and value in Settings → Secrets with what this backend expects";
  if (result.ok) {
    const hint = `the backend refused ${result.rejected} record(s) — see its docs for rejected-content rules`;
    return result.message ? `${hint}: ${result.message}` : hint;
  }
  if (result.status != null) return `the backend answered HTTP ${result.status} — check the endpoint path and protocol`;
  return "could not reach the endpoint — check the host and port";
}

/** One vendor the backend's `preset` enum names. Endpoint patterns come from the observability
 *  docs' examples only; the header is the name the backend's preset description asks for, and the
 *  header's VALUE never lives here — it goes in the observability-headers secret. */
export interface ObservabilityPreset {
  id: string;
  label: string;
  /** The docs' endpoint pattern, or null when the vendor's docs carry it (take it from there). */
  endpoint: string | null;
  /** The header name the vendor wants, or null when it usually wants none. */
  header: string | null;
  /** The shape of the header's value, shown beside the name; null with a null header. */
  headerValue: string | null;
}

/** The common backends (issue #855): the endpoint pattern and the header name to store, so the
 *  endpoint field is not typed from memory. `custom` is not a vendor, so it has no entry. */
export const PRESETS: readonly ObservabilityPreset[] = [
  {
    id: "grafana_cloud",
    label: "Grafana Cloud",
    endpoint: "https://otlp-gateway-<zone>.grafana.net/otlp",
    header: "Authorization",
    headerValue: "Basic <base64 of instance-id:token>",
  },
  {
    id: "local_collector",
    label: "A local OpenTelemetry Collector",
    endpoint: "http://127.0.0.1:4318",
    header: null,
    headerValue: null,
  },
  {
    id: "datadog",
    label: "Datadog (local Agent)",
    endpoint: "http://127.0.0.1:4318",
    header: "DD-API-KEY",
    headerValue: "<API key>",
  },
  {
    id: "honeycomb",
    label: "Honeycomb",
    endpoint: "https://api.honeycomb.io (or https://api.eu1.honeycomb.io in the EU)",
    header: "x-honeycomb-team",
    headerValue: "<ingest key>",
  },
  { id: "elastic", label: "Elastic", endpoint: null, header: "Authorization", headerValue: "ApiKey <base64 key>" },
  { id: "signoz", label: "SigNoz", endpoint: null, header: null, headerValue: null },
  { id: "new_relic", label: "New Relic", endpoint: null, header: "api-key", headerValue: "<insert key>" },
];

/** Mirrors the backend's rule (observability/settings.rs): a save that turns on
 *  `conversation_content` or `conversation_thinking` — from off, absent, or the other one on — is
 *  refused unless it carries `confirm_content`. This is the pane's half: the message the operator
 *  confirms before the save sends it. */
export function contentConfirmation(stored: Record<string, unknown>, draft: Record<string, unknown>): string | null {
  const turnsOn = (key: string) => draft[key] === true && stored[key] !== true;
  if (!turnsOn("conversation_content") && !turnsOn("conversation_thinking")) return null;
  return `Turning this on sends what you and the agent actually said — and the model's thinking — to your observability backend. Each workspace (org) must also opt in before any of it is sent, and restricted colonies, or colonies whose sensitivity was never recorded, never send content. This meaningfully increases data volume and backend cost. Continue?`;
}

/** How long a failing signal takes before the header says so (issue #855): a shorter outage is
 *  usually a backend restart, and the pane's health card already names it. */
export const DEGRADED_AFTER_SECS = 600;

/** The signals that are failing hard enough to count as degraded: backing off or unauthorized. */
function failingSignals(status: ObservabilityStatus | null): string[] {
  const signals = status?.exporter?.signals;
  if (!status?.configured || !signals) return [];
  return Object.entries(signals)
    .filter(([, s]) => s?.state === "auth_failed" || s?.state === "backing_off")
    .map(([name]) => name);
}

/** Remembers when the exporter's failing started, from this cockpit's own point of view (#855):
 *  null while nothing is failing; the first failing observation's `nowSecs` after that; and the
 *  earlier of the two while it keeps failing. The status payload carries no server-side
 *  first-failure time, so the clock starts when this cockpit first saw the failure — a cockpit
 *  opened after an outage began still waits the full window before it says so. */
export function advanceFailureClock(previous: number | null, status: ObservabilityStatus | null, nowSecs: number): number | null {
  if (failingSignals(status).length === 0) return null;
  return previous === null ? nowSecs : Math.min(previous, nowSecs);
}

/** The header badge's one line (issue #855): the first failing signal and how long it has been
 *  failing, once it has been failing for DEGRADED_AFTER_SECS. Null while the failure is younger,
 *  when nothing is failing, or before the clock has started. */
export function observabilityDegraded(status: ObservabilityStatus | null, nowSecs: number, failingSinceSecs: number | null): string | null {
  const failing = failingSignals(status);
  if (failing.length === 0 || failingSinceSecs === null) return null;
  const forSecs = nowSecs - failingSinceSecs;
  if (forSecs < DEGRADED_AFTER_SECS) return null;
  return `${failing[0]} have failed for ${Math.floor(forSecs / 60)} min`;
}

/** Badge tones for the signal rows: a muted row reads as neutral, the badge palette's quiet tone. */
const SIGNAL_TONE: Record<SignalRow["tone"], Tone> = { ok: "ok", warn: "warn", err: "err", muted: "neutral" };

/**
 * The observability pane's health card (issue #855): one row per signal with its badge and detail,
 * a muted summary (backlog, records sent, last success, the exporter's state when it is not
 * running), the provenance line saying where each setting came from, the common backends and their
 * headers, and the last test's result with a hint per failing signal. Presentational, props only,
 * so the tests render it to static markup.
 */
export function ObservabilityHealthCard({
  status,
  test,
  nowSecs,
}: {
  status: ObservabilityStatus | null;
  test: ObservabilityTest | { ok: false; error: string } | null;
  nowSecs: number;
}) {
  const rows = signalRows(status, nowSecs);
  const exporter = status?.exporter;
  const exporterState = exporter?.state != null && exporter.state !== "running" && exporter.state !== "ok" ? exporter.state : null;
  const summary = [
    exporter?.backlog_bytes != null ? `${formatBytes(exporter.backlog_bytes)} behind` : null,
    exporter?.exported != null ? `${exporter.exported} records sent` : null,
    exporter?.last_success_unix != null ? `last success ${unixAge(exporter.last_success_unix, nowSecs)}` : null,
    exporterState ? `exporter ${exporterState}` : null,
  ]
    .filter((part): part is string => part !== null)
    .join(" · ");
  const where = provenanceParts(status);
  const noted =
    test && "signals" in test && test.signals ? Object.entries(test.signals).filter(([, r]) => !r.ok || (r.rejected ?? 0) > 0) : [];
  const undocumented = PRESETS.filter((p) => p.endpoint === null);
  return (
    <div className="space-y-2 py-2.5">
      {rows.length > 0 && (
        <div>
          {rows.map((row) => (
            <div key={row.signal} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2">
              <span className="text-body-sm font-medium">{row.signal}</span>
              <Badge tone={SIGNAL_TONE[row.tone]}>{row.label}</Badge>
              {row.detail && <span className="min-w-0 flex-1 text-small-lg text-muted [overflow-wrap:anywhere]">{row.detail}</span>}
            </div>
          ))}
        </div>
      )}
      {summary && <p className="text-small-lg text-muted [overflow-wrap:anywhere]">{summary}</p>}
      {where.length > 0 && <p className="text-small-lg text-muted [overflow-wrap:anywhere]">{where.join(" · ")}</p>}
      <div>
        <p className="text-body-sm font-medium">Common backends</p>
        <ul className="mt-1 space-y-1 text-small-lg text-muted">
          {PRESETS.filter((p) => p.endpoint !== null).map((p) => (
            <li key={p.id} className="[overflow-wrap:anywhere]">
              {p.label} — endpoint <code className="font-mono">{p.endpoint}</code>
              {p.header !== null && (
                <>
                  {"; store header "}
                  <code className="font-mono">{p.header}</code> ({p.headerValue}) in Settings → Secrets
                </>
              )}
            </li>
          ))}
        </ul>
        {undocumented.length > 0 && (
          <p className="mt-1 text-small-lg text-muted [overflow-wrap:anywhere]">
            {undocumented.map((p) => (p.header !== null ? `${p.label} (header ${p.header})` : p.label)).join(", ")}: the endpoint comes
            from the vendor's own OpenTelemetry docs; the header value still goes in Settings → Secrets.
          </p>
        )}
      </div>
      {test && (
        <p role="status" className={cx("text-small-lg [overflow-wrap:anywhere]", test.ok && noted.length === 0 ? "text-ok" : "text-err")}>
          {test.ok && noted.length === 0
            ? "The backend accepted the test log record and metric point."
            : "error" in test && test.error && noted.length === 0
              ? test.error
              : noted
                  .map(([signal, r]) => `${signal}: ${r.error ?? (r.ok ? "rejected" : "failed")} — ${testHint(signal, r) ?? ""}`)
                  .join(" · ")}
        </p>
      )}
    </div>
  );
}

/**
 * The observability pane's own rows (issue #839): the exporter's status, the headers secret (a
 * standard `k=v,k2=v2` list; the value is write-only here), and a "Send test" button that sends one
 * log record and one metric point through the saved config. The health card (issue #855) sits under
 * the status line; this container keeps the polling and the two controls that need state.
 */
export function ObservabilityRows({ unsaved }: { unsaved: boolean }) {
  const api = useApi();
  const toast = useToast();
  const id = useId();
  const [status, setStatus] = useState<ObservabilityStatus | null>(null);
  const [headers, setHeaders] = useState("");
  const [busy, setBusy] = useState<"save" | "test" | "install" | "remove" | null>(null);
  const [test, setTest] = useState<ObservabilityTest | { ok: false; error: string } | null>(null);

  const load = useCallback(() => api.observabilityStatus().then(setStatus, () => setStatus(null)), [api]);
  useEffect(() => {
    void load();
    const t = setInterval(() => void load(), 10_000);
    return () => clearInterval(t);
  }, [load]);

  const saveHeaders = async () => {
    setBusy("save");
    try {
      await api.saveSecret("observability-headers", headers.trim());
      setHeaders("");
      toast("Observability headers saved");
      void load();
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setBusy(null);
    }
  };

  const runTest = async () => {
    setBusy("test");
    try {
      setTest(await api.observabilityTest());
    } catch (error) {
      setTest({ ok: false, error: errorMessage(error) });
    } finally {
      setBusy(null);
    }
  };

  const line = observabilityLine(status);
  const names = status?.headers?.names ?? [];
  return (
    <>
      <p role="status" className={cx("py-2.5 text-small-lg [overflow-wrap:anywhere]", line.tone === "ok" ? "text-ok" : line.tone === "warn" ? "text-warn" : "text-muted")}>
        {line.text}
      </p>
      <ObservabilityHealthCard status={status} test={test} nowSecs={Math.floor(Date.now() / 1000)} />
      <div className="space-y-2 py-2.5">
        <label htmlFor={id} className="block text-body-sm font-medium">
          Headers
        </label>
        <p className="text-small-lg text-muted">
          {names.length > 0
            ? `Sending ${names.join(", ")} (from ${status?.headers?.source === "env" ? "OTEL_EXPORTER_OTLP_HEADERS" : "the saved secret"}).`
            : "None saved."}{" "}
          A <code className="font-mono">k=v,k2=v2</code> list, kept as a secret: it is never shown again or logged.
        </p>
        <form
          className="flex flex-wrap gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (headers.trim()) void saveHeaders();
          }}
        >
          <input
            id={id}
            type="password"
            autoComplete="off"
            value={headers}
            onChange={(e) => setHeaders(e.target.value)}
            placeholder="x-honeycomb-team=…"
            className={cx(inputClass, "min-w-48 flex-1")}
          />
          <Button type="submit" variant="primary" disabled={!headers.trim() || busy !== null}>
            {busy === "save" && <Spinner />} Save
          </Button>
        </form>
      </div>
      <div className="space-y-2 py-2.5">
        <div className="flex flex-wrap items-center gap-3">
          <Button disabled={busy !== null || unsaved || !status?.configured} onClick={() => void runTest()}>
            {busy === "test" && <Spinner />} Send test
          </Button>
          <span className="text-small-lg text-muted">
            {unsaved ? "Save first: the test uses the saved settings." : "Sends one log record and one metric point to the backend."}
          </span>
        </div>
      </div>
    </>
  );
}
