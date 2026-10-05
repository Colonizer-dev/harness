import { useCallback, useEffect, useId, useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import type { ObservabilityStatus, ObservabilityTest } from "../../types";
import { Button, Spinner, cx, inputClass } from "../ui";

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

/**
 * The observability pane's own rows (issue #839): the exporter's status, the headers secret (a
 * standard `k=v,k2=v2` list; the value is write-only here), and a "Send test" button that sends one
 * log record and one metric point through the saved config.
 */
export function ObservabilityRows({ unsaved }: { unsaved: boolean }) {
  const api = useApi();
  const toast = useToast();
  const id = useId();
  const [status, setStatus] = useState<ObservabilityStatus | null>(null);
  const [headers, setHeaders] = useState("");
  const [busy, setBusy] = useState<"save" | "test" | null>(null);
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
  const failures = test && "signals" in test && test.signals ? Object.entries(test.signals).filter(([, r]) => !r.ok) : [];
  return (
    <>
      <p role="status" className={cx("py-2.5 text-small-lg [overflow-wrap:anywhere]", line.tone === "ok" ? "text-ok" : line.tone === "warn" ? "text-warn" : "text-muted")}>
        {line.text}
      </p>
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
        {test && (
          <p role="status" className={cx("text-small-lg [overflow-wrap:anywhere]", test.ok ? "text-ok" : "text-err")}>
            {test.ok
              ? "The backend accepted the test log record and metric point."
              : "error" in test && test.error
                ? test.error
                : failures.map(([signal, r]) => `${signal}: ${r.error ?? "rejected"}`).join(" · ")}
          </p>
        )}
      </div>
    </>
  );
}
