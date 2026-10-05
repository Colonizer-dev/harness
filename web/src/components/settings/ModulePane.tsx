import { useCallback, useEffect, useId, useState, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import { MergeTrainSection } from "../MergeTrain";
import type { AutonomyStatus, Mem0Check, Mem0Status, ModelOption, ModuleInfo, SchemaField, WebhookDeliveries as Deliveries } from "../../types";
import { type ImagePull } from "../../useImagePull";
import { Button, Spinner, Switch, cx, inputClass, seconds } from "../ui";
import { ModuleProviderMark, isAdvancedField } from "../settingsGuide";
import { IconCheck, IconChevron } from "../icons";
import { Pane, Row } from "./ui";
import { AutonomyHealth } from "./AutonomyHealth";
import { WebhookDeliveries } from "./WebhookDeliveries";
import { ObservabilityRows } from "./ObservabilityRows";
import { HeadroomRow, JevCompactionNotice, SettingField, VoiceKeyRow, VoiceTestRow, isDirty, kindInfo, useHeadroom, valueOf, type ModuleDraft } from "./moduleFields";

// ---------------------------------------------------------------------------
// Modules: one pane per kind, form generated from the provider's JSON Schema
// ---------------------------------------------------------------------------

const MODEL_KEYS = new Set(["model", "subagent_model", "background_model"]);

// The risk classes above `workspace_write`: saving autonomy that answers them is worth one more
// look, because the model is then settling what other people see, or something near a key.
const RISKY_CEILINGS = new Set(["publish_affecting", "credential_adjacent"]);

/**
 * The confirm a save needs when autonomy would answer questions above `workspace_write` (issue
 * #776), or `null` when nothing needs confirming. An autonomy that is off does not: there is no
 * answer to make. Pure, so the test pins the wording and the one case that matters — the ceiling.
 */
export function riskConfirmation(provider: string, enabled: boolean, settings: Record<string, unknown>): string | null {
  if (!enabled || (provider !== "judge" && provider !== "full_autonomy")) return null;
  const ceiling = String(settings.risk_ceiling ?? "");
  if (!RISKY_CEILINGS.has(ceiling)) return null;
  const risk = ceiling.replace(/_/g, " ");
  return provider === "full_autonomy"
    ? `Full autonomy will answer every question up to ${risk}, with no answer limit, for as long as a colony runs. Continue?`
    : `The judge will answer questions up to ${risk} — what other people see, or something near a key. Continue?`;
}

/**
 * The warning shown when Full autonomy (YOLO) is picked (issue #776): what it does, and the
 * guardrails that still hold. Loud on purpose — it is the one autonomy setting that never stops to
 * ask a person.
 */
export function FullAutonomyWarning() {
  return (
    <div role="alert" className="rounded-xl border border-err/30 bg-err-soft px-4 py-3 text-small-lg text-err">
      <p className="font-medium">Full autonomy: a model answers every question, with no answer limit.</p>
      <p className="mt-1 [overflow-wrap:anywhere]">
        Every question a colony stops to ask, at or below the risk ceiling, is settled by the model you name — however many it
        takes, so a long task runs to the end without you. It still never overrides a denial, never picks a label the agent did
        not offer, and the sandbox, egress and path policies hold. Every answer is logged as the judge's. Needs a model from a
        Model provider: the Claude login cannot be used.
      </p>
    </div>
  );
}

function ImagePullRow({ pull }: { pull: ImagePull }) {
  const { status, error, start } = pull;
  // Re-render once a second while pulling so the elapsed time moves.
  const [, tick] = useState(0);
  useEffect(() => {
    if (status?.state !== "pulling") return;
    const t = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(t);
  }, [status?.state]);

  const line = (tone: string, body: ReactNode, action?: ReactNode) => (
    <div className={cx("flex flex-wrap items-center gap-2 rounded-xl border border-border px-3.5 py-2.5 text-small-lg", tone)}>
      <div className="min-w-0 flex-1">{body}</div>
      {action}
    </div>
  );

  if (error) {
    return line("text-err", <>Could not start the download: {error}</>, <Button size="sm" onClick={() => void start()}>Retry</Button>);
  }
  if (!status || status.state === "idle") {
    return line(
      "text-muted",
      <>The colony image downloads on first use. Get it now so the first colony boots straight away.</>,
      <Button size="sm" onClick={() => void start()}>
        Download image
      </Button>,
    );
  }
  if (status.state === "pulling") {
    return (
      <div className="rounded-xl border border-border px-3.5 py-2.5 text-small-lg text-muted">
        <div className="mb-1.5 flex flex-wrap items-center gap-2">
          <Spinner />
          <span>
            Downloading <code className="font-mono text-text">{status.image}</code> · {seconds(status.started_at)}s
          </span>
          <span className="text-faint">happens once per image</span>
        </div>
        {/* Indeterminate on purpose: msb reports no progress when it is not on a terminal. */}
        <div className="h-1 overflow-hidden rounded-full bg-border" role="progressbar" aria-label={`Downloading ${status.image}`}>
          <div className="pull-slide h-full w-1/3 rounded-full bg-accent" />
        </div>
      </div>
    );
  }
  if (status.state === "failed") {
    return line(
      "text-err",
      <>
        Download of <code className="font-mono">{status.image}</code> failed{status.error ? `: ${status.error}` : ""}. A colony will
        try again when it boots.
      </>,
      <Button size="sm" onClick={() => void start()}>
        Retry
      </Button>,
    );
  }
  const ready =
    status.state === "cached" ? (
      <>
        <code className="font-mono text-text">{status.image}</code> is already on this machine.
      </>
    ) : (
      <>
        <code className="font-mono text-text">{status.image}</code> is ready · downloaded in {seconds(status.started_at, status.finished_at)}s.
      </>
    );
  return line(
    "text-muted",
    <span className="inline-flex items-center gap-1.5">
      <IconCheck size={13} className="text-ok" />
      {ready}
    </span>,
  );
}

export function ModulePane({
  module,
  draft,
  models,
  pull,
  back,
  onDraft,
  onReset,
  onSaved,
}: {
  module: ModuleInfo;
  draft: ModuleDraft;
  models?: ModelOption[];
  /** App owns the one image-pull poller; the sandbox pane and Setup read the same state. */
  pull: ImagePull;
  back?: () => void;
  onDraft: (patch: Partial<ModuleDraft>) => void;
  onReset: () => void;
  onSaved: (module: ModuleInfo) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const providerId = useId();
  const headroom = useHeadroom(module.kind === "agent");
  const info = kindInfo(module.kind);
  const fields = Object.entries(module.schema?.properties ?? {});
  const dirty = isDirty(module, draft);
  const providerInfo = module.providers.find((p) => p.id === draft.provider);

  // The autonomy judge's health (issue #875): fetched when its pane opens and again after a save,
  // since a save is the moment a broken judge is most likely to have just been configured.
  const [judge, setJudge] = useState<AutonomyStatus | null>(null);
  const loadJudge = useCallback(async () => {
    try {
      setJudge(await api.autonomyStatus());
    } catch {
      /* an older mothership has no /api/autonomy/status: there is no line to show */
    }
  }, [api]);
  useEffect(() => {
    if (module.kind === "autonomy") void loadJudge();
  }, [module.kind, loadJudge]);

  // The webhook's deliveries (issue #898): retries waiting and the dead letter, with Replay and
  // Discard. Fetched when the notify pane opens; an older mothership has no route and shows nothing.
  const [deliveries, setDeliveries] = useState<Deliveries | null>(null);
  const [deliveryBusy, setDeliveryBusy] = useState<string | null>(null);
  const loadDeliveries = useCallback(async () => {
    try {
      setDeliveries(await api.webhookDeliveries());
    } catch {
      /* no /api/notify/deliveries on an older mothership */
    }
  }, [api]);
  useEffect(() => {
    if (module.kind === "notify") void loadDeliveries();
  }, [module.kind, loadDeliveries]);
  const replayDeadLetter = (key: string) => {
    setDeliveryBusy(key);
    void api
      .replayDeadLetter(key)
      .then((answer) =>
        answer.delivered ? toast("Delivered.", "success") : toast(`Still failing: ${answer.error ?? "the webhook did not take it"}`, "error"),
      )
      .catch((error) => toast(errorMessage(error), "error"))
      .finally(() => {
        setDeliveryBusy(null);
        void loadDeliveries();
      });
  };
  const discardDeadLetter = (key: string) => {
    setDeliveryBusy(key);
    void api
      .discardDeadLetter(key)
      .catch((error) => toast(errorMessage(error), "error"))
      .finally(() => {
        setDeliveryBusy(null);
        void loadDeliveries();
      });
  };

  const save = async (anyway = false) => {
    // Autonomy above workspace_write asks the operator once before it runs (#776). Turning a save
    // into a no-op when they decline is the whole point: nothing is sent.
    const confirm = riskConfirmation(draft.provider, draft.enabled, draft.settings);
    if (confirm !== null && !window.confirm(confirm)) return;
    setSaving(true);
    try {
      const saved = await api.saveModule(module.kind, {
        provider: draft.provider,
        enabled: draft.enabled,
        settings: draft.settings,
        ...(anyway ? { save_anyway: true } : {}),
      });
      onSaved(saved);
      setSaveError(null);
      toast(`${info.title} module saved`);
      // Choosing a stack is the moment to download it, not the first launch.
      if (module.kind === "sandbox") void pull.start();
      // Switching Headroom on is the moment to download its bundle, too.
      if (module.kind === "agent" && saved.settings?.headroom === true) void headroom.start();
      if (module.kind === "autonomy") void loadJudge();
    } catch (error) {
      const message = errorMessage(error);
      toast(message, "error");
      // The autonomy judge is the one module whose save runs a live test call, so its refusal is
      // shown in the pane with a way past it (issue #875).
      if (module.kind === "autonomy") setSaveError(message);
    } finally {
      setSaving(false);
    }
  };

  const setField = (key: string, value: unknown) => onDraft({ settings: { ...draft.settings, [key]: value } });

  // Ports, timers and paths fold under Advanced; what most people change stays on the page.
  const essentials = fields.filter(([key, field]) => !isAdvancedField(key, field));
  const advanced = fields.filter(([key, field]) => isAdvancedField(key, field));
  const renderField = ([key, field]: [string, SchemaField]) => {
    // Jev's tunables only mean anything with the switch on.
    if (
      module.kind === "agent" &&
      (key === "jev_keep_threshold" || key === "jev_preserve_recent") &&
      draft.settings.jev_compaction !== true
    )
      return null;
    const setting = (
      <SettingField
        key={key}
        name={key}
        field={field}
        value={valueOf(draft.settings, key, field)}
        onChange={(v) => setField(key, v)}
        models={models && MODEL_KEYS.has(key) ? models : undefined}
      />
    );
    // The download sits under the switch that asks for it, one divider group with it.
    const showHeadroom =
      module.kind === "agent" &&
      key === "headroom" &&
      (draft.settings.headroom === true || ["downloading", "unpacking", "failed"].includes(headroom.status?.state ?? ""));
    // The data-egress warning sits under the switch that asks for it, one divider group with it.
    const showJevWarning = module.kind === "agent" && key === "jev_compaction" && draft.settings.jev_compaction === true;
    return showHeadroom || showJevWarning ? (
      <div key={key} className="pb-2.5">
        {setting}
        {showHeadroom && <HeadroomRow headroom={headroom} />}
        {showJevWarning && <JevCompactionNotice />}
      </div>
    ) : (
      setting
    );
  };

  return (
    <Pane
      title={info.title}
      subtitle={info.description}
      back={back}
      aside={
        <span className="flex items-center gap-2 text-small-lg text-muted">
          <span aria-hidden="true">{draft.enabled ? "On" : "Off"}</span>
          <Switch checked={draft.enabled} onChange={(enabled) => onDraft({ enabled })} label={`${info.title} module enabled`} />
        </span>
      }
      footer={
        <>
          <span className="mr-auto text-small-lg text-muted">{dirty ? "Unsaved changes" : "Changes apply to new colonies"}</span>
          {dirty && (
            <Button variant="ghost" onClick={onReset}>
              Reset
            </Button>
          )}
          <Button variant="primary" disabled={!dirty || saving} onClick={() => void save()}>
            {saving && <Spinner />} Save
          </Button>
        </>
      }
    >
      {module.kind === "sandbox" && (
        <div className="mb-1">
          <ImagePullRow pull={pull} />
        </div>
      )}
      {module.kind === "notify" && deliveries && (
        <div className="mb-3">
          <WebhookDeliveries status={deliveries} busy={deliveryBusy} onReplay={replayDeadLetter} onDiscard={discardDeadLetter} />
        </div>
      )}
      {module.kind === "autonomy" && (judge !== null || saveError !== null || draft.provider === "full_autonomy") && (
        <div className="mb-3 flex flex-col gap-2">
          {draft.provider === "full_autonomy" && <FullAutonomyWarning />}
          {judge && <AutonomyHealth status={judge} />}
          {saveError && (
            <div role="alert" className="rounded-xl border border-err/30 bg-err-soft px-4 py-3 text-small-lg text-err">
              <p className="[overflow-wrap:anywhere]">{saveError}</p>
              <Button size="sm" className="mt-2" disabled={saving} onClick={() => void save(true)}>
                {saving && <Spinner />} Save anyway
              </Button>
            </div>
          )}
        </div>
      )}
      <div className={cx(!draft.enabled && "opacity-60")}>
      <div className="divide-y divide-border rounded-xl border border-border bg-panel px-4">
        <Row id={providerId} label="Provider" info={providerInfo?.description ? <p>{providerInfo.description}</p> : undefined}>
          {module.providers.length > 1 ? (
            <select id={providerId} value={draft.provider} onChange={(e) => onDraft({ provider: e.target.value })} className={inputClass}>
              {module.providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
          ) : (
            <span id={providerId} className="block">
              <ModuleProviderMark id={draft.provider} name={providerInfo?.name ?? draft.provider} />
            </span>
          )}
        </Row>

        {draft.provider !== module.provider && fields.length > 0 && (
          <p className="py-2.5 text-small-lg text-warn">These fields belong to the current provider. Save to switch.</p>
        )}

        {essentials.map(renderField)}

        {module.kind === "memory" && draft.provider === "mem0" && <Mem0KeyRow />}

        {module.kind === "voice" && draft.provider !== "browser" && <VoiceKeyRow provider={draft.provider} name={providerInfo?.name ?? draft.provider} />}
        {module.kind === "voice" && <VoiceTestRow unsaved={dirty} />}
        {module.kind === "observability" && <ObservabilityRows unsaved={dirty} />}

        {fields.length === 0 && module.providers.length <= 1 && <p className="py-3 text-body-sm text-faint">Nothing to configure.</p>}
      </div>
      {advanced.length > 0 && (
        <details className="group mt-3 rounded-xl border border-border bg-panel-2/40 [&_summary::-webkit-details-marker]:hidden">
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-xl px-4 py-2.5 text-body-sm font-medium text-muted hover:text-text">
            <IconChevron size={14} className="transition-transform group-open:rotate-90" />
            Advanced
            <span className="font-normal text-faint">
              · {advanced.length} {advanced.length === 1 ? "setting" : "settings"}
            </span>
            <span className="ml-auto text-small font-normal text-faint">Ports, timers and paths — the defaults suit most setups</span>
          </summary>
          <div className="divide-y divide-border border-t border-border px-4">{advanced.map(renderField)}</div>
        </details>
      )}
      {module.kind === "publish" && <MergeTrainSection />}
      </div>
    </Pane>
  );
}

/**
 * The mem0 key has its own row and its own save because it is not a module setting: settings go
 * to modules.json and come back from the API, and a key must do neither. Shown as soon as mem0 is
 * picked, so the key can be in place before the switch is saved.
 */
function Mem0KeyRow() {
  const api = useApi();
  const toast = useToast();
  const id = useId();
  const [status, setStatus] = useState<Mem0Status | null>(null);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<"save" | "remove" | "check" | null>(null);
  const [check, setCheck] = useState<Mem0Check | null>(null);

  useEffect(() => {
    api.mem0Status().then(setStatus, () => setStatus(null));
  }, [api]);

  const saveKey = async (value: string, kind: "save" | "remove") => {
    setBusy(kind);
    setCheck(null);
    try {
      setStatus(await api.saveMem0Key(value));
      setKey("");
      toast(kind === "save" ? "mem0 key saved" : "mem0 key removed");
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setBusy(null);
    }
  };

  const runCheck = async () => {
    setBusy("check");
    try {
      setCheck(await api.checkMem0());
    } catch (error) {
      setCheck({ ok: false, error: errorMessage(error) });
    } finally {
      setBusy(null);
    }
  };

  const state = !status
    ? "Checking…"
    : !status.has_key
      ? "Not set. Until it is, colonies start without shared memory."
      : status.source === "MEM0_API_KEY"
        ? "Read from MEM0_API_KEY."
        : "Saved on this machine.";

  return (
    <div className="space-y-2 py-2.5">
      <label htmlFor={id} className="block text-body-sm font-medium">
        mem0 API key
      </label>
      <p className="text-small-lg text-muted">{state} It stays on the Mothership: colonies never see it.</p>
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
          placeholder={status?.has_key ? "Replace the key" : "m0-…"}
          className={cx(inputClass, "min-w-48 flex-1")}
        />
        <Button type="submit" variant="primary" disabled={!key.trim() || busy !== null}>
          {busy === "save" && <Spinner />} Save
        </Button>
        {status?.source === "saved" && (
          <Button disabled={busy !== null} onClick={() => void saveKey("", "remove")}>
            {busy === "remove" && <Spinner />} Remove
          </Button>
        )}
        <Button disabled={!status?.has_key || busy !== null} onClick={() => void runCheck()}>
          {busy === "check" && <Spinner />} Check
        </Button>
      </form>
      {check && (
        <p role="status" className={cx("text-small-lg", check.ok ? "text-ok" : "text-err")}>
          {check.ok ? "mem0 accepted the key." : check.error}
        </p>
      )}
    </div>
  );
}
