import { useCallback, useEffect, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../context";
import type {
  HarnessStatus,
  LoginView,
  ModelOption,
  ModelProvider,
  ModuleInfo,
  ProviderAuth,
  ProviderHealth,
  ProviderLimits,
  ProviderPreset,
  ProviderWire,
  PullStatus,
  SchemaField,
} from "../types";
import { useModels } from "../useModels";
import { IconCheck, IconChevron, IconCpu, IconExternal, IconKey, IconNetwork, IconPencil, IconPlus, IconX } from "./icons";
import { Badge, Button, ModelInput, Spinner, Switch, cx, inputClass } from "./ui";

export function SettingsDialog({
  open,
  onClose,
  status,
  onStatusChanged,
  onModulesChanged,
}: {
  open: boolean;
  onClose: () => void;
  status: HarnessStatus | null;
  onStatusChanged: () => void;
  onModulesChanged: (modules: ModuleInfo[]) => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const [tab, setTab] = useState<"connections" | "modules">("connections");

  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    else if (!open && dialog.open) dialog.close();
  }, [open]);

  return (
    <dialog
      ref={ref}
      onClose={onClose}
      aria-labelledby="settings-title"
      className="m-auto w-[min(680px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50"
    >
      <div className="flex max-h-[calc(100dvh-24px)] flex-col">
        <div className="flex shrink-0 items-center gap-3 border-b border-border px-5 pt-4">
          <div className="min-w-0 flex-1">
            <h2 id="settings-title" className="text-[16px] font-semibold">
              Settings
            </h2>
            <div role="tablist" className="mt-2 flex gap-1">
              {(["connections", "modules"] as const).map((t) => (
                <button
                  key={t}
                  type="button"
                  role="tab"
                  aria-selected={tab === t}
                  onClick={() => setTab(t)}
                  className={cx(
                    "-mb-px cursor-pointer border-b-2 px-2.5 py-2 text-[13px] font-medium capitalize",
                    tab === t ? "border-accent text-text" : "border-transparent text-muted hover:text-text",
                  )}
                >
                  {t}
                </button>
              ))}
            </div>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close settings"
            className="grid size-8 cursor-pointer place-items-center self-start rounded-lg text-muted hover:bg-panel-2 hover:text-text"
          >
            <IconX size={17} />
          </button>
        </div>
        <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-5 py-5">
          {open && tab === "connections" && <ConnectionsTab status={status} onStatusChanged={onStatusChanged} />}
          {open && tab === "modules" && <ModulesTab onModulesChanged={onModulesChanged} />}
        </div>
      </div>
    </dialog>
  );
}

function Section({ title, ok, detail, children }: { title: string; ok?: boolean | null; detail?: string; children: ReactNode }) {
  return (
    <section className="space-y-3 border-b border-border pb-5 last:border-b-0 last:pb-0 [&+section]:pt-5">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-[14.5px] font-semibold">{title}</h3>
        {ok != null && <Badge tone={ok ? "ok" : "err"}>{ok ? "Connected" : "Not connected"}</Badge>}
        {detail && <span className="text-[12.5px] text-muted [overflow-wrap:anywhere]">{detail}</span>}
      </div>
      {children}
    </section>
  );
}

const IDLE_LOGIN: LoginView = { state: "idle", url: null, message: null };

function ConnectionsTab({ status, onStatusChanged }: { status: HarnessStatus | null; onStatusChanged: () => void }) {
  const api = useApi();
  const toast = useToast();
  const [githubToken, setGithubToken] = useState("");
  const [claudeToken, setClaudeToken] = useState("");
  const [saving, setSaving] = useState<"github" | "claude" | null>(null);
  const [login, setLogin] = useState<LoginView>(IDLE_LOGIN);
  const [code, setCode] = useState("");
  const flowActive = login.state === "starting" || login.state === "awaiting_code" || login.state === "verifying";

  useEffect(() => {
    if (!flowActive) return;
    const timer = setInterval(async () => {
      try {
        const view = await api.claudeLogin();
        setLogin(view);
        if (view.state === "done") {
          toast(view.message || "Claude subscription connected");
          onStatusChanged();
        }
      } catch {
        /* retry on next tick */
      }
    }, 1000);
    return () => clearInterval(timer);
  }, [api, flowActive, onStatusChanged, toast]);

  const run = async (kind: "github" | "claude", fn: () => Promise<unknown>, success: string) => {
    setSaving(kind);
    try {
      await fn();
      toast(success);
      onStatusChanged();
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setSaving(null);
    }
  };

  const startLogin = async () => {
    setCode("");
    try {
      setLogin(await api.claudeLoginStart());
    } catch (error) {
      setLogin({ state: "error", url: null, message: errorMessage(error) });
    }
  };

  const submitCode = async (event: FormEvent) => {
    event.preventDefault();
    try {
      setLogin(await api.claudeLoginCode(code.trim()));
    } catch (error) {
      toast(errorMessage(error), "error");
    }
  };

  const github = status?.github;
  const claude = status?.claude;

  return (
    <div>
      <Section
        title="GitHub"
        ok={github ? github.connected : null}
        detail={github?.connected ? `@${github.login} via ${github.source}` : github?.error ? github.error.split("\n")[0] : undefined}
      >
        <p className="text-[13px] text-muted">
          Colonizer uses your <code className="rounded bg-panel-2 px-1 font-mono text-[12px]">gh auth login</code> session
          automatically. You can also save a token (fine-grained: Contents, Issues and Pull requests — read &amp; write).
        </p>
        <form
          className="flex flex-wrap gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            void run("github", () => api.setGithubToken(githubToken.trim()), "GitHub token saved").then(() => setGithubToken(""));
          }}
        >
          <input
            type="password"
            autoComplete="off"
            value={githubToken}
            onChange={(e) => setGithubToken(e.target.value)}
            placeholder="github_pat_… or ghp_…"
            aria-label="GitHub token"
            className={cx(inputClass, "min-w-48 flex-1")}
          />
          <Button type="submit" variant="primary" disabled={!githubToken.trim() || saving !== null}>
            {saving === "github" && <Spinner />} Save
          </Button>
          <Button disabled={saving !== null} onClick={() => run("github", () => api.deleteGithubToken(), "Saved GitHub token removed")}>
            Remove saved token
          </Button>
        </form>
      </Section>

      <Section title="Claude" ok={claude ? claude.configured : null} detail={claude?.configured ? `via ${claude.source}` : undefined}>
        <p className="text-[13px] text-muted">
          Sign in with your Claude Pro or Max subscription. This runs{" "}
          <code className="rounded bg-panel-2 px-1 font-mono text-[12px]">claude setup-token</code> on the Mothership (this machine) and
          saves a 1-year token there. microVMs only ever see a placeholder; the real token is swapped in for requests to
          api.anthropic.com.
        </p>
        <div className="flex flex-wrap gap-2">
          <Button variant="primary" onClick={startLogin} disabled={flowActive}>
            {login.state === "starting" && <Spinner />} Log in with Claude subscription
          </Button>
          <Button onClick={() => run("claude", () => api.deleteClaudeToken(), "Saved Claude token removed")} disabled={saving !== null}>
            Remove saved token
          </Button>
        </div>

        {login.state !== "idle" && (
          <div className="space-y-3 rounded-xl border border-dashed border-border-strong p-3.5">
            {login.state === "starting" && (
              <p className="flex items-center gap-2 text-[13px] text-muted">
                <Spinner /> Starting claude setup-token…
              </p>
            )}
            {(login.state === "awaiting_code" || login.state === "verifying") && (
              <>
                <ol className="space-y-1.5 text-[13px]">
                  <li>
                    <span className="mr-1 font-semibold">1.</span>
                    {login.url?.startsWith("https://") ? (
                      <a href={login.url} target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1 font-medium text-accent hover:underline">
                        Open the Claude sign-in page <IconExternal size={12} />
                      </a>
                    ) : (
                      "Waiting for the sign-in link…"
                    )}{" "}
                    and approve access.
                  </li>
                  <li>
                    <span className="mr-1 font-semibold">2.</span>Paste the code it shows:
                  </li>
                </ol>
                <form onSubmit={submitCode} className="flex flex-wrap gap-2">
                  <input
                    value={code}
                    onChange={(e) => setCode(e.target.value)}
                    autoComplete="off"
                    placeholder="Sign-in code"
                    aria-label="Sign-in code"
                    className={cx(inputClass, "min-w-48 flex-1 font-mono")}
                  />
                  <Button type="submit" variant="primary" disabled={!code.trim() || login.state === "verifying"}>
                    {login.state === "verifying" && <Spinner />} Submit
                  </Button>
                  <Button onClick={async () => setLogin(await api.claudeLoginCancel().catch(() => IDLE_LOGIN))}>Cancel</Button>
                </form>
              </>
            )}
            {login.state === "done" && (
              <p className="flex items-center gap-2 text-[13px] text-ok">
                <IconCheck size={14} /> {login.message ?? "Connected"}
              </p>
            )}
            {login.state === "error" && <p className="text-[13px] text-err">{login.message ?? "Sign-in failed"}</p>}
          </div>
        )}

        <details className="group text-[13px]">
          <summary className="cursor-pointer text-muted hover:text-text">Use an existing token or API key instead</summary>
          <form
            className="mt-2 flex flex-wrap gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              void run("claude", () => api.setClaudeToken(claudeToken.trim()), "Claude token saved").then(() => setClaudeToken(""));
            }}
          >
            <input
              type="password"
              autoComplete="off"
              value={claudeToken}
              onChange={(e) => setClaudeToken(e.target.value)}
              placeholder="sk-ant-oat01-… or sk-ant-api…"
              aria-label="Claude token"
              className={cx(inputClass, "min-w-48 flex-1")}
            />
            <Button type="submit" disabled={!claudeToken.trim() || saving !== null}>
              Save
            </Button>
          </form>
        </details>
      </Section>

      <ProvidersSection />

      <Section title="Runtime">
        {status ? (
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1.5 text-[13px]">
            <dt className="text-muted">microsandbox</dt>
            <dd className={status.sandbox.msb_version ? "" : "text-err"}>{status.sandbox.msb_version ?? "not found"}</dd>
            <dt className="text-muted">Image</dt>
            <dd className="font-mono text-[12.5px]">{status.sandbox.image}</dd>
            <dt className="text-muted">Agent binary</dt>
            <dd className={cx("font-mono text-[12.5px] [overflow-wrap:anywhere]", status.sandbox.claude_bin_error && "text-err")}>
              {status.sandbox.claude_bin ?? status.sandbox.claude_bin_error ?? "—"}
            </dd>
            <dt className="text-muted">Mesh</dt>
            <dd className={status.mesh?.error ? "text-err" : ""}>
              {status.mesh
                ? status.mesh.enabled
                  ? [status.mesh.provider, status.mesh.state, status.mesh.harness_ip, status.mesh.error].filter(Boolean).join(" · ")
                  : "disabled"
                : "—"}
            </dd>
          </dl>
        ) : (
          <p className="text-[13px] text-muted">Mothership status unavailable.</p>
        )}
      </Section>
    </div>
  );
}

const KIND_INFO: Record<string, { title: string; description: string }> = {
  source: { title: "Source", description: "Where tasks come from" },
  sandbox: { title: "Sandbox", description: "Where agents run" },
  mesh: { title: "Mesh", description: "Private network between the Mothership and colonies" },
  agent: { title: "Agent", description: "The coding agent inside each microVM" },
  interfaces: { title: "Interfaces", description: "Panels in the colony view" },
  publish: { title: "Publish", description: "Where finished work goes" },
  memory: { title: "Memory", description: "Shared notes colonies can search and propose" },
  watchdog: { title: "Watchdog", description: "Notices stalled colonies and nudges them" },
};

const MODEL_KEYS = new Set(["model", "subagent_model", "background_model"]);

function ModulesTab({ onModulesChanged }: { onModulesChanged: (modules: ModuleInfo[]) => void }) {
  const api = useApi();
  const [modules, setModules] = useState<ModuleInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const models = useModels();

  useEffect(() => {
    let cancelled = false;
    api
      .modules()
      .then((list) => !cancelled && setModules(list))
      .catch((e) => !cancelled && setError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api]);

  if (error) return <p className="text-[13px] text-err">{error}</p>;
  if (!modules) {
    return (
      <div className="flex items-center gap-2 text-[13px] text-muted">
        <Spinner /> Loading modules…
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <p className="text-[13px] text-muted">Every part of the harness is a module. Changes apply to new colonies.</p>
      {modules.map((module) => (
        <ModuleCard
          key={module.kind}
          module={module}
          models={module.kind === "agent" ? models : undefined}
          onSaved={(saved) => {
            const next = modules.map((m) => (m.kind === saved.kind ? saved : m));
            setModules(next);
            onModulesChanged(next);
          }}
        />
      ))}
    </div>
  );
}

function valueOf(settings: Record<string, unknown>, key: string, field: SchemaField): unknown {
  if (settings[key] !== undefined) return settings[key];
  if (field.default !== undefined) return field.default;
  return field.type === "boolean" ? false : "";
}

/**
 * The colony image's download, kept off the launch path.
 *
 * A cold pull of the default image measured 108 s. Started here when a stack is
 * saved, it happens while someone is looking at Settings instead of while their
 * first colony sits on a spinner. Polls only while a pull is running.
 */
function useImagePull(active: boolean) {
  const api = useApi();
  const [status, setStatus] = useState<PullStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!active) return;
    let stop = false;
    api
      .sandboxPullStatus()
      .then((s) => !stop && setStatus(s))
      .catch(() => {});
    return () => {
      stop = true;
    };
  }, [active, api]);

  useEffect(() => {
    if (!active || status?.state !== "pulling") return;
    const timer = setInterval(() => {
      api
        .sandboxPullStatus()
        .then(setStatus)
        .catch(() => {});
    }, 1000);
    return () => clearInterval(timer);
  }, [active, api, status?.state]);

  const start = useCallback(async () => {
    setError(null);
    try {
      setStatus(await api.sandboxPull());
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api]);

  return { status, error, start };
}

const seconds = (from: string | null, to?: string | null) => {
  if (!from) return 0;
  const end = to ? Date.parse(to) : Date.now();
  return Math.max(0, Math.round((end - Date.parse(from)) / 1000));
};

function ImagePullRow({ pull }: { pull: ReturnType<typeof useImagePull> }) {
  const { status, error, start } = pull;
  // Re-render once a second while pulling so the elapsed time moves.
  const [, tick] = useState(0);
  useEffect(() => {
    if (status?.state !== "pulling") return;
    const t = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(t);
  }, [status?.state]);

  const line = (tone: string, body: ReactNode, action?: ReactNode) => (
    <div className={cx("flex flex-wrap items-center gap-2 border-t border-border px-4 py-2.5 text-[12.5px]", tone)}>
      <div className="min-w-0 flex-1">{body}</div>
      {action}
    </div>
  );

  if (error) {
    return line("text-err", <>Could not start the download: {error}</>, <Button onClick={() => void start()}>Retry</Button>);
  }
  if (!status || status.state === "idle") {
    return line(
      "text-muted",
      <>The colony image downloads on first use. Get it now so the first colony boots straight away.</>,
      <Button onClick={() => void start()}>Download image</Button>,
    );
  }
  if (status.state === "pulling") {
    return (
      <div className="border-t border-border px-4 py-2.5 text-[12.5px] text-muted">
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
      <Button onClick={() => void start()}>Retry</Button>,
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

function ModuleCard({
  module,
  models,
  onSaved,
}: {
  module: ModuleInfo;
  models?: ModelOption[];
  onSaved: (module: ModuleInfo) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [provider, setProvider] = useState(module.provider);
  const [enabled, setEnabled] = useState(module.enabled);
  const [settings, setSettings] = useState<Record<string, unknown>>(module.settings ?? {});
  const [saving, setSaving] = useState(false);
  const pull = useImagePull(module.kind === "sandbox");
  const info = KIND_INFO[module.kind] ?? { title: module.kind, description: "" };
  const fields = Object.entries(module.schema?.properties ?? {});
  const dirty =
    provider !== module.provider || enabled !== module.enabled || JSON.stringify(settings) !== JSON.stringify(module.settings ?? {});
  const providerInfo = module.providers.find((p) => p.id === provider);

  const save = async () => {
    setSaving(true);
    try {
      const saved = await api.saveModule(module.kind, { provider, enabled, settings });
      setSettings(saved.settings ?? {});
      onSaved(saved);
      toast(`${info.title} module saved`);
      // Choosing a stack is the moment to download it, not the first launch.
      if (module.kind === "sandbox") void pull.start();
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setSaving(false);
    }
  };

  const setField = (key: string, value: unknown) => setSettings((s) => ({ ...s, [key]: value }));

  return (
    <div className="rounded-xl border border-border bg-panel">
      <div className="flex flex-wrap items-center gap-3 px-4 py-3">
        <div className="min-w-0 flex-1">
          <div className="text-[14px] font-semibold">{info.title}</div>
          <div className="text-[12.5px] text-muted">{info.description}</div>
        </div>
        <Switch checked={enabled} onChange={setEnabled} label={`Enable ${info.title} module`} />
      </div>
      {module.kind === "sandbox" && <ImagePullRow pull={pull} />}
      <div className={cx("space-y-3 border-t border-border px-4 py-3", !enabled && "opacity-60")}>
        <label className="block space-y-1">
          <span className="text-[12.5px] font-medium text-muted">Provider</span>
          {module.providers.length > 1 ? (
            <select value={provider} onChange={(e) => setProvider(e.target.value)} className={inputClass}>
              {module.providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
          ) : (
            <span className="block text-[13.5px]">{providerInfo?.name ?? provider}</span>
          )}
          {providerInfo?.description && <span className="block text-[12px] text-faint">{providerInfo.description}</span>}
        </label>

        {provider !== module.provider && fields.length > 0 && (
          <p className="text-[12px] text-warn">Settings below belong to the current provider; save to switch.</p>
        )}

        {fields.length > 0 && (
          <div className="grid gap-3 sm:grid-cols-2">
            {fields.map(([key, field]) => (
              <SettingField
                key={key}
                name={key}
                field={field}
                value={valueOf(settings, key, field)}
                onChange={(v) => setField(key, v)}
                models={models && MODEL_KEYS.has(key) ? models : undefined}
              />
            ))}
          </div>
        )}

        <div className="flex justify-end gap-2">
          {dirty && (
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                setProvider(module.provider);
                setEnabled(module.enabled);
                setSettings(module.settings ?? {});
              }}
            >
              Reset
            </Button>
          )}
          <Button size="sm" variant="primary" disabled={!dirty || saving} onClick={save}>
            {saving && <Spinner />} Save
          </Button>
        </div>
      </div>
    </div>
  );
}

function SettingField({
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
  const label = field.title ?? name;
  if (models && !field.enum && field.type !== "boolean") {
    return (
      <label className="block min-w-0 space-y-1">
        <span className="text-[12.5px] font-medium text-muted">{label}</span>
        <ModelInput
          value={value === undefined || value === null ? "" : String(value)}
          onChange={onChange}
          models={models}
          placeholder={name === "subagent_model" ? "Same as orchestrator" : "Claude Code default"}
        />
        {field.description && <span className="block text-[12px] text-faint">{field.description}</span>}
      </label>
    );
  }
  if (field.type === "boolean") {
    return (
      <div className="flex items-start gap-2.5 sm:col-span-2">
        <Switch checked={Boolean(value)} onChange={onChange} label={label} />
        <span className="text-[13px] leading-snug">
          <span className="font-medium">{label}</span>
          {field.description && <span className="block text-[12px] text-muted">{field.description}</span>}
        </span>
      </div>
    );
  }
  const numeric = field.type === "integer" || field.type === "number";
  return (
    <label className="block min-w-0 space-y-1">
      <span className="text-[12.5px] font-medium text-muted">{label}</span>
      {field.enum ? (
        <select
          value={String(value ?? "")}
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
          type={numeric ? "number" : "text"}
          value={value === undefined || value === null ? "" : String(value)}
          min={field.minimum}
          max={field.maximum}
          step={field.type === "integer" ? 1 : undefined}
          onChange={(e) => {
            const raw = e.target.value;
            if (!numeric) onChange(raw);
            else onChange(raw === "" ? undefined : field.type === "integer" ? Math.trunc(Number(raw)) : Number(raw));
          }}
          className={inputClass}
        />
      )}
      {field.description && <span className="block text-[12px] text-faint">{field.description}</span>}
    </label>
  );
}

// ---------------------------------------------------------------------------
// Model providers (§6.3)
// ---------------------------------------------------------------------------

type ProviderDraft = ProviderLimits & { id: string; name: string; base_url: string; auth: ProviderAuth; wire: ProviderWire; models: string[] };

const DEFAULT_TIMEOUT = 600;
const DEFAULT_LIMITS: ProviderLimits = { timeout_secs: DEFAULT_TIMEOUT, max_concurrent: null, queue_timeout_secs: null, context_tokens: null, fallback_model: null };

const PRESETS: Record<ProviderPreset, ProviderDraft> = {
  deepseek: {
    id: "deepseek",
    name: "DeepSeek",
    base_url: "https://api.deepseek.com/anthropic",
    auth: "x-api-key",
    wire: "anthropic",
    models: ["deepseek-flash", "deepseek-v4-pro"],
    ...DEFAULT_LIMITS,
  },
  // OpenAI is not Anthropic-compatible: the gateway translates this one in both directions.
  openai: {
    id: "openai",
    name: "OpenAI",
    base_url: "https://api.openai.com",
    auth: "bearer",
    wire: "openai",
    models: ["gpt-5.6", "gpt-5.5"],
    ...DEFAULT_LIMITS,
    // Conservative: Claude Code compacts against this, and overshooting the real limit costs a failed turn.
    context_tokens: 272_000,
  },
  // Local servers are slow and usually serve one or two requests at a time.
  local: { id: "local", name: "Local", base_url: "http://127.0.0.1:8080", auth: "none", wire: "anthropic", models: [], ...DEFAULT_LIMITS, timeout_secs: 900, max_concurrent: 1 },
  custom: { id: "", name: "", base_url: "", auth: "x-api-key", wire: "anthropic", models: [], ...DEFAULT_LIMITS },
};

/** Integer fields of the Advanced group, with the ranges the Mothership accepts. */
const LIMIT_RANGE = {
  timeout_secs: [30, 3600],
  max_concurrent: [1, 64],
  queue_timeout_secs: [1, 3600],
  context_tokens: [1024, 2_000_000],
} as const;

type LimitKey = keyof typeof LIMIT_RANGE;

function parseLimit(key: LimitKey, raw: string): { value: number | null; error: string | null } {
  const text = raw.trim().replace(/[_,\s]/g, "");
  if (!text) return { value: null, error: null };
  const [min, max] = LIMIT_RANGE[key];
  const value = Number(text);
  if (!Number.isInteger(value) || value < min || value > max) {
    return { value: null, error: `A whole number from ${min.toLocaleString()} to ${max.toLocaleString()}` };
  }
  return { value, error: null };
}

const limitText = (value: number | null | undefined) => (value == null ? "" : String(value));

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${+(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

/** Short labels for non-default gateway settings, for rows and the collapsed Advanced summary. */
function limitLabels(limits: Partial<ProviderLimits>): string[] {
  const labels: string[] = [];
  if (limits.max_concurrent != null) labels.push(`max ${limits.max_concurrent}`);
  if (limits.timeout_secs != null && limits.timeout_secs !== DEFAULT_TIMEOUT) labels.push(`timeout ${limits.timeout_secs} s`);
  if (limits.queue_timeout_secs != null) labels.push(`queue ${limits.queue_timeout_secs} s`);
  if (limits.context_tokens != null) labels.push(`context ${formatTokens(limits.context_tokens)}`);
  if (limits.fallback_model) labels.push(`fallback ${limits.fallback_model}`);
  return labels;
}

const PRESET_LABEL: Record<ProviderPreset, string> = { deepseek: "DeepSeek preset", openai: "OpenAI preset", local: "Local preset", custom: "Custom" };

const AUTH_LABEL: Record<ProviderAuth, string> = {
  "x-api-key": "API key (x-api-key header)",
  bearer: "Bearer token (Authorization header)",
  none: "No authentication",
};

type Editing = { mode: "new"; preset: ProviderPreset } | { mode: "edit"; id: string } | null;

function ProvidersSection() {
  const api = useApi();
  const [providers, setProviders] = useState<ModelProvider[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<Editing>(null);
  const [health, setHealth] = useState<Record<string, HealthView>>({});

  const load = useCallback(async () => {
    try {
      setProviders(await api.providers());
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api]);

  useEffect(() => {
    void load();
  }, [load]);

  // In-flight and queued counts change as colonies work; refresh them quietly.
  useEffect(() => {
    const timer = setInterval(() => {
      if (document.hidden) return;
      api
        .providers()
        .then(setProviders)
        .catch(() => {});
    }, 5000);
    return () => clearInterval(timer);
  }, [api]);

  const check = async (id: string) => {
    setHealth((h) => ({ ...h, [id]: { state: "checking" } }));
    try {
      const result = await api.providerHealth(id);
      setHealth((h) => ({ ...h, [id]: { state: "done", result } }));
    } catch (e) {
      setHealth((h) => ({ ...h, [id]: { state: "failed", message: errorMessage(e) } }));
    }
  };

  const has = (id: string) => providers?.some((p) => p.id === id) ?? false;
  const upsert = (saved: ModelProvider) =>
    setProviders((list) => {
      const rest = (list ?? []).filter((p) => p.id !== saved.id);
      const index = (list ?? []).findIndex((p) => p.id === saved.id);
      if (index < 0) return [...rest, saved];
      rest.splice(index, 0, saved);
      return rest;
    });

  return (
    <Section title="Model providers" detail={providers ? `${providers.length} configured` : undefined}>
      <p className="text-[13px] text-muted">
        Anthropic-compatible endpoints for orchestrator, subagent and background models. Pick a provider model as{" "}
        <code className="rounded bg-panel-2 px-1 font-mono text-[12px]">provider/model</code>, for example{" "}
        <code className="rounded bg-panel-2 px-1 font-mono text-[12px]">deepseek/deepseek-flash</code>.
      </p>
      {error && <p className="text-[13px] text-err">{error}</p>}
      {!providers && !error && (
        <p className="flex items-center gap-2 text-[13px] text-muted">
          <Spinner /> Loading providers…
        </p>
      )}
      {providers && (
        <div className="space-y-2">
          {providers.length === 0 && editing?.mode !== "new" && (
            <p className="rounded-xl border border-dashed border-border-strong px-3.5 py-3 text-[13px] text-muted">
              Only Claude models are available. Add DeepSeek, a local server, or any Anthropic-compatible endpoint.
            </p>
          )}
          {providers.map((provider) =>
            editing?.mode === "edit" && editing.id === provider.id ? (
              <ProviderForm
                key={provider.id}
                initial={provider}
                preset={provider.preset}
                takenIds={[]}
                onCancel={() => setEditing(null)}
                onSaved={(saved) => {
                  upsert(saved);
                  setHealth((h) => {
                    const next = { ...h };
                    delete next[saved.id];
                    return next;
                  });
                  setEditing(null);
                }}
                onDeleted={(id) => {
                  setProviders((list) => list?.filter((p) => p.id !== id) ?? null);
                  setEditing(null);
                }}
              />
            ) : (
              <ProviderRow
                key={provider.id}
                provider={provider}
                health={health[provider.id]}
                disabled={editing !== null}
                onCheck={() => void check(provider.id)}
                onEdit={() => setEditing({ mode: "edit", id: provider.id })}
              />
            ),
          )}
          {editing?.mode === "new" && (
            <ProviderForm
              key={`new-${editing.preset}`}
              preset={editing.preset}
              takenIds={providers.map((p) => p.id)}
              onCancel={() => setEditing(null)}
              onSaved={(saved) => {
                upsert(saved);
                setEditing(null);
              }}
            />
          )}
        </div>
      )}
      <div className="flex flex-wrap gap-2">
        <Button size="sm" disabled={!providers || editing !== null || has("deepseek")} onClick={() => setEditing({ mode: "new", preset: "deepseek" })}>
          <IconPlus size={13} /> DeepSeek
        </Button>
        <Button size="sm" disabled={!providers || editing !== null || has("openai")} onClick={() => setEditing({ mode: "new", preset: "openai" })}>
          <IconPlus size={13} /> OpenAI
        </Button>
        <Button size="sm" disabled={!providers || editing !== null || has("local")} onClick={() => setEditing({ mode: "new", preset: "local" })}>
          <IconPlus size={13} /> Local
        </Button>
        <Button size="sm" disabled={!providers || editing !== null} onClick={() => setEditing({ mode: "new", preset: "custom" })}>
          <IconPlus size={13} /> Custom
        </Button>
      </div>
      <p className="flex items-start gap-1.5 text-[12px] text-faint">
        <IconKey size={13} className="mt-px shrink-0" />
        Requests go through the Mothership gateway. Colonies never see provider keys, and private-network servers (LAN, tailnet) work without extra setup.
      </p>
    </Section>
  );
}

function KeyBadge({ provider }: { provider: ModelProvider }) {
  if (provider.auth === "none") return <Badge>No key needed</Badge>;
  return provider.has_key ? <Badge tone="ok">Key saved</Badge> : <Badge tone="warn">No key</Badge>;
}

type HealthView = { state: "checking" } | { state: "done"; result: ProviderHealth } | { state: "failed"; message: string };

function HealthStatus({ health }: { health: HealthView }) {
  if (health.state === "checking") {
    return (
      <span role="status" className="flex items-center gap-1.5 text-[12px] text-muted">
        <Spinner className="size-3" /> Checking from the Mothership…
      </span>
    );
  }
  let tone: "ok" | "warn" | "err";
  let text: string;
  let title: string | undefined;
  if (health.state === "failed") {
    tone = "err";
    text = `Check failed: ${health.message}`;
  } else {
    const r = health.result;
    const latency = r.latency_ms != null ? `${Math.round(r.latency_ms)} ms` : null;
    const models = r.models.length ? `${r.models.length} model${r.models.length === 1 ? "" : "s"}` : null;
    if (!r.reachable) {
      tone = "err";
      text = `Unreachable${r.error ? `: ${r.error}` : ""}`;
    } else if (r.status != null && (r.status < 200 || r.status > 299)) {
      tone = "warn";
      text = [`HTTP ${r.status}`, latency, r.error].filter(Boolean).join(" · ");
    } else {
      tone = "ok";
      text = ["Reachable", latency, models].filter(Boolean).join(" · ");
    }
    title = [r.models.length ? `Models: ${r.models.join(", ")}` : null, r.checked_at ? `Checked ${new Date(r.checked_at).toLocaleTimeString()}` : null]
      .filter(Boolean)
      .join("\n");
  }
  return (
    <span
      role="status"
      title={title || undefined}
      className={cx("flex items-start gap-1.5 text-[12px] font-medium", tone === "ok" ? "text-ok" : tone === "warn" ? "text-warn" : "text-err")}
    >
      <span className="mt-[5px] size-1.5 shrink-0 rounded-full bg-current" />
      <span className="min-w-0 [overflow-wrap:anywhere]">{text}</span>
    </span>
  );
}

function ProviderRow({
  provider,
  health,
  disabled,
  onCheck,
  onEdit,
}: {
  provider: ModelProvider;
  health?: HealthView;
  disabled: boolean;
  onCheck: () => void;
  onEdit: () => void;
}) {
  const running = provider.in_flight ?? 0;
  const queued = provider.queued ?? 0;
  const limits = limitLabels(provider);
  return (
    <div className="flex flex-wrap items-start gap-x-3 gap-y-2 rounded-xl border border-border px-3.5 py-3">
      <div className="grid size-8 shrink-0 place-items-center rounded-lg bg-panel-2 text-muted">
        <IconCpu size={16} />
      </div>
      <div className="min-w-0 flex-1 basis-48">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-[14px] font-semibold">{provider.name}</span>
          <span className="font-mono text-[12px] text-faint">{provider.id}</span>
          <Badge tone={provider.preset === "custom" ? "neutral" : "accent"}>{PRESET_LABEL[provider.preset] ?? provider.preset}</Badge>
          <KeyBadge provider={provider} />
          {(running > 0 || queued > 0) && (
            <Badge tone={queued > 0 ? "warn" : "info"} pulse={running > 0}>
              {[running > 0 && `${running} running`, queued > 0 && `${queued} queued`].filter(Boolean).join(" · ")}
            </Badge>
          )}
        </div>
        <div className="mt-0.5 font-mono text-[12px] text-muted [overflow-wrap:anywhere]">{provider.base_url}</div>
        {limits.length > 0 && (
          <div className="mt-1.5 flex flex-wrap gap-1">
            {limits.map((label) => (
              <Badge key={label}>{label}</Badge>
            ))}
          </div>
        )}
        <div className="mt-1.5 flex flex-wrap gap-1">
          {provider.models.length === 0 ? (
            <span className="text-[12px] text-faint">No models listed; type model IDs where you pick a model.</span>
          ) : (
            provider.models.map((model) => (
              <span key={model} className="rounded bg-panel-2 px-1.5 py-px font-mono text-[11.5px] text-muted">
                {model}
              </span>
            ))
          )}
        </div>
        {health && (
          <div className="mt-2">
            <HealthStatus health={health} />
          </div>
        )}
      </div>
      <div className="flex shrink-0 gap-1.5">
        <Button size="sm" disabled={health?.state === "checking"} onClick={onCheck} title="Check that the Mothership can reach this provider">
          {health?.state === "checking" ? <Spinner className="size-3" /> : <IconNetwork size={13} />} Check
        </Button>
        <Button size="sm" disabled={disabled} onClick={onEdit}>
          <IconPencil size={13} /> Edit
        </Button>
      </div>
    </div>
  );
}

const PROVIDER_ID = /^[a-z0-9][a-z0-9-]{0,31}$/;

function ProviderForm({
  initial,
  preset,
  takenIds,
  onCancel,
  onSaved,
  onDeleted,
}: {
  initial?: ModelProvider;
  preset: ProviderPreset;
  takenIds: string[];
  onCancel: () => void;
  onSaved: (provider: ModelProvider) => void;
  onDeleted?: (id: string) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const start = initial ?? PRESETS[preset];
  const wire = initial?.wire ?? PRESETS[preset].wire;
  const [id, setId] = useState(start.id);
  const [name, setName] = useState(start.name);
  const [baseUrl, setBaseUrl] = useState(start.base_url);
  const [auth, setAuth] = useState<ProviderAuth>(start.auth);
  const [models, setModels] = useState<string[]>(start.models);
  const [keyMode, setKeyMode] = useState<"keep" | "replace" | "remove">(initial?.has_key ? "keep" : "replace");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<"save" | "delete" | null>(null);
  const [limitDraft, setLimitDraft] = useState<Record<LimitKey, string>>({
    timeout_secs: limitText(start.timeout_secs),
    max_concurrent: limitText(start.max_concurrent),
    queue_timeout_secs: limitText(start.queue_timeout_secs),
    context_tokens: limitText(start.context_tokens),
  });
  const [fallback, setFallback] = useState(start.fallback_model ?? "");
  // A new Local provider opens Advanced so the prefilled limits are visible.
  const [advancedOpen, setAdvancedOpen] = useState(!initial && preset === "local");
  const anthropicModels = useModels().filter((m) => m.provider === "anthropic");
  const limits = {
    timeout_secs: parseLimit("timeout_secs", limitDraft.timeout_secs),
    max_concurrent: parseLimit("max_concurrent", limitDraft.max_concurrent),
    queue_timeout_secs: parseLimit("queue_timeout_secs", limitDraft.queue_timeout_secs),
    context_tokens: parseLimit("context_tokens", limitDraft.context_tokens),
  };
  const limitsInvalid = Object.values(limits).some((l) => l.error);
  const setLimit = (k: LimitKey, value: string) => setLimitDraft((d) => ({ ...d, [k]: value }));
  const advancedSummary = limitLabels({
    timeout_secs: limits.timeout_secs.value ?? undefined,
    max_concurrent: limits.max_concurrent.value,
    queue_timeout_secs: limits.queue_timeout_secs.value,
    context_tokens: limits.context_tokens.value,
    fallback_model: fallback || null,
  });

  const isNew = !initial;
  const idError = !isNew
    ? null
    : !PROVIDER_ID.test(id)
      ? "Lowercase letters, digits and dashes"
      : id === "anthropic"
        ? "“anthropic” is reserved"
        : takenIds.includes(id)
          ? "Already in use"
          : null;
  const urlError = /^https?:\/\/[^\s/]+(\/\S*)?$/.test(baseUrl.trim()) ? null : "An http(s) URL";
  const keyError = auth !== "none" && keyMode === "replace" && initial?.has_key && !key.trim() ? "Paste the new key" : null;
  const invalid = Boolean(idError || urlError || keyError || limitsInvalid || !name.trim());
  const loopback = /^https?:\/\/(127\.|localhost|\[::1\])/.test(baseUrl.trim());

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (invalid) return;
    setBusy("save");
    let api_key: string | undefined;
    if (auth !== "none") {
      if (keyMode === "remove") api_key = "";
      else if (keyMode === "replace" && key.trim()) api_key = key.trim();
    }
    try {
      const saved = await api.saveProvider(id, {
        name: name.trim(),
        base_url: baseUrl.trim(),
        auth,
        wire,
        models,
        preset: initial?.preset ?? preset,
        api_key,
        timeout_secs: limits.timeout_secs.value,
        max_concurrent: limits.max_concurrent.value,
        queue_timeout_secs: limits.queue_timeout_secs.value,
        context_tokens: limits.context_tokens.value,
        fallback_model: fallback || null,
      });
      toast(`${saved.name} saved`);
      onSaved(saved);
    } catch (e) {
      toast(errorMessage(e), "error");
      setBusy(null);
    }
  };

  const remove = async () => {
    if (!initial || !window.confirm(`Remove ${initial.name}? Colonies using ${initial.id}/… models will fall back to the default model.`)) return;
    setBusy("delete");
    try {
      await api.deleteProvider(initial.id);
      toast(`${initial.name} removed`);
      onDeleted?.(initial.id);
    } catch (e) {
      toast(errorMessage(e), "error");
      setBusy(null);
    }
  };

  const fieldLabel = "text-[12.5px] font-medium text-muted";

  return (
    <form onSubmit={save} className="space-y-3 rounded-xl border border-accent/40 bg-panel p-3.5">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-[13.5px] font-semibold">{isNew ? `Add ${PRESET_LABEL[preset].replace(" preset", "")} provider` : `Edit ${initial.name}`}</span>
        {!isNew && <KeyBadge provider={initial} />}
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        <label className="block min-w-0 space-y-1">
          <span className={fieldLabel}>ID</span>
          <input
            value={id}
            onChange={(e) => setId(e.target.value.toLowerCase())}
            disabled={!isNew}
            placeholder="my-provider"
            spellCheck={false}
            aria-invalid={Boolean(idError)}
            className={cx(inputClass, "font-mono text-[13px] disabled:opacity-60")}
          />
          <span className={cx("block text-[12px]", idError && id ? "text-err" : "text-faint")}>
            {isNew ? (idError && id ? idError : `Models are picked as ${id || "id"}/model`) : "IDs can't be changed"}
          </span>
        </label>
        <label className="block min-w-0 space-y-1">
          <span className={fieldLabel}>Name</span>
          <input value={name} onChange={(e) => setName(e.target.value)} placeholder="My provider" className={inputClass} />
        </label>
        <label className="block min-w-0 space-y-1 sm:col-span-2">
          <span className={fieldLabel}>Base URL</span>
          <input
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder={wire === "openai" ? "https://api.openai.com" : "https://api.example.com/anthropic"}
            spellCheck={false}
            aria-invalid={Boolean(urlError && baseUrl)}
            className={cx(inputClass, "font-mono text-[13px]")}
          />
          {urlError && baseUrl ? (
            <span className="block text-[12px] text-err">{urlError}</span>
          ) : loopback ? (
            <span className="block text-[12px] text-faint">The Mothership connects to this address, so localhost is the Mothership itself.</span>
          ) : null}
        </label>
        <label className="block min-w-0 space-y-1">
          <span className={fieldLabel}>Authentication</span>
          <select value={auth} onChange={(e) => setAuth(e.target.value as ProviderAuth)} className={inputClass}>
            {(Object.keys(AUTH_LABEL) as ProviderAuth[]).map((mode) => (
              <option key={mode} value={mode}>
                {AUTH_LABEL[mode]}
              </option>
            ))}
          </select>
        </label>
        <div className="min-w-0 space-y-1">
          <span className={fieldLabel}>{auth === "bearer" ? "Token" : "API key"}</span>
          {auth === "none" ? (
            <p className="flex h-9 items-center text-[13px] text-faint">Not needed</p>
          ) : keyMode === "keep" ? (
            <div className="flex flex-wrap items-center gap-2">
              <span className="inline-flex h-9 items-center gap-1.5 text-[13px] text-ok">
                <IconCheck size={14} /> Saved
              </span>
              <Button size="sm" onClick={() => setKeyMode("replace")}>
                Replace
              </Button>
              <Button size="sm" variant="danger" onClick={() => setKeyMode("remove")}>
                Remove
              </Button>
            </div>
          ) : keyMode === "remove" ? (
            <div className="flex flex-wrap items-center gap-2">
              <span className="inline-flex h-9 items-center text-[13px] text-warn">Removed on save</span>
              <Button size="sm" variant="ghost" onClick={() => setKeyMode("keep")}>
                Undo
              </Button>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              <input
                type="password"
                autoComplete="off"
                value={key}
                onChange={(e) => setKey(e.target.value)}
                placeholder={initial?.has_key ? "New key" : "sk-…"}
                aria-label={`${name || "Provider"} API key`}
                className={cx(inputClass, "font-mono text-[13px]")}
              />
              {initial?.has_key && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    setKey("");
                    setKeyMode("keep");
                  }}
                >
                  Keep saved
                </Button>
              )}
            </div>
          )}
        </div>
        <div className="min-w-0 space-y-1 sm:col-span-2">
          <span className={fieldLabel}>Models</span>
          <ChipsInput values={models} onChange={setModels} placeholder={models.length ? "Add another" : "deepseek-flash, qwen3-coder, …"} label="Models" />
          <span className="block text-[12px] text-faint">Model IDs as the endpoint expects them. Press Enter or comma to add.</span>
        </div>
        <details
          open={advancedOpen}
          onToggle={(e) => setAdvancedOpen(e.currentTarget.open)}
          className="group min-w-0 rounded-lg border border-border sm:col-span-2"
        >
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-3 py-2 text-[13px] hover:bg-panel-2 [&::-webkit-details-marker]:hidden">
            <IconChevron size={14} className="shrink-0 text-muted transition-transform group-open:rotate-90" />
            <span className="font-medium">Advanced</span>
            <span className={cx("min-w-0 flex-1 truncate text-[12px]", limitsInvalid ? "text-err" : "text-faint")}>
              {limitsInvalid
                ? "Some values are out of range"
                : advancedSummary.length
                  ? advancedSummary.join(" · ")
                  : "Timeouts, concurrency, context window, fallback"}
            </span>
          </summary>
          <div className="grid gap-3 border-t border-border px-3 pb-3 pt-3 sm:grid-cols-2">
            <LimitField
              label="Request timeout (seconds)"
              value={limitDraft.timeout_secs}
              onChange={(v) => setLimit("timeout_secs", v)}
              placeholder={String(DEFAULT_TIMEOUT)}
              error={limits.timeout_secs.error}
              help={`How long the Mothership waits for a response. Blank uses ${DEFAULT_TIMEOUT}.`}
            />
            <LimitField
              label="Queue timeout (seconds)"
              value={limitDraft.queue_timeout_secs}
              onChange={(v) => setLimit("queue_timeout_secs", v)}
              placeholder="Same as request timeout"
              error={limits.queue_timeout_secs.error}
              help="How long a request may wait for a free slot."
            />
            <LimitField
              label="Max concurrent requests"
              value={limitDraft.max_concurrent}
              onChange={(v) => setLimit("max_concurrent", v)}
              placeholder="Unlimited"
              error={limits.max_concurrent.error}
              help="Requests beyond this wait in a queue on the Mothership; a local server usually handles 1-2."
            />
            <LimitField
              label="Context window (tokens)"
              value={limitDraft.context_tokens}
              onChange={(v) => setLimit("context_tokens", v)}
              placeholder="Claude default"
              error={limits.context_tokens.error}
              help="The model's context size, so agents compact before they hit it."
            />
            <label className="block min-w-0 space-y-1">
              <span className={fieldLabel}>Fallback model</span>
              <select value={fallback} onChange={(e) => setFallback(e.target.value)} className={inputClass}>
                <option value="">None</option>
                {fallback && !anthropicModels.some((m) => m.id === fallback) && <option value={fallback}>{fallback}</option>}
                {anthropicModels.map((model) => (
                  <option key={model.id} value={model.id}>
                    {model.label === model.id ? model.id : `${model.id} · ${model.label}`}
                  </option>
                ))}
              </select>
              <span className="block text-[12px] text-faint">Used when the provider is unreachable, times out or the queue is full.</span>
            </label>
          </div>
        </details>
      </div>
      <div className="flex flex-wrap items-center gap-2 border-t border-border pt-3">
        {!isNew && (
          <Button size="sm" variant="danger" className="mr-auto" disabled={busy !== null} onClick={remove}>
            {busy === "delete" && <Spinner />} Remove provider
          </Button>
        )}
        <Button size="sm" variant="ghost" className={isNew ? "ml-auto" : ""} onClick={onCancel} disabled={busy !== null}>
          Cancel
        </Button>
        <Button size="sm" type="submit" variant="primary" disabled={invalid || busy !== null}>
          {busy === "save" && <Spinner />} {isNew ? "Add provider" : "Save"}
        </Button>
      </div>
    </form>
  );
}

function LimitField({
  label,
  value,
  onChange,
  placeholder,
  error,
  help,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
  error: string | null;
  help: string;
}) {
  return (
    <label className="block min-w-0 space-y-1">
      <span className="text-[12.5px] font-medium text-muted">{label}</span>
      <input
        inputMode="numeric"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        aria-invalid={Boolean(error)}
        className={cx(inputClass, "font-mono text-[13px]", error && "border-err")}
      />
      <span className={cx("block text-[12px]", error ? "text-err" : "text-faint")}>{error ?? help}</span>
    </label>
  );
}

function ChipsInput({
  values,
  onChange,
  placeholder,
  label,
}: {
  values: string[];
  onChange: (values: string[]) => void;
  placeholder?: string;
  label: string;
}) {
  const [draft, setDraft] = useState("");
  const commit = (raw: string) => {
    const added = raw
      .split(/[,\s]+/)
      .map((v) => v.trim())
      .filter((v) => v && !values.includes(v));
    if (added.length) onChange([...values, ...new Set(added)]);
    setDraft("");
  };
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" || e.key === ",") {
      e.preventDefault();
      commit(draft);
    } else if (e.key === "Backspace" && !draft && values.length) {
      onChange(values.slice(0, -1));
    }
  };
  return (
    <div className="flex min-h-9 w-full min-w-0 flex-wrap items-center gap-1.5 rounded-lg border border-border bg-panel px-2 py-1.5 focus-within:border-accent focus-within:ring-2 focus-within:ring-[var(--accent-ring)]">
      {values.map((value) => (
        <span key={value} className="inline-flex max-w-full items-center gap-1 rounded-md bg-panel-2 py-0.5 pl-2 pr-1 font-mono text-[12px]">
          <span className="truncate">{value}</span>
          <button
            type="button"
            onClick={() => onChange(values.filter((v) => v !== value))}
            aria-label={`Remove ${value}`}
            className="grid size-4 cursor-pointer place-items-center rounded text-faint hover:bg-panel-3 hover:text-text"
          >
            <IconX size={11} />
          </button>
        </span>
      ))}
      <input
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
        onBlur={() => draft.trim() && commit(draft)}
        placeholder={placeholder}
        aria-label={label}
        spellCheck={false}
        className="min-w-24 flex-1 bg-transparent px-1 font-mono text-[13px] text-text outline-none placeholder:text-faint"
      />
    </div>
  );
}
