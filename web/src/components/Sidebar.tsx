import { useEffect, useRef, useState, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../context";
import type { HarnessStatus, Issue, OrgInfo, Repo, Session } from "../types";
import {
  IconCheck,
  IconChevron,
  IconChevronDown,
  IconExternal,
  IconMemory,
  IconOrg,
  IconPlus,
  IconSearch,
  IconSettings,
  IconX,
} from "./icons";
import {
  AttentionBadge,
  Badge,
  Button,
  Spinner,
  StatusBadge,
  Switch,
  cx,
  inputClass,
  isLive,
  orgOf,
  sameOrg,
  store,
  stored,
  timeAgo,
} from "./ui";

export type MainView = "colonies" | "memory";

export function LogoMark({ size = 28 }: { size?: number }) {
  return (
    // The "outpost": a hexagon with a beacon at its center.
    <span
      className="grid shrink-0 place-items-center rounded-lg border border-border bg-panel-2 text-accent"
      style={{ width: size, height: size }}
    >
      <svg width={size * 0.68} height={size * 0.68} viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 2.8 20 7.4v9.2L12 21.2 4 16.6V7.4z" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round" />
        <circle cx="12" cy="12" r="2.6" fill="currentColor" />
      </svg>
    </span>
  );
}

export function Sidebar({
  status,
  statusError,
  sessions,
  sessionsLoaded,
  selectedId,
  onSelect,
  onCreated,
  onOpenSettings,
  onClose,
  orgs,
  selectedOrg,
  onSelectOrg,
  onOpenOrgSettings,
  view,
  onOpenMemory,
  pendingMemory,
  autopilotDefault,
}: {
  status: HarnessStatus | null;
  statusError: boolean;
  sessions: Session[];
  sessionsLoaded: boolean;
  selectedId: string | null;
  onSelect: (id: string) => void;
  onCreated: (session: Session) => void;
  onOpenSettings: () => void;
  onClose?: () => void;
  orgs: OrgInfo[];
  /** null means "All orgs". */
  selectedOrg: string | null;
  onSelectOrg: (org: string | null) => void;
  onOpenOrgSettings: (org: string) => void;
  view: MainView;
  onOpenMemory: () => void;
  pendingMemory: number;
  autopilotDefault: boolean;
}) {
  const api = useApi();
  const [tab, setTab] = useState<"sessions" | "new">(() => (stored("colonizer.sidebar-tab") === "new" ? "new" : "sessions"));

  useEffect(() => {
    store("colonizer.sidebar-tab", tab);
  }, [tab]);

  const visible = selectedOrg ? sessions.filter((s) => sameOrg(orgOf(s), selectedOrg)) : sessions;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-center gap-2.5 px-4 pb-2 pt-3.5">
        <LogoMark />
        <div className="min-w-0 flex-1 leading-tight">
          <div className="truncate text-[15px] font-semibold">Colonizer</div>
          <div className="truncate text-[11.5px] text-faint">Colonize your backlog.</div>
        </div>
        {api.mock && <Badge tone="warn">Mock data</Badge>}
        <button
          type="button"
          onClick={onOpenSettings}
          aria-label="Settings"
          title="Settings"
          className="grid size-8 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
        >
          <IconSettings size={17} />
        </button>
        {onClose && (
          <button
            type="button"
            onClick={onClose}
            aria-label="Close sidebar"
            className="grid size-8 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
          >
            <IconX size={17} />
          </button>
        )}
      </div>

      <div className="mx-3 mb-1 flex items-center gap-1.5">
        <OrgSwitcher orgs={orgs} sessions={sessions} selected={selectedOrg} onSelect={onSelectOrg} />
        {selectedOrg && (
          <button
            type="button"
            onClick={() => onOpenOrgSettings(selectedOrg)}
            aria-label={`Settings for ${selectedOrg}`}
            title={`Settings for ${selectedOrg}`}
            className="grid size-9 shrink-0 cursor-pointer place-items-center rounded-lg border border-border bg-panel text-muted hover:bg-panel-2 hover:text-text"
          >
            <IconSettings size={15} />
          </button>
        )}
      </div>

      <StatusRow status={status} error={statusError} onOpenSettings={onOpenSettings} />

      <div role="tablist" aria-label="Sidebar" className="mx-3 mt-2 grid grid-cols-2 gap-1 rounded-lg bg-panel-2 p-1">
        <SidebarTab active={tab === "sessions"} onClick={() => setTab("sessions")}>
          Colonies
          {visible.length > 0 && <span className="text-faint">{visible.length}</span>}
        </SidebarTab>
        <SidebarTab active={tab === "new"} onClick={() => setTab("new")}>
          <IconPlus size={13} /> Launch
        </SidebarTab>
      </div>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-2 pb-4 pt-2">
        {tab === "sessions" ? (
          <SessionList
            sessions={visible}
            loaded={sessionsLoaded}
            selectedId={view === "colonies" ? selectedId : null}
            org={selectedOrg}
            onSelect={onSelect}
            onNew={() => setTab("new")}
          />
        ) : (
          <NewSession
            org={selectedOrg}
            githubConnected={status?.github.connected ?? false}
            statusKnown={status !== null}
            onOpenSettings={onOpenSettings}
            autopilotDefault={autopilotDefault}
            onCreated={(session) => {
              setTab("sessions");
              onCreated(session);
            }}
          />
        )}
      </div>

      <div className="shrink-0 border-t border-border p-2">
        <button
          type="button"
          onClick={onOpenMemory}
          aria-current={view === "memory" ? "page" : undefined}
          className={cx(
            "flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-3 py-2 text-left text-[13.5px] font-medium transition-colors",
            view === "memory" ? "bg-accent-soft text-text" : "text-muted hover:bg-panel-2 hover:text-text",
          )}
        >
          <IconMemory size={16} className={view === "memory" ? "text-accent" : undefined} />
          <span className="min-w-0 flex-1">Memory</span>
          {pendingMemory > 0 && (
            <span
              className="rounded-full bg-accent px-1.5 text-[11.5px] font-semibold leading-5 text-on-accent"
              aria-label={`${pendingMemory} waiting for review`}
            >
              {pendingMemory}
            </span>
          )}
        </button>
      </div>
    </div>
  );
}

function SidebarTab({ active, onClick, children }: { active: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active}
      onClick={onClick}
      className={cx(
        "flex cursor-pointer items-center justify-center gap-1.5 rounded-md px-2 py-1.5 text-[13px] font-medium transition-colors",
        active ? "bg-panel text-text shadow-sm" : "text-muted hover:text-text",
      )}
    >
      {children}
    </button>
  );
}

interface OrgEntry {
  org: string;
  live: number;
  total: number;
  pending: number;
}

/** Orgs from GET /api/orgs plus any org that only appears in the colony list; counts come from the live list. */
function orgEntries(orgs: OrgInfo[], sessions: Session[]): OrgEntry[] {
  const byKey = new Map<string, OrgEntry>();
  const entry = (org: string) => {
    const key = org.toLowerCase();
    let found = byKey.get(key);
    if (!found) byKey.set(key, (found = { org, live: 0, total: 0, pending: 0 }));
    return found;
  };
  for (const info of orgs) entry(info.org).pending = info.pending_memory ?? 0;
  for (const session of sessions) {
    const org = orgOf(session);
    if (!org) continue;
    const e = entry(org);
    e.total += 1;
    if (isLive(session.status)) e.live += 1;
  }
  return [...byKey.values()].sort((a, b) => a.org.localeCompare(b.org));
}

function colonyCount(n: number): string {
  return `${n} ${n === 1 ? "colony" : "colonies"}`;
}

function OrgSwitcher({
  orgs,
  sessions,
  selected,
  onSelect,
}: {
  orgs: OrgInfo[];
  sessions: Session[];
  selected: string | null;
  onSelect: (org: string | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (event: MouseEvent) => {
      if (!ref.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const entries = orgEntries(orgs, sessions);
  const current = selected ? entries.find((e) => sameOrg(e.org, selected)) : null;
  const totalLive = sessions.filter((s) => isLive(s.status)).length;
  const live = current ? current.live : totalLive;
  const choose = (org: string | null) => {
    onSelect(org);
    setOpen(false);
  };

  return (
    <div ref={ref} className="relative min-w-0 flex-1">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label="Switch organisation"
        className="flex h-9 w-full cursor-pointer items-center gap-2 rounded-lg border border-border bg-panel px-2.5 text-left hover:bg-panel-2"
      >
        <span className="grid size-5 shrink-0 place-items-center rounded-md bg-panel-3 text-muted">
          <IconOrg size={12} />
        </span>
        <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium">{current?.org ?? selected ?? "All orgs"}</span>
        {live > 0 && <span className="shrink-0 text-[11.5px] text-info">{live} live</span>}
        <IconChevronDown size={14} className={cx("shrink-0 text-faint transition-transform", open && "rotate-180")} />
      </button>
      {open && (
        <ul
          role="listbox"
          aria-label="Organisations"
          className="scroll-thin absolute inset-x-0 top-[calc(100%+4px)] z-30 max-h-80 overflow-y-auto rounded-xl border border-border bg-panel p-1 shadow-[var(--shadow)]"
        >
          <OrgOption label="All orgs" meta={colonyCount(sessions.length)} live={totalLive} pending={0} active={!selected} onClick={() => choose(null)} />
          {entries.length > 0 && <li role="separator" className="my-1 border-t border-border" />}
          {entries.map((e) => (
            <OrgOption
              key={e.org}
              label={e.org}
              meta={colonyCount(e.total)}
              live={e.live}
              pending={e.pending}
              active={sameOrg(selected, e.org)}
              onClick={() => choose(e.org)}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function OrgOption({
  label,
  meta,
  live,
  pending,
  active,
  onClick,
}: {
  label: string;
  meta: string;
  live: number;
  pending: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <li role="option" aria-selected={active}>
      <button
        type="button"
        onClick={onClick}
        className={cx("flex w-full cursor-pointer items-center gap-2 rounded-lg px-2.5 py-2 text-left", active ? "bg-accent-soft" : "hover:bg-panel-2")}
      >
        <span className="min-w-0 flex-1">
          <span className="block truncate text-[13.5px] font-medium">{label}</span>
          <span className="block text-[11.5px] text-faint">
            {meta}
            {pending > 0 && ` · ${pending} to review`}
          </span>
        </span>
        {live > 0 && <Badge tone="info">{live} live</Badge>}
        <IconCheck size={14} className={cx("shrink-0 text-accent", !active && "invisible")} />
      </button>
    </li>
  );
}

function StatusRow({ status, error, onOpenSettings }: { status: HarnessStatus | null; error: boolean; onOpenSettings: () => void }) {
  if (!status) {
    return (
      <div className="mx-4 flex items-center gap-2 py-1 text-[12px] text-muted">
        {error ? (
          <>
            <span className="size-1.5 rounded-full bg-err" /> Mothership unreachable
          </>
        ) : (
          <>
            <Spinner className="size-3" /> Checking connections…
          </>
        )}
      </div>
    );
  }
  const mesh = status.mesh;
  const items: { label: string; state: "ok" | "bad" | "off" }[] = [
    { label: status.github.connected ? `@${status.github.login}` : "GitHub", state: status.github.connected ? "ok" : "bad" },
    { label: "Claude", state: status.claude.configured ? "ok" : "bad" },
    { label: "microVMs", state: status.sandbox.msb_version ? "ok" : "bad" },
    {
      label: mesh?.enabled ? `Mesh${mesh.nodes != null ? ` · ${mesh.nodes}` : ""}` : "Mesh off",
      state: !mesh || !mesh.enabled ? "off" : mesh.error || mesh.state === "error" ? "bad" : "ok",
    },
  ];
  return (
    <button
      type="button"
      onClick={onOpenSettings}
      title="Connections and modules"
      className="mx-3 flex cursor-pointer flex-wrap gap-x-3 gap-y-1 rounded-lg px-1 py-1 text-left text-[12px] text-muted hover:text-text"
    >
      {items.map((item) => (
        <span key={item.label} className="inline-flex items-center gap-1.5">
          <span className={cx("size-1.5 rounded-full", item.state === "ok" ? "bg-ok" : item.state === "bad" ? "bg-err" : "bg-faint")} />
          {item.label}
        </span>
      ))}
    </button>
  );
}

function SessionList({
  sessions,
  loaded,
  selectedId,
  org,
  onSelect,
  onNew,
}: {
  sessions: Session[];
  loaded: boolean;
  selectedId: string | null;
  org: string | null;
  onSelect: (id: string) => void;
  onNew: () => void;
}) {
  if (!loaded) {
    return (
      <div className="grid place-items-center py-10 text-muted">
        <Spinner />
      </div>
    );
  }
  if (sessions.length === 0) {
    return (
      <div className="px-3 py-10 text-center text-[13px] text-muted">
        {org ? `No colonies in ${org} yet.` : "No colonies yet."}
        <div className="mt-3">
          <Button size="sm" onClick={onNew}>
            <IconPlus size={13} /> Launch one from an issue
          </Button>
        </div>
      </div>
    );
  }
  return (
    <ul className="space-y-0.5">
      {sessions.map((session) => {
        const active = session.id === selectedId;
        return (
          <li key={session.id}>
            <button
              type="button"
              onClick={() => onSelect(session.id)}
              aria-current={active ? "true" : undefined}
              className={cx(
                "w-full cursor-pointer rounded-xl px-3 py-2.5 text-left transition-colors",
                active ? "bg-accent-soft" : "hover:bg-panel-2",
              )}
            >
              <div className="flex items-center justify-between gap-2">
                <span className="min-w-0 truncate font-mono text-[12px] text-muted">
                  {session.repo}
                  {session.issue != null && `#${session.issue}`}
                </span>
                <StatusBadge status={session.status} />
              </div>
              <div className="mt-1 line-clamp-2 text-[13.5px] font-medium leading-snug">
                {session.issue_title || (session.issue != null ? `Issue #${session.issue}` : "Open colony")}
              </div>
              <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-[12px] text-faint">
                {!org && (
                  <span className="rounded-md bg-panel-3 px-1.5 text-[11px] font-medium leading-[18px] text-muted">{orgOf(session)}</span>
                )}
                <span>{timeAgo(session.updated_at)}</span>
                {session.cost_usd != null && <span>· ${session.cost_usd.toFixed(2)}</span>}
                {session.cleaned_up && <span>· cleaned up</span>}
                <AttentionBadge attention={session.attention} />
              </div>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

function SectionLabel({ children }: { children: ReactNode }) {
  return <div className="px-1 text-[11.5px] font-semibold uppercase tracking-wide text-faint">{children}</div>;
}

function NewSession({
  org,
  githubConnected,
  statusKnown,
  onOpenSettings,
  autopilotDefault,
  onCreated,
}: {
  org: string | null;
  githubConnected: boolean;
  statusKnown: boolean;
  onOpenSettings: () => void;
  autopilotDefault: boolean;
  onCreated: (session: Session) => void;
}) {
  const api = useApi();
  const [repos, setRepos] = useState<Repo[] | null>(null);
  const [reposError, setReposError] = useState<string | null>(null);
  const [repoQuery, setRepoQuery] = useState("");
  const [repo, setRepo] = useState<string | null>(() => stored("colonizer.repo"));
  const [picking, setPicking] = useState(() => !stored("colonizer.repo"));
  const [issues, setIssues] = useState<Issue[] | null>(null);
  const [issuesError, setIssuesError] = useState<string | null>(null);
  const [issueQuery, setIssueQuery] = useState("");
  const [openIssue, setOpenIssue] = useState<number | null>(null);

  const inOrg = (name: string) => !org || sameOrg(name.split("/")[0], org);
  // A remembered repository from another org doesn't belong in this workspace.
  const activeRepo = repo && inOrg(repo) ? repo : null;

  useEffect(() => {
    if (!githubConnected) return;
    let cancelled = false;
    setReposError(null);
    api
      .repos()
      .then((list) => !cancelled && setRepos(list))
      .catch((error) => !cancelled && setReposError(errorMessage(error)));
    return () => {
      cancelled = true;
    };
  }, [api, githubConnected]);

  useEffect(() => {
    if (!activeRepo || !githubConnected) return;
    let cancelled = false;
    setIssues(null);
    setIssuesError(null);
    api
      .issues(activeRepo)
      .then((list) => !cancelled && setIssues(list))
      .catch((error) => !cancelled && setIssuesError(errorMessage(error)));
    return () => {
      cancelled = true;
    };
  }, [api, activeRepo, githubConnected]);

  if (!statusKnown) {
    return (
      <div className="grid place-items-center py-10 text-muted">
        <Spinner />
      </div>
    );
  }
  if (!githubConnected) {
    return (
      <div className="px-3 py-10 text-center text-[13px] text-muted">
        Connect GitHub to browse issues.
        <div className="mt-3">
          <Button size="sm" onClick={onOpenSettings}>
            Open settings
          </Button>
        </div>
      </div>
    );
  }

  const chooseRepo = (name: string) => {
    setRepo(name);
    store("colonizer.repo", name);
    setPicking(false);
    setRepoQuery("");
    setIssueQuery("");
    setOpenIssue(null);
  };

  const showPicker = picking || !activeRepo;
  const query = repoQuery.trim().toLowerCase();
  const orgRepos = (repos ?? []).filter((r) => inOrg(r.full_name));
  const matchingRepos = orgRepos.filter((r) => r.full_name.toLowerCase().includes(query)).slice(0, 80);
  const typedRepo =
    /^[\w.-]+\/[\w.-]+$/.test(repoQuery.trim()) && !(repos ?? []).some((r) => r.full_name.toLowerCase() === query)
      ? repoQuery.trim()
      : null;
  const issueFilter = issueQuery.trim().toLowerCase();
  const matchingIssues = (issues ?? []).filter(
    (i) => !issueFilter || i.title.toLowerCase().includes(issueFilter) || String(i.number).includes(issueFilter),
  );

  return (
    <div className="space-y-2 px-1">
      <SectionLabel>{org ? `Repository in ${org}` : "Repository"}</SectionLabel>
      {!showPicker && activeRepo ? (
        <div className="flex items-center gap-2 rounded-lg border border-border bg-panel py-1.5 pl-3 pr-1.5">
          <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium">{activeRepo}</span>
          <Button size="sm" variant="ghost" onClick={() => setPicking(true)}>
            Change
          </Button>
        </div>
      ) : (
        <div className="space-y-1.5">
          <div className="relative">
            <IconSearch size={14} className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-faint" />
            <input
              value={repoQuery}
              onChange={(e) => setRepoQuery(e.target.value)}
              placeholder={org ? `Search ${org}, or type owner/repo` : "Search, or type owner/repo"}
              aria-label="Search repositories"
              className={cx(inputClass, "pl-8")}
            />
          </div>
          <div className="scroll-thin max-h-80 overflow-y-auto rounded-lg border border-border bg-panel">
            {reposError && <p className="px-3 py-3 text-[13px] text-err">{reposError}</p>}
            {!repos && !reposError && (
              <div className="flex items-center gap-2 px-3 py-3 text-[13px] text-muted">
                <Spinner /> Loading repositories…
              </div>
            )}
            {typedRepo && <RepoRow name={typedRepo} meta="Open this repository" onClick={() => chooseRepo(typedRepo)} />}
            {matchingRepos.map((r) => (
              <RepoRow
                key={r.full_name}
                name={r.full_name}
                meta={[r.private ? "private" : "public", `${r.open_issues_count} open`, r.archived ? "archived" : null, r.fork ? "fork" : null]
                  .filter(Boolean)
                  .join(" · ")}
                onClick={() => chooseRepo(r.full_name)}
              />
            ))}
            {repos && matchingRepos.length === 0 && !typedRepo && (
              <p className="px-3 py-3 text-[13px] text-muted">
                {org && orgRepos.length === 0 ? `No repositories in ${org}` : "No matching repositories"}
              </p>
            )}
          </div>
          {activeRepo && (
            <button type="button" className="cursor-pointer px-1 text-[12.5px] text-muted hover:text-text" onClick={() => setPicking(false)}>
              Keep {activeRepo}
            </button>
          )}
        </div>
      )}

      {activeRepo && !showPicker && (
        <>
          <OpenSessionRow repo={activeRepo} autopilotDefault={autopilotDefault} onCreated={onCreated} />
          <div className="flex items-center justify-between px-1 pt-3">
            <SectionLabel>Open issues</SectionLabel>
            {issues && <span className="text-[11.5px] text-faint">{issues.length}</span>}
          </div>
          {issues && issues.length > 6 && (
            <input
              value={issueQuery}
              onChange={(e) => setIssueQuery(e.target.value)}
              placeholder="Filter issues"
              aria-label="Filter issues"
              className={inputClass}
            />
          )}
          {issuesError && <p className="px-2 text-[13px] text-err">{issuesError}</p>}
          {!issues && !issuesError && (
            <div className="flex items-center gap-2 px-2 py-3 text-[13px] text-muted">
              <Spinner /> Loading issues…
            </div>
          )}
          {issues && issues.length === 0 && <p className="px-2 py-6 text-center text-[13px] text-muted">No open issues</p>}
          <ul className="space-y-1">
            {matchingIssues.map((issue) => (
              <IssueRow
                key={issue.number}
                repo={activeRepo}
                issue={issue}
                autopilotDefault={autopilotDefault}
                open={openIssue === issue.number}
                onToggle={() => setOpenIssue(openIssue === issue.number ? null : issue.number)}
                onCreated={onCreated}
              />
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

function OpenSessionRow({
  repo,
  autopilotDefault,
  onCreated,
}: {
  repo: string;
  autopilotDefault: boolean;
  onCreated: (session: Session) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [instructions, setInstructions] = useState("");
  // null follows the server default until the switch is touched.
  const [autopilot, setAutopilot] = useState<boolean | null>(null);
  const [starting, setStarting] = useState(false);

  const start = async () => {
    setStarting(true);
    try {
      const session = await api.createSession({ repo, instructions: instructions.trim() || undefined, autopilot: autopilot ?? undefined });
      toast(`Colony launched on ${repo}`);
      onCreated(session);
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setStarting(false);
    }
  };

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="flex w-full cursor-pointer items-center gap-2.5 rounded-xl border border-dashed border-border-strong px-3 py-2.5 text-left text-muted hover:bg-panel-2 hover:text-text"
      >
        <IconPlus size={15} className="shrink-0" />
        <span className="min-w-0 flex-1">
          <span className="block text-[13.5px] font-medium text-text">Launch colony</span>
          <span className="block text-[12px]">A microVM on this repository — no issue needed</span>
        </span>
      </button>
    );
  }

  return (
    <div className="space-y-2.5 rounded-xl border border-border bg-panel p-3 shadow-[var(--shadow)]">
      <div className="flex items-center justify-between gap-2">
        <span className="text-[13.5px] font-medium">New colony on this repository</span>
        <Button size="sm" variant="ghost" onClick={() => setOpen(false)}>
          Cancel
        </Button>
      </div>
      <textarea
        value={instructions}
        onChange={(e) => setInstructions(e.target.value)}
        rows={3}
        autoFocus
        placeholder="What should the agent work on? Optional — you can also just chat."
        aria-label="Colony instructions"
        className={cx(inputClass, "resize-y text-[13px]")}
      />
      <AutopilotSwitch checked={autopilot ?? autopilotDefault} onChange={setAutopilot} />
      <Button variant="primary" className="w-full" disabled={starting} onClick={start}>
        {starting ? <Spinner /> : <IconPlus size={15} />} Launch colony
      </Button>
    </div>
  );
}

function RepoRow({ name, meta, onClick }: { name: string; meta: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="block w-full cursor-pointer border-b border-border px-3 py-2 text-left last:border-b-0 hover:bg-panel-2"
    >
      <span className="block truncate text-[13.5px] font-medium">{name}</span>
      <span className="block text-[12px] text-faint">{meta}</span>
    </button>
  );
}

function IssueRow({
  repo,
  issue,
  autopilotDefault,
  open,
  onToggle,
  onCreated,
}: {
  repo: string;
  issue: Issue;
  autopilotDefault: boolean;
  open: boolean;
  onToggle: () => void;
  onCreated: (session: Session) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [instructions, setInstructions] = useState("");
  const [autopilot, setAutopilot] = useState<boolean | null>(null);
  const [starting, setStarting] = useState(false);

  const start = async () => {
    setStarting(true);
    try {
      const session = await api.createSession({
        repo,
        issue: issue.number,
        title: issue.title,
        instructions: instructions.trim() || undefined,
        autopilot: autopilot ?? undefined,
      });
      toast(`Colony launched for #${issue.number}`);
      onCreated(session);
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setStarting(false);
    }
  };

  return (
    <li className={cx("rounded-xl border transition-colors", open ? "border-border bg-panel shadow-[var(--shadow)]" : "border-transparent hover:bg-panel-2")}>
      <button type="button" onClick={onToggle} aria-expanded={open} className="flex w-full cursor-pointer items-start gap-2 px-3 py-2.5 text-left">
        <span className="mt-px shrink-0 font-mono text-[12px] text-faint">#{issue.number}</span>
        <span className="min-w-0 flex-1">
          <span className="block text-[13.5px] font-medium leading-snug">{issue.title}</span>
          <span className="mt-1 flex flex-wrap items-center gap-1.5 text-[11.5px] text-faint">
            {issue.labels.slice(0, 3).map((label) => (
              <span key={label.name} className="inline-flex items-center gap-1 rounded-full border border-border px-1.5 leading-4 text-muted">
                <span
                  className="size-1.5 rounded-full"
                  style={{ background: /^[0-9a-f]{6}$/i.test(label.color) ? `#${label.color}` : "var(--faint)" }}
                />
                {label.name}
              </span>
            ))}
            <span>{timeAgo(issue.updatedAt)}</span>
          </span>
        </span>
        <IconChevron size={14} className={cx("mt-1 shrink-0 text-faint transition-transform", open && "rotate-90")} />
      </button>
      {open && (
        <div className="space-y-3 border-t border-border px-3 pb-3 pt-2.5">
          {issue.body?.trim() && (
            <p className="line-clamp-6 whitespace-pre-wrap text-[13px] text-muted [overflow-wrap:anywhere]">{issue.body.trim()}</p>
          )}
          <a href={issue.url} target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1 text-[12.5px] text-accent hover:underline">
            View on GitHub <IconExternal size={12} />
          </a>
          <textarea
            value={instructions}
            onChange={(e) => setInstructions(e.target.value)}
            rows={3}
            placeholder="Extra instructions for the agent (optional)"
            aria-label="Extra instructions"
            className={cx(inputClass, "resize-y text-[13px]")}
          />
          <AutopilotSwitch checked={autopilot ?? autopilotDefault} onChange={setAutopilot} />
          <Button variant="primary" className="w-full" disabled={starting} onClick={start}>
            {starting ? <Spinner /> : <IconPlus size={15} />} Launch colony
          </Button>
        </div>
      )}
    </li>
  );
}

function AutopilotSwitch({ checked, onChange }: { checked: boolean; onChange: (checked: boolean) => void }) {
  return (
    <div className="flex items-start gap-2.5">
      <Switch checked={checked} onChange={onChange} label="Autopilot" />
      <span className="text-[12.5px] leading-snug">
        <span className="font-medium text-text">Autopilot</span>
        <span className="block text-muted">Open the PR automatically when the agent finishes and writes its PR description.</span>
      </span>
    </div>
  );
}
