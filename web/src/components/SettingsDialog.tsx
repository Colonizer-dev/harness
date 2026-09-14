import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../context";
import type { HarnessStatus, LoginView, ModuleInfo, SchemaField } from "../types";
import { IconCheck, IconExternal, IconX } from "./icons";
import { Badge, Button, Spinner, Switch, cx, inputClass } from "./ui";

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
};

function ModulesTab({ onModulesChanged }: { onModulesChanged: (modules: ModuleInfo[]) => void }) {
  const api = useApi();
  const [modules, setModules] = useState<ModuleInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);

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

function ModuleCard({ module, onSaved }: { module: ModuleInfo; onSaved: (module: ModuleInfo) => void }) {
  const api = useApi();
  const toast = useToast();
  const [provider, setProvider] = useState(module.provider);
  const [enabled, setEnabled] = useState(module.enabled);
  const [settings, setSettings] = useState<Record<string, unknown>>(module.settings ?? {});
  const [saving, setSaving] = useState(false);
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
              <SettingField key={key} name={key} field={field} value={valueOf(settings, key, field)} onChange={(v) => setField(key, v)} />
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
}: {
  name: string;
  field: SchemaField;
  value: unknown;
  onChange: (value: unknown) => void;
}) {
  const label = field.title ?? name;
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
