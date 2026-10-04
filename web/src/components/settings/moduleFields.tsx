import { useCallback, useEffect, useId, useState, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import type { HeadroomStatus, ModelOption, ModuleInfo, SchemaField, VoiceStatus } from "../../types";
import { canRecord, keySourceLabel, startRecording } from "../../voiceRecorder";
import { ModelPicker } from "../ModelPicker";
import { SkillsetField } from "../Skillsets";
import { Button, Spinner, Switch, cx, inputClass } from "../ui";
import { Row } from "./ui";

// ---------------------------------------------------------------------------
// The module panes' shared pieces: the kind labels, the draft bookkeeping, and
// the schema-driven field controls (including Headroom, Jev and the voice keys).
// ---------------------------------------------------------------------------

const KIND_INFO: Record<string, { title: string; description: string }> = {
  source: { title: "Source", description: "Where tasks come from" },
  sandbox: { title: "Sandbox", description: "Where agents run" },
  mesh: { title: "Mesh", description: "Private network between the Mothership and colonies" },
  agent: { title: "Agent", description: "The coding agent inside each microVM" },
  interfaces: { title: "Interfaces", description: "Panels in the colony view" },
  publish: { title: "Publish", description: "Where finished work goes" },
  memory: { title: "Memory", description: "Shared notes colonies can search and propose" },
  watchdog: { title: "Watchdog", description: "Notices stalled colonies and nudges them" },
  autonomy: { title: "Autonomy", description: "Who answers a colony's questions when you are not there" },
  burn_down: { title: "Burn-down", description: "Spend the weekly token plan down to a reserve before it resets" },
  screen: { title: "Prompt screening", description: "Screen the diff and PR body for hidden code points before publishing" },
  voice: { title: "Voice", description: "Speech-to-text for the composer's microphone" },
  observability: { title: "Observability", description: "Send logs, traces and metrics to Grafana or any OpenTelemetry backend" },
};

export const kindInfo = (kind: string) => KIND_INFO[kind] ?? { title: kind, description: "" };

export type ModuleDraft = { provider: string; enabled: boolean; settings: Record<string, unknown> };

export const draftOf = (m: ModuleInfo): ModuleDraft => ({ provider: m.provider, enabled: m.enabled, settings: m.settings ?? {} });

export const isDirty = (m: ModuleInfo, d: ModuleDraft | undefined) =>
  d !== undefined && (d.provider !== m.provider || d.enabled !== m.enabled || JSON.stringify(d.settings) !== JSON.stringify(m.settings ?? {}));

export function valueOf(settings: Record<string, unknown>, key: string, field: SchemaField): unknown {
  if (settings[key] !== undefined) return settings[key];
  if (field.default !== undefined) return field.default;
  return field.type === "boolean" ? false : "";
}

/** What goes behind a field's "i": the schema description plus its default and range. */
function fieldInfo(field: SchemaField): ReactNode | null {
  const facts: string[] = [];
  if (field.default !== undefined && field.default !== null && field.default !== "") {
    facts.push(`Default ${typeof field.default === "boolean" ? (field.default ? "on" : "off") : String(field.default)}`);
  }
  if (field.minimum != null || field.maximum != null) facts.push(`Range ${field.minimum ?? "…"}–${field.maximum ?? "…"}`);
  if (!field.description && facts.length === 0) return null;
  return (
    <>
      {field.description && <p>{field.description}</p>}
      {facts.length > 0 && <p className="text-muted">{facts.join(" · ")}</p>}
    </>
  );
}

/** The Headroom bundle download (GET/POST /api/headroom), polled while it runs. */
export function useHeadroom(active: boolean) {
  const api = useApi();
  const [status, setStatus] = useState<HeadroomStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!active) return;
    let stop = false;
    api
      .headroom()
      .then((s) => !stop && setStatus(s))
      .catch(() => {});
    return () => {
      stop = true;
    };
  }, [active, api]);

  const running = status?.state === "downloading" || status?.state === "unpacking";
  useEffect(() => {
    if (!active || !running) return;
    const timer = setInterval(() => {
      api
        .headroom()
        .then(setStatus)
        .catch(() => {});
    }, 1000);
    return () => clearInterval(timer);
  }, [active, api, running]);

  const start = useCallback(async () => {
    setError(null);
    try {
      setStatus(await api.headroomDownload());
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api]);

  return { status, error, start };
}

const megabytes = (bytes: number) => `${Math.round(bytes / 1048576)} MB`;

/** Shown in the agent pane while Headroom is switched on, or while its download runs or has failed. */
export function HeadroomRow({ headroom }: { headroom: ReturnType<typeof useHeadroom> }) {
  const { status, error, start } = headroom;
  const box = "flex flex-wrap items-center gap-2 rounded-xl border border-border px-3.5 py-2.5 text-[12.5px]";

  if (error) {
    return (
      <div className={cx(box, "text-err")}>
        <div className="min-w-0 flex-1">Could not start the Headroom download: {error}</div>
        <Button size="sm" onClick={() => void start()}>
          Retry
        </Button>
      </div>
    );
  }
  if (!status) return null;
  switch (status.state) {
    case "unavailable":
      return <div className={cx(box, "text-muted")}>No Headroom bundle is published for this machine's architecture, so colonies run without it.</div>;
    case "installed":
      return (
        <div className={cx(box, "text-muted")}>
          <span className="text-ok">Headroom {status.release} is downloaded.</span> Colonies that start with it switched on use it.
        </div>
      );
    case "downloading":
    case "unpacking": {
      const pct = status.total ? Math.min(100, Math.round((status.bytes * 100) / status.total)) : null;
      return (
        <div className="rounded-xl border border-border px-3.5 py-2.5 text-[12.5px] text-muted">
          <div className="mb-1.5 flex flex-wrap items-center gap-2">
            <Spinner />
            <span>
              {status.state === "unpacking" ? "Unpacking" : "Downloading"} Headroom {status.release}
              {status.state === "downloading" && status.total ? ` · ${megabytes(status.bytes)} of ${megabytes(status.total)}` : ""}
            </span>
            <span className="text-faint">happens once per release</span>
          </div>
          <div
            className="h-1 overflow-hidden rounded-full bg-border"
            role="progressbar"
            aria-label="Downloading Headroom"
            aria-valuenow={pct ?? undefined}
            aria-valuemin={0}
            aria-valuemax={100}
          >
            {pct === null || status.state === "unpacking" ? (
              <div className="pull-slide h-full w-1/3 rounded-full bg-accent" />
            ) : (
              <div className="h-full rounded-full bg-accent transition-[width]" style={{ width: `${pct}%` }} />
            )}
          </div>
        </div>
      );
    }
    case "failed":
      return (
        <div className={cx(box, "text-err")}>
          <div className="min-w-0 flex-1">The Headroom download failed: {status.error}</div>
          <Button size="sm" onClick={() => void start()}>
            Retry
          </Button>
        </div>
      );
    default:
      return (
        <div className={cx(box, "text-muted")}>
          <div className="min-w-0 flex-1">Headroom downloads when you save with it switched on (220–245 MB, once). Colonies run without it until then. While it runs, it takes 300–370 MB of each colony’s memory.</div>
          <Button size="sm" onClick={() => void start()}>
            Download now
          </Button>
        </div>
      );
  }
}

/** Shown in the agent pane while Jev compaction is switched on: the data-egress and cost warning. */
export function JevCompactionNotice() {
  return (
    <div className="rounded-xl border border-border px-3.5 py-2.5 text-[12.5px] text-warn">
      Sends this colony&apos;s conversation and tool-call history — file paths, command output — to TypeSafe
      (api.typesafe.ai) at each compaction. TypeSafe bills it directly: the cost isn&apos;t tracked by the
      Colonizer gateway or shown in colony cost. Read TypeSafe&apos;s data terms before using it on private repos.
    </div>
  );
}

/**
 * A voice service's key, beside the module like mem0's: write-only, saved on the Mothership, never
 * in modules.json and never shown again. The status line says where the active key comes from — a
 * key already set on a matching model provider (OpenAI, Groq) is reused, so there may be nothing to add.
 */
export function VoiceKeyRow({ provider, name }: { provider: string; name: string }) {
  const api = useApi();
  const toast = useToast();
  const id = useId();
  const [status, setStatus] = useState<VoiceStatus | null>(null);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<"save" | "remove" | null>(null);

  useEffect(() => {
    api.voice().then(setStatus, () => setStatus(null));
  }, [api, provider]);

  const saveKey = async (value: string, kind: "save" | "remove") => {
    setBusy(kind);
    try {
      setStatus(await api.saveVoiceKey(provider, value));
      setKey("");
      toast(kind === "save" ? `${name} key saved` : `${name} key removed`);
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setBusy(null);
    }
  };

  // The status describes the saved (active) provider; while another one is picked but unsaved,
  // its key state is unknown until the module is saved.
  const same = status?.provider === provider;
  const state = !status ? "Checking…" : !same ? "Save the module to see this service's key." : keySourceLabel(status.source, status.key_optional);

  return (
    <div className="space-y-2 py-2.5">
      <label htmlFor={id} className="block text-[13px] font-medium">
        {name} API key
      </label>
      <p className="text-[12.5px] text-muted">{state} It stays on the Mothership: the browser sends audio there, never the key.</p>
      <form
        className="flex flex-wrap gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          if (key.trim()) void saveKey(key.trim(), "save");
        }}
      >
        <input
          id={id}
          type="password"
          autoComplete="off"
          value={key}
          onChange={(e) => setKey(e.target.value)}
          placeholder={same && status?.has_key ? "Replace the key" : "Paste the key"}
          className={cx(inputClass, "min-w-48 flex-1")}
        />
        <Button type="submit" variant="primary" disabled={!key.trim() || busy !== null}>
          {busy === "save" && <Spinner />} Save
        </Button>
        {same && status?.source === "saved" && (
          <Button disabled={busy !== null} onClick={() => void saveKey("", "remove")}>
            {busy === "remove" && <Spinner />} Remove
          </Button>
        )}
      </form>
    </div>
  );
}

/** Records three seconds and runs them through the saved voice service, so a key and a microphone
 *  are proven together before the composer relies on them. */
export function VoiceTestRow({ unsaved }: { unsaved: boolean }) {
  const api = useApi();
  const [state, setState] = useState<{ phase: "idle" | "recording" | "transcribing" } | { phase: "done"; text: string } | { phase: "failed"; error: string }>({
    phase: "idle",
  });

  const run = async () => {
    try {
      const status = await api.voice();
      if (status.provider === "browser") {
        setState({ phase: "failed", error: "The browser recognises speech itself; there is no service to test. Pick one and save first." });
        return;
      }
      if (!status.configured) {
        setState({ phase: "failed", error: `${status.name} is not ready: add its key${status.key_optional ? " or base URL" : ""} first.` });
        return;
      }
      setState({ phase: "recording" });
      const recording = await startRecording();
      await new Promise((resolve) => setTimeout(resolve, 3000));
      const clip = await recording.stop();
      setState({ phase: "transcribing" });
      const { text } = await api.transcribe(clip);
      setState({ phase: "done", text });
    } catch (error) {
      setState({ phase: "failed", error: error instanceof DOMException && error.name === "NotAllowedError" ? "Microphone access was blocked" : errorMessage(error) });
    }
  };

  const busy = state.phase === "recording" || state.phase === "transcribing";
  return (
    <div className="space-y-2 py-2.5">
      <div className="flex flex-wrap items-center gap-3">
        <Button disabled={busy || unsaved || !canRecord()} onClick={() => void run()}>
          {busy && <Spinner />} Test microphone
        </Button>
        <span className="text-[12.5px] text-muted">
          {unsaved
            ? "Save first: the test uses the saved service."
            : state.phase === "recording"
              ? "Recording 3 seconds — say something…"
              : state.phase === "transcribing"
                ? "Transcribing…"
                : !canRecord()
                  ? "This browser can't record audio."
                  : "Records 3 seconds and transcribes them with the saved service."}
        </span>
      </div>
      {state.phase === "done" && (
        <p role="status" className="text-[13px] text-ok">
          {state.text ? `Heard: “${state.text}”` : "The service answered, but heard nothing."}
        </p>
      )}
      {state.phase === "failed" && (
        <p role="status" className="text-[12.5px] text-err">
          {state.error}
        </p>
      )}
    </div>
  );
}

export function SettingField({
  name,
  field,
  value,
  onChange,
  models,
}: {
  name: string;
  field: SchemaField;
  value: unknown;
  onChange: (value: unknown) => void;
  /** Suggestions for free-text model fields. */
  models?: ModelOption[];
}) {
  const id = useId();
  const label = field.title ?? name;
  if (field.format === "plugin-dirs") {
    return <SkillsetField label={label} description={field.description} value={value} onChange={onChange} />;
  }
  const info = fieldInfo(field);
  const text = value === undefined || value === null ? "" : String(value);

  if (field.type === "array") {
    // A string array edits as one comma-separated line and is saved back as an array on blur:
    // parsing every keystroke would eat the separators a person is still typing. Each entry is
    // checked again at save time by the mothership (path policy: modules.rs `validate_settings`).
    const entries = Array.isArray(value) ? value.map(String) : [];
    return (
      <Row id={id} label={label} info={info}>
        <input
          id={id}
          type="text"
          defaultValue={entries.join(", ")}
          onBlur={(e) => onChange(e.target.value.split(",").map((entry) => entry.trim()).filter(Boolean))}
          className={cx(inputClass, "font-mono text-[13px]")}
        />
      </Row>
    );
  }

  if (field.type === "boolean") {
    return (
      <Row id={id} label={label} info={info} inline>
        <Switch id={id} checked={Boolean(value)} onChange={onChange} label={label} labelledBy={`${id}-label`} />
      </Row>
    );
  }

  if (models && !field.enum) {
    return (
      <Row id={id} label={label} info={info}>
        <ModelPicker
          id={id}
          value={text}
          onChange={onChange}
          models={models}
          ariaLabel={label}
          emptyLabel={name === "subagent_model" || name === "model_low" ? "Same as orchestrator" : "Claude Code default"}
        />
      </Row>
    );
  }

  const numeric = field.type === "integer" || field.type === "number";
  return (
    <Row id={id} label={label} info={info}>
      {field.enum ? (
        <select
          id={id}
          value={text}
          onChange={(e) => {
            const raw = e.target.value;
            onChange(numeric ? Number(raw) : raw);
          }}
          className={inputClass}
        >
          {field.enum.map((option) => (
            <option key={String(option)} value={String(option)}>
              {option === "" ? "Default" : String(option)}
            </option>
          ))}
        </select>
      ) : (
        <input
          id={id}
          type={numeric ? "number" : "text"}
          inputMode={numeric ? "numeric" : undefined}
          value={text}
          min={field.minimum}
          max={field.maximum}
          step={field.type === "integer" ? 1 : undefined}
          onChange={(e) => {
            const raw = e.target.value;
            if (!numeric) onChange(raw);
            else onChange(raw === "" ? undefined : field.type === "integer" ? Math.trunc(Number(raw)) : Number(raw));
          }}
          className={cx(inputClass, numeric ? "w-32" : "font-mono text-[13px]")}
        />
      )}
    </Row>
  );
}
