import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type ComponentType,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
} from "react";
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
import { PROVIDER_CATALOG, type CatalogEntry } from "../providerCatalog";
import { useModels } from "../useModels";
import {
  BrandAlibabaCloud,
  BrandClaude,
  BrandDeepSeek,
  BrandGitHubCopilot,
  BrandKimi,
  BrandMiniMax,
  BrandModelScope,
  BrandNvidia,
  BrandOpenRouter,
  BrandXai,
  BrandXiaomi,
  IconCheck,
  IconChevron,
  IconExternal,
  IconNetwork,
  IconPencil,
  IconPlug,
  IconPlus,
  IconServer,
  IconX,
  type IconProps,
} from "./icons";
import { SkillsetField } from "./Skillsets";
import { Badge, Button, InfoButton, ModelInput, Spinner, Switch, cx, inputClass, useMediaQuery, type Tone } from "./ui";

// ---------------------------------------------------------------------------
// Shell: a section list on the left, the selected section on the right.
// Below 700px the list is the first screen and each section is a back-navigable page.
// ---------------------------------------------------------------------------

type SectionId = "connections" | "providers" | "runtime" | `module:${string}`;

const PANE_TITLE_ID = "settings-pane-title";

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

const kindInfo = (kind: string) => KIND_INFO[kind] ?? { title: kind, description: "" };

type ModuleDraft = { provider: string; enabled: boolean; settings: Record<string, unknown> };

const draftOf = (m: ModuleInfo): ModuleDraft => ({ provider: m.provider, enabled: m.enabled, settings: m.settings ?? {} });

const isDirty = (m: ModuleInfo, d: ModuleDraft | undefined) =>
  d !== undefined && (d.provider !== m.provider || d.enabled !== m.enabled || JSON.stringify(d.settings) !== JSON.stringify(m.settings ?? {}));

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
      className="m-auto w-[min(900px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50"
    >
      {open && <SettingsBody status={status} onStatusChanged={onStatusChanged} onModulesChanged={onModulesChanged} onClose={onClose} />}
    </dialog>
  );
}

function SettingsBody({
  status,
  onStatusChanged,
  onModulesChanged,
  onClose,
}: {
  status: HarnessStatus | null;
  onStatusChanged: () => void;
  onModulesChanged: (modules: ModuleInfo[]) => void;
  onClose: () => void;
}) {
  const api = useApi();
  const narrow = useMediaQuery("(max-width: 699px)");
  // A narrow window opens on the section list; a wide one on the first section.
  const [section, setSection] = useState<SectionId | null>(() => (narrow ? null : "connections"));
  // Where focus returns when a narrow window goes back to the section list.
  const lastSection = useRef<SectionId>("connections");
  const select = (id: SectionId) => {
    lastSection.current = id;
    setSection(id);
  };
  const [modules, setModules] = useState<ModuleInfo[] | null>(null);
  const [modulesError, setModulesError] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<Record<string, ModuleDraft>>({});
  const [providers, setProviders] = useState<ModelProvider[] | null>(null);
  const [providersError, setProvidersError] = useState<string | null>(null);
  const models = useModels();

  useEffect(() => {
    let cancelled = false;
    api
      .modules()
      .then((list) => {
        if (cancelled) return;
        setModules(list);
        setDrafts(Object.fromEntries(list.map((m) => [m.kind, draftOf(m)])));
      })
      .catch((e) => !cancelled && setModulesError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api]);

  const loadProviders = useCallback(async () => {
    try {
      setProviders(await api.providers());
      setProvidersError(null);
    } catch (e) {
      setProvidersError(errorMessage(e));
    }
  }, [api]);

  useEffect(() => {
    void loadProviders();
  }, [loadProviders]);

  const active: SectionId | null = narrow ? section : (section ?? "connections");

  const github = status?.github;
  const claude = status?.claude;
  const connectionsTone: Tone | null = !status ? null : github?.connected && claude?.configured ? "ok" : "err";
  const runtimeBroken = Boolean(status && (!status.sandbox.msb_version || status.sandbox.claude_bin_error || status.mesh?.error));

  const groups: NavGroup[] = [
    {
      label: "General",
      items: [
        {
          id: "connections",
          label: "Connections",
          hint: "GitHub and Claude",
          tone: connectionsTone,
          toneText: connectionsTone === "ok" ? "All connected" : connectionsTone === "err" ? "Needs setup" : undefined,
        },
        {
          id: "providers",
          label: "Model providers",
          hint: "Claude, and other Anthropic-compatible endpoints",
          // Anthropic is always in the list as a built-in row, so the count follows what is on screen.
          badge: providers ? String(providers.length + 1) : undefined,
        },
        { id: "runtime", label: "Runtime", hint: "Detected on this machine", tone: runtimeBroken ? "err" : null, toneText: runtimeBroken ? "Something is missing" : undefined },
      ],
    },
    {
      label: "Modules",
      loading: !modules && !modulesError,
      error: modulesError,
      items: (modules ?? []).map((m) => ({
        id: `module:${m.kind}` as const,
        label: kindInfo(m.kind).title,
        hint: kindInfo(m.kind).description,
        badge: m.enabled ? undefined : "Off",
        dirty: isDirty(m, drafts[m.kind]),
      })),
    },
  ];

  const back = narrow ? () => setSection(null) : undefined;

  let pane: ReactNode = null;
  if (active === "connections") pane = <ConnectionsPane status={status} onStatusChanged={onStatusChanged} back={back} />;
  else if (active === "runtime") pane = <RuntimePane status={status} back={back} />;
  else if (active === "providers") {
    pane = (
      <ProvidersPane
        providers={providers}
        error={providersError}
        setProviders={setProviders}
        reload={loadProviders}
        claude={claude ?? null}
        models={models}
        onOpenConnections={() => select("connections")}
        back={back}
      />
    );
  } else if (active?.startsWith("module:")) {
    const kind = active.slice("module:".length);
    const module = modules?.find((m) => m.kind === kind);
    const draft = drafts[kind];
    pane =
      module && draft ? (
        <ModulePane
          key={kind}
          module={module}
          draft={draft}
          models={kind === "agent" ? models : undefined}
          back={back}
          onDraft={(patch) => setDrafts((d) => ({ ...d, [kind]: { ...d[kind], ...patch } }))}
          onReset={() => setDrafts((d) => ({ ...d, [kind]: draftOf(module) }))}
          onSaved={(saved) => {
            const next = (modules ?? []).map((m) => (m.kind === saved.kind ? saved : m));
            setModules(next);
            setDrafts((d) => ({ ...d, [kind]: draftOf(saved) }));
            onModulesChanged(next);
          }}
        />
      ) : (
        <Pane title={kindInfo(kind).title} back={back}>
          <p className="flex items-center gap-2 text-[13px] text-muted">
            <Spinner /> Loading…
          </p>
        </Pane>
      );
  }

  return (
    <div className="flex h-[min(680px,calc(100dvh-24px))] flex-col">
      <div className="flex shrink-0 items-center gap-3 border-b border-border px-5 py-3">
        <h2 id="settings-title" className="min-w-0 flex-1 text-[16px] font-semibold">
          Settings
        </h2>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close settings"
          className="grid size-8 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
        >
          <IconX size={17} />
        </button>
      </div>
      {narrow ? (
        active === null ? (
          <SectionNav layout="list" groups={groups} active={null} onSelect={select} initialFocus={lastSection.current} />
        ) : (
          pane
        )
      ) : (
        <div className="flex min-h-0 flex-1">
          <SectionNav layout="side" groups={groups} active={active} onSelect={select} />
          {pane}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Section list
// ---------------------------------------------------------------------------

type NavItem = {
  id: SectionId;
  label: string;
  hint?: string;
  /** A coloured dot; `toneText` is what a screen reader hears for it. */
  tone?: Tone | null;
  toneText?: string;
  badge?: string;
  dirty?: boolean;
};

type NavGroup = { label: string; items: NavItem[]; loading?: boolean; error?: string | null };

function SectionNav({
  layout,
  groups,
  active,
  onSelect,
  initialFocus,
}: {
  layout: "side" | "list";
  groups: NavGroup[];
  active: SectionId | null;
  onSelect: (id: SectionId) => void;
  /** Focus this item when the list appears (the section a narrow window just left). */
  initialFocus?: SectionId;
}) {
  const refs = useRef<Partial<Record<SectionId, HTMLButtonElement | null>>>({});
  const flat = groups.flatMap((g) => g.items);
  const side = layout === "side";

  useEffect(() => {
    if (initialFocus) refs.current[initialFocus]?.focus();
  }, [initialFocus]);

  // In the side layout the list is one Tab stop: arrows move between sections.
  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (!side || !["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
    const index = flat.findIndex((item) => item.id === active);
    let next = index;
    if (e.key === "ArrowDown") next = (index + 1) % flat.length;
    else if (e.key === "ArrowUp") next = (index - 1 + flat.length) % flat.length;
    else if (e.key === "Home") next = 0;
    else next = flat.length - 1;
    const target = flat[next];
    if (!target) return;
    e.preventDefault();
    onSelect(target.id);
    refs.current[target.id]?.focus();
  };

  return (
    <nav
      aria-label="Settings sections"
      onKeyDown={onKeyDown}
      className={cx(
        "scroll-thin min-h-0 overflow-y-auto",
        side ? "w-[200px] shrink-0 space-y-4 border-r border-border px-2.5 py-3" : "flex-1 space-y-4 px-5 py-3",
      )}
    >
      {groups.map((group) => (
        <div key={group.label}>
          <h3 className={cx("text-[11.5px] font-semibold uppercase tracking-wide text-faint", side ? "mb-1 px-2.5" : "mb-1")}>{group.label}</h3>
          {group.loading && (
            <p className={cx("flex items-center gap-2 py-1.5 text-[12.5px] text-muted", side && "px-2.5")}>
              <Spinner className="size-3" /> Loading…
            </p>
          )}
          {group.error && <p className={cx("py-1.5 text-[12.5px] text-err", side && "px-2.5")}>{group.error}</p>}
          <ul className={side ? "space-y-0.5" : "divide-y divide-border overflow-hidden rounded-xl border border-border"}>
            {group.items.map((item) => {
              const current = item.id === active;
              return (
                <li key={item.id}>
                  <button
                    type="button"
                    ref={(el) => {
                      refs.current[item.id] = el;
                    }}
                    aria-current={current ? "true" : undefined}
                    tabIndex={side ? (current ? 0 : -1) : 0}
                    onClick={() => onSelect(item.id)}
                    className={cx(
                      "flex w-full cursor-pointer items-center gap-2 text-left",
                      side
                        ? cx("rounded-lg px-2.5 py-1.5 text-[13px]", current ? "bg-panel-2 font-medium text-text" : "text-muted hover:bg-panel-2 hover:text-text")
                        : "px-3.5 py-3 text-[13.5px] hover:bg-panel-2",
                    )}
                  >
                    <span className="min-w-0 flex-1">
                      <span className="block truncate">{item.label}</span>
                      {!side && item.hint && <span className="block truncate text-[12px] text-muted">{item.hint}</span>}
                    </span>
                    {item.dirty && (
                      <>
                        <span aria-hidden="true" className="size-1.5 shrink-0 rounded-full bg-accent" />
                        <span className="sr-only">unsaved changes</span>
                      </>
                    )}
                    {item.badge && <span className="shrink-0 text-[11.5px] text-faint">{item.badge}</span>}
                    {item.tone && (
                      <>
                        <span
                          aria-hidden="true"
                          className={cx("size-2 shrink-0 rounded-full", item.tone === "ok" ? "bg-ok" : item.tone === "err" ? "bg-err" : "bg-warn")}
                        />
                        {item.toneText && <span className="sr-only">{item.toneText}</span>}
                      </>
                    )}
                    {!side && <IconChevron size={14} className="shrink-0 text-faint" />}
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </nav>
  );
}

// ---------------------------------------------------------------------------
// Pane and row layout shared by every section
// ---------------------------------------------------------------------------

function Pane({
  title,
  subtitle,
  info,
  aside,
  back,
  footer,
  children,
}: {
  title: string;
  subtitle?: string;
  /** Longer explanation, behind the "i" next to the title. */
  info?: ReactNode;
  aside?: ReactNode;
  back?: () => void;
  footer?: ReactNode;
  children: ReactNode;
}) {
  const titleRef = useRef<HTMLHeadingElement>(null);
  const stacked = Boolean(back);
  // In the narrow, back-navigable layout the pane replaces the list, so focus must follow.
  useEffect(() => {
    if (stacked) titleRef.current?.focus();
  }, [stacked]);

  return (
    <section aria-labelledby={PANE_TITLE_ID} className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex shrink-0 items-start gap-2 border-b border-border px-5 py-3.5">
        {back && (
          <button
            type="button"
            onClick={back}
            aria-label="Back to all settings"
            className="-ml-1.5 grid size-8 shrink-0 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
          >
            <IconChevron size={16} className="rotate-180" />
          </button>
        )}
        <div className="min-w-0 flex-1">
          <h3
            id={PANE_TITLE_ID}
            ref={titleRef}
            tabIndex={stacked ? -1 : undefined}
            className="flex items-center gap-1 rounded text-[15px] font-semibold leading-8"
          >
            {title}
            {info && <InfoButton label={title}>{info}</InfoButton>}
          </h3>
          {subtitle && <p className="-mt-1 text-[12.5px] text-muted">{subtitle}</p>}
        </div>
        {aside && <div className="flex shrink-0 items-center leading-8">{aside}</div>}
      </div>
      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">{children}</div>
      {footer && <div className="flex shrink-0 flex-wrap items-center gap-2 border-t border-border px-5 py-3">{footer}</div>}
    </section>
  );
}

/** One setting: label on the left, control on the right, explanation behind the "i". The label's id is `${id}-label`. */
function Row({
  id,
  label,
  info,
  inline,
  children,
}: {
  id?: string;
  label: string;
  info?: ReactNode;
  /** For switches: keeps the control on the label's line at every width. */
  inline?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 py-3">
      <div className="min-w-0 flex-1 basis-40">
        <div className="flex items-center gap-1">
          <label id={id ? `${id}-label` : undefined} htmlFor={id} className="text-[13.5px] font-medium">
            {label}
          </label>
          {info && <InfoButton label={label}>{info}</InfoButton>}
        </div>
      </div>
      <div className={cx("min-w-0", inline ? "flex shrink-0 justify-end sm:w-[260px]" : "w-full sm:w-[260px]")}>{children}</div>
    </div>
  );
}

function Code({ children }: { children: ReactNode }) {
  return <code className="rounded bg-panel-3 px-1 font-mono text-[11.5px]">{children}</code>;
}

// ---------------------------------------------------------------------------
// Connections: GitHub and Claude
// ---------------------------------------------------------------------------

const IDLE_LOGIN: LoginView = { state: "idle", url: null, message: null };

function ConnectionCard({
  name,
  connected,
  detail,
  detailTone,
  info,
  children,
}: {
  name: string;
  connected: boolean | null;
  detail?: string;
  detailTone?: "err";
  info: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="rounded-xl border border-border">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 px-4 py-3">
        <span className="flex items-center gap-1 text-[14px] font-semibold">
          {name}
          <InfoButton label={name}>{info}</InfoButton>
        </span>
        {connected != null && <Badge tone={connected ? "ok" : "err"}>{connected ? "Connected" : "Not connected"}</Badge>}
        {detail && <span className={cx("text-[12.5px] [overflow-wrap:anywhere]", detailTone === "err" ? "text-err" : "text-muted")}>{detail}</span>}
      </div>
      <div className="space-y-3 border-t border-border px-4 py-3">{children}</div>
    </div>
  );
}

function ConnectionsPane({ status, onStatusChanged, back }: { status: HarnessStatus | null; onStatusChanged: () => void; back?: () => void }) {
  const api = useApi();
  const toast = useToast();
  const [githubToken, setGithubToken] = useState("");
  const [claudeToken, setClaudeToken] = useState("");
  const [saving, setSaving] = useState<"github" | "claude" | null>(null);
  const [login, setLogin] = useState<LoginView>(IDLE_LOGIN);
  const [code, setCode] = useState("");
  const githubTokenId = useId();
  const claudeTokenId = useId();
  const codeId = useId();
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
  const githubViaToken = github?.source === "saved token";

  const githubForm = (
    <form
      className="flex flex-wrap gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        void run("github", () => api.setGithubToken(githubToken.trim()), "GitHub token saved").then(() => setGithubToken(""));
      }}
    >
      <label htmlFor={githubTokenId} className="sr-only">
        GitHub token
      </label>
      <input
        id={githubTokenId}
        type="password"
        autoComplete="off"
        value={githubToken}
        onChange={(e) => setGithubToken(e.target.value)}
        placeholder="github_pat_… or ghp_…"
        className={cx(inputClass, "min-w-48 flex-1")}
      />
      <Button type="submit" variant="primary" disabled={!githubToken.trim() || saving !== null}>
        {saving === "github" && <Spinner />} Save
      </Button>
      <Button disabled={saving !== null} onClick={() => run("github", () => api.deleteGithubToken(), "Saved GitHub token removed")}>
        Remove saved token
      </Button>
    </form>
  );

  return (
    <Pane title="Connections" subtitle="Both are needed before the first colony" back={back}>
      <div className="space-y-4">
        <ConnectionCard
          name="GitHub"
          connected={github ? github.connected : null}
          detail={github?.connected ? `@${github.login} · ${github.source}` : github?.error?.split("\n")[0]}
          detailTone={github && !github.connected ? "err" : undefined}
          info={
            <>
              <p>
                Uses your <Code>gh auth login</Code> session when there is one, or a token you save here.
              </p>
              <p className="text-muted">A fine-grained token needs Contents, Issues and Pull requests, read and write.</p>
            </>
          }
        >
          {github?.connected ? (
            <details className="text-[13px]">
              <summary className="cursor-pointer text-muted hover:text-text">{githubViaToken ? "Replace or remove the token" : "Use a token instead"}</summary>
              <div className="mt-2">{githubForm}</div>
            </details>
          ) : (
            githubForm
          )}
        </ConnectionCard>

        <ConnectionCard
          name="Claude"
          connected={claude ? claude.configured : null}
          detail={claude?.configured ? claude.source ?? undefined : undefined}
          info={
            <>
              <p>
                Log in runs <Code>claude setup-token</Code> on the Mothership (this machine) and saves a 1-year token here.
              </p>
              <p className="text-muted">microVMs only ever see a placeholder; the real token is swapped in for requests to api.anthropic.com.</p>
            </>
          }
        >
          <div className="flex flex-wrap gap-2">
            <Button variant="primary" onClick={startLogin} disabled={flowActive}>
              {login.state === "starting" && <Spinner />} Log in with Claude subscription
            </Button>
            <Button onClick={() => run("claude", () => api.deleteClaudeToken(), "Saved Claude token removed")} disabled={saving !== null}>
              Remove saved token
            </Button>
          </div>

          {login.state !== "idle" && (
            <div role="status" className="space-y-3 rounded-lg border border-dashed border-border-strong p-3.5">
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
                        <a
                          href={login.url}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="inline-flex items-center gap-1 font-medium text-accent hover:underline"
                        >
                          Open the Claude sign-in page <IconExternal size={12} />
                        </a>
                      ) : (
                        "Waiting for the sign-in link…"
                      )}{" "}
                      and approve access.
                    </li>
                    <li>
                      <span className="mr-1 font-semibold">2.</span>Paste the code it shows.
                    </li>
                  </ol>
                  <form onSubmit={submitCode} className="flex flex-wrap gap-2">
                    <label htmlFor={codeId} className="sr-only">
                      Sign-in code
                    </label>
                    <input
                      id={codeId}
                      value={code}
                      onChange={(e) => setCode(e.target.value)}
                      autoComplete="off"
                      placeholder="Sign-in code"
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

          <details className="text-[13px]">
            <summary className="cursor-pointer text-muted hover:text-text">Use a token or API key instead</summary>
            <form
              className="mt-2 flex flex-wrap gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                void run("claude", () => api.setClaudeToken(claudeToken.trim()), "Claude token saved").then(() => setClaudeToken(""));
              }}
            >
              <label htmlFor={claudeTokenId} className="sr-only">
                Claude token
              </label>
              <input
                id={claudeTokenId}
                type="password"
                autoComplete="off"
                value={claudeToken}
                onChange={(e) => setClaudeToken(e.target.value)}
                placeholder="sk-ant-oat01-… or sk-ant-api…"
                className={cx(inputClass, "min-w-48 flex-1")}
              />
              <Button type="submit" disabled={!claudeToken.trim() || saving !== null}>
                Save
              </Button>
            </form>
          </details>
        </ConnectionCard>
      </div>
    </Pane>
  );
}

// ---------------------------------------------------------------------------
// Runtime: what the Mothership found on this machine
// ---------------------------------------------------------------------------

function RuntimePane({ status, back }: { status: HarnessStatus | null; back?: () => void }) {
  const rows: { label: string; value: ReactNode; bad?: boolean; mono?: boolean }[] = status
    ? [
        { label: "microsandbox", value: status.sandbox.msb_version ?? "not found", bad: !status.sandbox.msb_version },
        { label: "Image", value: status.sandbox.image, mono: true },
        {
          label: "Agent binary",
          value: status.sandbox.claude_bin ?? status.sandbox.claude_bin_error ?? "—",
          bad: Boolean(status.sandbox.claude_bin_error),
          mono: true,
        },
        {
          label: "Mesh",
          value: status.mesh
            ? status.mesh.enabled
              ? [status.mesh.provider, status.mesh.state, status.mesh.harness_ip, status.mesh.error].filter(Boolean).join(" · ")
              : "disabled"
            : "—",
          bad: Boolean(status.mesh?.error),
        },
      ]
    : [];

  return (
    <Pane title="Runtime" subtitle="Detected on this machine" back={back}>
      {status ? (
        <dl className="divide-y divide-border">
          {rows.map((row) => (
            <div key={row.label} className="flex flex-wrap items-baseline gap-x-4 gap-y-1 py-2.5">
              <dt className="flex w-32 shrink-0 items-center gap-1.5 text-[13px] text-muted">
                {row.bad && <span aria-hidden="true" className="size-2 rounded-full bg-err" />}
                {row.label}
              </dt>
              <dd className={cx("min-w-0 flex-1 text-[13px] [overflow-wrap:anywhere]", row.mono && "font-mono text-[12.5px]", row.bad && "text-err")}>
                {row.value}
              </dd>
            </div>
          ))}
        </dl>
      ) : (
        <p className="text-[13px] text-muted">Mothership status unavailable.</p>
      )}
    </Pane>
  );
}

// ---------------------------------------------------------------------------
// Modules: one pane per kind, form generated from the provider's JSON Schema
// ---------------------------------------------------------------------------

const MODEL_KEYS = new Set(["model", "subagent_model", "background_model"]);

function valueOf(settings: Record<string, unknown>, key: string, field: SchemaField): unknown {
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
    <div className={cx("flex flex-wrap items-center gap-2 rounded-xl border border-border px-3.5 py-2.5 text-[12.5px]", tone)}>
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
      <div className="rounded-xl border border-border px-3.5 py-2.5 text-[12.5px] text-muted">
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

function ModulePane({
  module,
  draft,
  models,
  back,
  onDraft,
  onReset,
  onSaved,
}: {
  module: ModuleInfo;
  draft: ModuleDraft;
  models?: ModelOption[];
  back?: () => void;
  onDraft: (patch: Partial<ModuleDraft>) => void;
  onReset: () => void;
  onSaved: (module: ModuleInfo) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState(false);
  const providerId = useId();
  const pull = useImagePull(module.kind === "sandbox");
  const info = kindInfo(module.kind);
  const fields = Object.entries(module.schema?.properties ?? {});
  const dirty = isDirty(module, draft);
  const providerInfo = module.providers.find((p) => p.id === draft.provider);

  const save = async () => {
    setSaving(true);
    try {
      const saved = await api.saveModule(module.kind, { provider: draft.provider, enabled: draft.enabled, settings: draft.settings });
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

  const setField = (key: string, value: unknown) => onDraft({ settings: { ...draft.settings, [key]: value } });

  return (
    <Pane
      title={info.title}
      subtitle={info.description}
      back={back}
      aside={
        <span className="flex items-center gap-2 text-[12.5px] text-muted">
          <span aria-hidden="true">{draft.enabled ? "On" : "Off"}</span>
          <Switch checked={draft.enabled} onChange={(enabled) => onDraft({ enabled })} label={`${info.title} module enabled`} />
        </span>
      }
      footer={
        <>
          <span className="mr-auto text-[12.5px] text-muted">{dirty ? "Unsaved changes" : "Changes apply to new colonies"}</span>
          {dirty && (
            <Button variant="ghost" onClick={onReset}>
              Reset
            </Button>
          )}
          <Button variant="primary" disabled={!dirty || saving} onClick={save}>
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
      <div className={cx("divide-y divide-border", !draft.enabled && "opacity-60")}>
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
            <span id={providerId} className="block text-[13.5px]">
              {providerInfo?.name ?? draft.provider}
            </span>
          )}
        </Row>

        {draft.provider !== module.provider && fields.length > 0 && (
          <p className="py-2.5 text-[12.5px] text-warn">These fields belong to the current provider. Save to switch.</p>
        )}

        {fields.map(([key, field]) => (
          <SettingField
            key={key}
            name={key}
            field={field}
            value={valueOf(draft.settings, key, field)}
            onChange={(v) => setField(key, v)}
            models={models && MODEL_KEYS.has(key) ? models : undefined}
          />
        ))}

        {fields.length === 0 && module.providers.length <= 1 && <p className="py-3 text-[13px] text-faint">Nothing to configure.</p>}
      </div>
    </Pane>
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
  const id = useId();
  const label = field.title ?? name;
  if (field.format === "plugin-dirs") {
    return <SkillsetField label={label} description={field.description} value={value} onChange={onChange} />;
  }
  const info = fieldInfo(field);
  const text = value === undefined || value === null ? "" : String(value);

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
        <ModelInput
          id={id}
          value={text}
          onChange={onChange}
          models={models}
          placeholder={name === "subagent_model" ? "Same as orchestrator" : "Claude Code default"}
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
  // Z.AI's coding plan speaks the Anthropic protocol; its key goes in an Authorization header.
  zai: {
    id: "zai",
    name: "Z.AI",
    base_url: "https://api.z.ai/api/anthropic",
    auth: "bearer",
    wire: "anthropic",
    models: ["glm-5.3", "glm-5.3-flash"],
    ...DEFAULT_LIMITS,
  },
  // Alibaba bills coding plans and token plans through different hosts; this is the token plan's.
  alibaba: {
    id: "alibaba",
    name: "Alibaba (Qwen)",
    base_url: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic",
    auth: "bearer",
    wire: "anthropic",
    models: ["qwen3.7-plus", "qwen3.8-flash"],
    ...DEFAULT_LIMITS,
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

const PRESET_LABEL: Record<ProviderPreset, string> = {
  deepseek: "DeepSeek",
  openai: "OpenAI",
  zai: "Z.AI",
  alibaba: "Alibaba",
  local: "Local",
  custom: "Custom",
};

/**
 * What sits in a provider's tile. Vendors with a CC0 mark in the icon set get it;
 * `local` and `custom` are not brands and keep a plain glyph. Anything else,
 * including OpenAI and Z.AI (no CC0 artwork exists for them) and every preset
 * added later, falls through to a lettermark built from the provider's name.
 */
const PRESET_MARK: Partial<Record<ProviderPreset | "anthropic", ComponentType<IconProps>>> = {
  anthropic: BrandClaude,
  deepseek: BrandDeepSeek,
  alibaba: BrandAlibabaCloud,
  local: IconServer,
  custom: IconPlug,
  // Catalogue vendors whose mark exists under CC0; the rest fall back to initials.
  kimi: BrandKimi,
  "kimi-for-coding": BrandKimi,
  minimax: BrandMiniMax,
  modelscope: BrandModelScope,
  openrouter: BrandOpenRouter,
  "github-copilot": BrandGitHubCopilot,
  "xai-grok": BrandXai,
  nvidia: BrandNvidia,
  xiaomi: BrandXiaomi,
};

/** One or two initials: the capitals of the name ("OpenAI" gives OA, "Z.AI" gives ZA), else the first letters of its words. */
function initialsOf(name: string): string {
  const capitals = name.replace(/[^A-Z]/g, "");
  if (capitals.length >= 2) return capitals.slice(0, 2);
  const words = name.split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  const letters = words.map((w) => w[0]).join("").slice(0, 2).toUpperCase();
  return letters || capitals || "?";
}

/**
 * The tile at the start of a provider row or add button. Decorative: the vendor's
 * name is always beside it as text, so the tile is hidden from assistive tech.
 */
function ProviderMark({ preset, name, size = "row" }: { preset?: ProviderPreset | "anthropic"; name: string; size?: "row" | "button" | "tile" }) {
  const Mark = preset ? PRESET_MARK[preset] : undefined;
  const box = { tile: "size-11 rounded-xl", row: "size-8 rounded-lg", button: "size-[18px] rounded-[5px]" }[size];
  const glyph = { tile: 24, row: 18, button: 12 }[size];
  // Initials carry the whole tile when a vendor has no mark, so they scale with it.
  const initials = { tile: "text-[15px] font-semibold tracking-tight", row: "text-[11.5px] font-semibold tracking-tight", button: "text-[8.5px] font-bold" }[size];
  return (
    <span aria-hidden="true" className={cx("grid shrink-0 select-none place-items-center bg-panel-2 text-text", box, !Mark && initials)}>
      {Mark ? <Mark size={glyph} strokeWidth={size === "button" ? 2 : 1.75} /> : initialsOf(name)}
    </span>
  );
}

/** Shown while adding a provider, where the base URL is the thing people get wrong. */
const PRESET_HINT: Partial<Record<ProviderPreset, string>> = {
  zai: "Uses your Z.AI coding plan key as a bearer token.",
  alibaba: "This is the token plan's host. A coding plan key needs coding-intl.dashscope.aliyuncs.com instead — the two are not interchangeable.",
};

const WIRE_LABEL: Record<ProviderWire, string> = { anthropic: "Anthropic API", openai: "OpenAI API" };

const AUTH_LABEL: Record<ProviderAuth, string> = {
  "x-api-key": "API key (x-api-key header)",
  bearer: "Bearer token (Authorization header)",
  none: "No authentication",
};

type Editing = { mode: "new"; preset: ProviderPreset } | { mode: "edit"; id: string } | null;

/** Add buttons, in the order the presets are listed. */
const CATALOG_BY_ID = new Map(PROVIDER_CATALOG.map((entry) => [entry.id, entry]));

/** The starting values for a new provider: a built-in preset, a catalogue entry, or bare Custom. */
function presetDraft(preset: ProviderPreset): ProviderDraft {
  const built = PRESETS[preset];
  if (built) return built;
  const entry = CATALOG_BY_ID.get(preset);
  if (!entry) return PRESETS.custom;
  return { ...PRESETS.custom, id: entry.id, name: entry.name, base_url: entry.base_url, auth: entry.auth, wire: entry.wire };
}

/** A catalogue entry's label, for the form header and the mark's fallback initials. */
function presetLabel(preset: ProviderPreset): string {
  return PRESET_LABEL[preset] ?? CATALOG_BY_ID.get(preset)?.name ?? "Provider";
}

const ADD_PRESETS: { preset: ProviderPreset; label: string }[] = [
  { preset: "deepseek", label: "DeepSeek" },
  { preset: "openai", label: "OpenAI" },
  { preset: "zai", label: "Z.AI" },
  { preset: "alibaba", label: "Alibaba" },
  { preset: "local", label: "Local server" },
  { preset: "custom", label: "Custom" },
];

function ProvidersPane({
  providers,
  error,
  setProviders,
  reload,
  claude,
  models,
  onOpenConnections,
  back,
}: {
  providers: ModelProvider[] | null;
  error: string | null;
  setProviders: (update: (list: ModelProvider[] | null) => ModelProvider[] | null) => void;
  reload: () => Promise<void>;
  claude: HarnessStatus["claude"] | null;
  models: ModelOption[];
  onOpenConnections: () => void;
  back?: () => void;
}) {
  const api = useApi();
  const addLabelId = useId();
  const [editing, setEditing] = useState<Editing>(null);
  const [browsing, setBrowsing] = useState(false);
  const [catalogQuery, setCatalogQuery] = useState("");
  const [health, setHealth] = useState<Record<string, HealthView>>({});

  // In-flight and queued counts change as colonies work; refresh them quietly.
  useEffect(() => {
    const timer = setInterval(() => {
      if (document.hidden) return;
      void reload();
    }, 5000);
    return () => clearInterval(timer);
  }, [reload]);

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

  const addDisabled = !providers || editing !== null;

  return (
    <Pane
      title="Model providers"
      subtitle="Claude, and other Anthropic-compatible endpoints"
      back={back}
      info={
        <>
          <p>
            Claude is the default: a model picked by its plain id, such as <Code>sonnet</Code>, goes to Anthropic. Pick another provider's
            model as <Code>provider/model</Code>, for example <Code>deepseek/deepseek-flash</Code>, wherever you choose an orchestrator,
            subagent or background model.
          </p>
          <p className="text-muted">
            Requests go through the Mothership gateway: colonies never see provider keys, and servers on a private network (LAN, tailnet)
            work without extra setup.
          </p>
        </>
      }
    >
      <div className="space-y-3">
        <div className="space-y-2">
          <ClaudeRow claude={claude} models={models} onOpenConnections={onOpenConnections} />
          {error && <p className="text-[13px] text-err">{error}</p>}
          {!providers && !error && (
            <p className="flex items-center gap-2 py-1 text-[13px] text-muted">
              <Spinner /> Loading providers…
            </p>
          )}
        </div>
        {providers && (
          <div className="space-y-2">
            {providers.length === 0 && editing?.mode !== "new" && (
              <p className="rounded-xl border border-dashed border-border-strong px-3.5 py-4 text-center text-[13px] text-muted">
                No extra providers yet. Claude works without one.
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
        <div role="group" aria-labelledby={addLabelId} className="space-y-2">
          <span id={addLabelId} className="block text-[12.5px] text-muted">
            Add a provider
          </span>
          <div className="grid grid-cols-3 gap-2 sm:grid-cols-6">
            {ADD_PRESETS.map(({ preset, label }) => {
              const configured = preset !== "custom" && has(preset);
              return (
                <button
                  key={preset}
                  type="button"
                  disabled={addDisabled || configured}
                  title={configured ? `${label} is already configured` : undefined}
                  onClick={() => setEditing({ mode: "new", preset })}
                  className={cx(
                    "flex cursor-pointer select-none flex-col items-center gap-2 rounded-xl border border-border bg-panel px-1.5 py-3",
                    "text-[12px] font-medium text-text transition-colors hover:bg-panel-2",
                    "disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:bg-panel",
                  )}
                >
                  <ProviderMark preset={preset} name={presetLabel(preset)} size="tile" />
                  <span className="w-full truncate text-center">{label}</span>
                </button>
              );
            })}
            <button
              type="button"
              disabled={addDisabled}
              aria-expanded={browsing}
              onClick={() => setBrowsing((open) => !open)}
              className={cx(
                "flex cursor-pointer select-none flex-col items-center gap-2 rounded-xl border border-dashed border-border-strong bg-panel px-1.5 py-3",
                "text-[12px] font-medium text-muted transition-colors hover:bg-panel-2 hover:text-text",
                "disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:bg-panel",
              )}
            >
              <span aria-hidden="true" className="grid size-11 shrink-0 place-items-center rounded-xl bg-panel-2">
                <IconPlus size={22} />
              </span>
              <span className="w-full truncate text-center">{browsing ? "Close" : "More"}</span>
            </button>
          </div>
          {browsing && <CatalogBrowser query={catalogQuery} onQuery={setCatalogQuery} taken={has} disabled={addDisabled} onPick={(id) => {
            setBrowsing(false);
            setCatalogQuery("");
            setEditing({ mode: "new", preset: id });
          }} />}
        </div>
        <p className="text-[11.5px] leading-snug text-faint">
          Logos and names are the property of their owners. Colonizer is not affiliated with, endorsed by or connected to any of them.
        </p>
      </div>
    </Pane>
  );
}

/**
 * Claude, at the top of the list. Not a provider anyone configured here: it is the
 * default the harness falls back to, so it is read-only, has no key field and no
 * endpoint to probe. Its state is the Connections card's, its models the ones
 * `/api/models` lists under `anthropic`.
 */
function ClaudeRow({ claude, models, onOpenConnections }: { claude: HarnessStatus["claude"] | null; models: ModelOption[]; onOpenConnections: () => void }) {
  const own = models.filter((m) => m.provider === "anthropic");
  return (
    <div className="flex flex-wrap items-start gap-x-3 gap-y-2 rounded-xl border border-border px-3.5 py-3">
      <ProviderMark preset="anthropic" name="Anthropic" />
      <div className="min-w-0 flex-1 basis-48">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-[14px] font-semibold">Anthropic</span>
          <Badge>Built-in</Badge>
          {claude && <Badge tone={claude.configured ? "ok" : "err"}>{claude.configured ? "Connected" : "Not connected"}</Badge>}
        </div>
        <div className="mt-0.5 text-[12px] text-muted">
          {claude?.configured ? `${claude.source ?? "Connected"} · managed in Connections` : "Managed in Connections"}
        </div>
        <div className="mt-1.5 flex flex-wrap items-center gap-1">
          {own.length === 0 ? (
            <span className="text-[12px] text-faint">Model list not loaded.</span>
          ) : (
            own.map((model) => (
              <span key={model.id} title={model.label} className="rounded bg-panel-2 px-1.5 py-px font-mono text-[11.5px] text-muted">
                {model.id}
              </span>
            ))
          )}
        </div>
        <p className="mt-1.5 text-[12px] text-faint">The default. A model id with no provider prefix, and a provider's fallback, go here.</p>
      </div>
      <div className="flex shrink-0 gap-1.5">
        <Button size="sm" onClick={onOpenConnections}>
          Connections <IconChevron size={13} />
        </Button>
      </div>
    </div>
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
      <ProviderMark preset={provider.preset} name={provider.name} />
      <div className="min-w-0 flex-1 basis-48">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-[14px] font-semibold">{provider.name}</span>
          <span className="font-mono text-[12px] text-faint">{provider.id}</span>
          <KeyBadge provider={provider} />
          {provider.wire === "openai" && (
            <Badge tone="info" title="Speaks the OpenAI protocol; the Mothership gateway translates">
              {WIRE_LABEL.openai}
            </Badge>
          )}
          {(running > 0 || queued > 0) && (
            <Badge tone={queued > 0 ? "warn" : "info"} pulse={running > 0}>
              {[running > 0 && `${running} running`, queued > 0 && `${queued} queued`].filter(Boolean).join(" · ")}
            </Badge>
          )}
        </div>
        <div className="mt-0.5 font-mono text-[12px] text-muted [overflow-wrap:anywhere]">{provider.base_url}</div>
        <div className="mt-1.5 flex flex-wrap items-center gap-1">
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
        {limits.length > 0 && <div className="mt-1 text-[12px] text-faint">{limits.join(" · ")}</div>}
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

/** A labelled field in the provider form: label and "i" above, control below, error under. */
function FormField({
  id,
  label,
  info,
  error,
  hint,
  className,
  children,
}: {
  id: string;
  label: string;
  info?: ReactNode;
  error?: string | null;
  hint?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div className={cx("min-w-0 space-y-1", className)}>
      <div className="flex items-center gap-1">
        <label htmlFor={id} className="text-[12.5px] font-medium text-muted">
          {label}
        </label>
        {info && <InfoButton label={label}>{info}</InfoButton>}
      </div>
      {children}
      {error ? <span className="block text-[12px] text-err">{error}</span> : hint ? <span className="block text-[12px] text-faint">{hint}</span> : null}
    </div>
  );
}


/** The host part of a base URL, which is what tells two endpoints apart in a list. */
function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/**
 * The long tail of Anthropic-compatible endpoints, searchable. Kept behind "More" because six
 * vendors cover almost everyone and seventy would bury them.
 */
function CatalogBrowser({
  query,
  onQuery,
  taken,
  disabled,
  onPick,
}: {
  query: string;
  onQuery: (value: string) => void;
  taken: (id: string) => boolean;
  disabled: boolean;
  onPick: (id: string) => void;
}) {
  const needle = query.trim().toLowerCase();
  const matches = PROVIDER_CATALOG.filter(
    (entry: CatalogEntry) =>
      !taken(entry.id) && (!needle || entry.name.toLowerCase().includes(needle) || entry.base_url.toLowerCase().includes(needle)),
  );
  return (
    <div className="space-y-2 rounded-xl border border-border bg-panel p-2.5">
      <input
        value={query}
        onChange={(e) => onQuery(e.target.value)}
        placeholder="Search providers"
        aria-label="Search providers"
        autoFocus
        className={inputClass}
      />
      <ul className="scroll-thin max-h-64 space-y-0.5 overflow-y-auto">
        {matches.map((entry) => (
          <li key={entry.id}>
            <button
              type="button"
              disabled={disabled}
              onClick={() => onPick(entry.id)}
              className="flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-2 py-1.5 text-left hover:bg-panel-2 disabled:cursor-not-allowed disabled:opacity-45"
            >
              <ProviderMark preset={entry.id} name={entry.name} />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[13px] font-medium">{entry.name}</span>
                <span className="block truncate font-mono text-[11.5px] text-faint">{hostOf(entry.base_url)}</span>
              </span>
              {entry.wire === "openai" && <Badge tone="info">{WIRE_LABEL.openai}</Badge>}
            </button>
          </li>
        ))}
        {matches.length === 0 && <li className="px-2 py-3 text-[13px] text-faint">Nothing matches that.</li>}
      </ul>
      <p className="px-1 text-[11.5px] leading-snug text-faint">
        {PROVIDER_CATALOG.length} endpoints, from the cc-switch catalogue. Colonizer neither vets nor endorses them, and many resell
        access rather than run the model themselves.
      </p>
    </div>
  );
}

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
  const start = initial ?? presetDraft(preset);
  const wire = initial?.wire ?? presetDraft(preset).wire;
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
  const ids = {
    id: useId(),
    name: useId(),
    url: useId(),
    auth: useId(),
    key: useId(),
    models: useId(),
    fallback: useId(),
  };
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

  return (
    <form onSubmit={save} className="space-y-3 rounded-xl border border-accent/40 bg-panel p-3.5">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-[13.5px] font-semibold">{isNew ? `New ${PRESET_LABEL[preset]} provider` : `Edit ${initial.name}`}</span>
        {!isNew && <KeyBadge provider={initial} />}
        {wire === "openai" && (
          <Badge tone="info" title="Speaks the OpenAI protocol; the Mothership gateway translates">
            {WIRE_LABEL.openai}
          </Badge>
        )}
      </div>
      {isNew && PRESET_HINT[preset] && <p className="text-[12.5px] text-muted">{PRESET_HINT[preset]}</p>}
      <div className="grid gap-3 sm:grid-cols-2">
        <FormField
          id={ids.id}
          label="ID"
          error={idError && id ? idError : null}
          hint={isNew ? `Models are picked as ${id || "id"}/model` : "Can't be changed"}
          info={isNew ? <p>Lowercase letters, digits and dashes. Fixed once saved.</p> : undefined}
        >
          <input
            id={ids.id}
            value={id}
            onChange={(e) => setId(e.target.value.toLowerCase())}
            disabled={!isNew}
            placeholder="my-provider"
            spellCheck={false}
            aria-invalid={Boolean(idError && id)}
            className={cx(inputClass, "font-mono text-[13px] disabled:opacity-60")}
          />
        </FormField>
        <FormField id={ids.name} label="Name">
          <input id={ids.name} value={name} onChange={(e) => setName(e.target.value)} placeholder="My provider" className={inputClass} />
        </FormField>
        <FormField
          id={ids.url}
          label="Base URL"
          className="sm:col-span-2"
          error={urlError && baseUrl ? urlError : null}
          hint={loopback ? "The Mothership connects to this address, so localhost is the Mothership itself." : undefined}
        >
          <input
            id={ids.url}
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder={wire === "openai" ? "https://api.openai.com" : "https://api.example.com/anthropic"}
            spellCheck={false}
            aria-invalid={Boolean(urlError && baseUrl)}
            className={cx(inputClass, "font-mono text-[13px]")}
          />
        </FormField>
        <FormField id={ids.auth} label="Authentication">
          <select id={ids.auth} value={auth} onChange={(e) => setAuth(e.target.value as ProviderAuth)} className={inputClass}>
            {(Object.keys(AUTH_LABEL) as ProviderAuth[]).map((mode) => (
              <option key={mode} value={mode}>
                {AUTH_LABEL[mode]}
              </option>
            ))}
          </select>
        </FormField>
        <FormField id={ids.key} label={auth === "bearer" ? "Token" : "API key"} error={keyError}>
          {auth === "none" ? (
            <p id={ids.key} className="flex h-9 items-center text-[13px] text-faint">
              Not needed
            </p>
          ) : keyMode === "keep" ? (
            <div id={ids.key} className="flex flex-wrap items-center gap-2">
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
            <div id={ids.key} className="flex flex-wrap items-center gap-2">
              <span className="inline-flex h-9 items-center text-[13px] text-warn">Removed on save</span>
              <Button size="sm" variant="ghost" onClick={() => setKeyMode("keep")}>
                Undo
              </Button>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              <input
                id={ids.key}
                type="password"
                autoComplete="off"
                value={key}
                onChange={(e) => setKey(e.target.value)}
                placeholder={initial?.has_key ? "New key" : "sk-…"}
                aria-invalid={Boolean(keyError)}
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
        </FormField>
        <FormField
          id={ids.models}
          label="Models"
          className="sm:col-span-2"
          info={<p>Model IDs as the endpoint expects them. Enter or a comma adds one; leave empty to type IDs where you pick a model.</p>}
        >
          <ChipsInput id={ids.models} values={models} onChange={setModels} placeholder={models.length ? "Add another" : "deepseek-flash, qwen3-coder, …"} />
        </FormField>
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
              label="Request timeout (s)"
              value={limitDraft.timeout_secs}
              onChange={(v) => setLimit("timeout_secs", v)}
              placeholder={String(DEFAULT_TIMEOUT)}
              error={limits.timeout_secs.error}
              help={`How long the Mothership waits for a response. Blank uses ${DEFAULT_TIMEOUT}.`}
            />
            <LimitField
              label="Queue timeout (s)"
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
            <FormField id={ids.fallback} label="Fallback model" info={<p>Used when the provider is unreachable, times out or the queue is full.</p>}>
              <select id={ids.fallback} value={fallback} onChange={(e) => setFallback(e.target.value)} className={inputClass}>
                <option value="">None</option>
                {fallback && !anthropicModels.some((m) => m.id === fallback) && <option value={fallback}>{fallback}</option>}
                {anthropicModels.map((model) => (
                  <option key={model.id} value={model.id}>
                    {model.label === model.id ? model.id : `${model.id} · ${model.label}`}
                  </option>
                ))}
              </select>
            </FormField>
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
  const id = useId();
  return (
    <FormField id={id} label={label} info={<p>{help}</p>} error={error}>
      <input
        id={id}
        inputMode="numeric"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        aria-invalid={Boolean(error)}
        className={cx(inputClass, "font-mono text-[13px]", error && "border-err")}
      />
    </FormField>
  );
}

function ChipsInput({
  id,
  values,
  onChange,
  placeholder,
}: {
  id: string;
  values: string[];
  onChange: (values: string[]) => void;
  placeholder?: string;
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
        id={id}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
        onBlur={() => draft.trim() && commit(draft)}
        placeholder={placeholder}
        spellCheck={false}
        className="min-w-24 flex-1 bg-transparent px-1 font-mono text-[13px] text-text outline-none placeholder:text-faint"
      />
    </div>
  );
}
