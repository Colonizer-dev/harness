import { useEffect, useState, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../context";
import type { HarnessStatus, Issue, Repo, Session } from "../types";
import { IconChevron, IconExternal, IconPlus, IconSearch, IconSettings, IconX } from "./icons";
import { Badge, Button, Spinner, StatusBadge, Switch, cx, inputClass, store, stored, timeAgo } from "./ui";

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
}) {
  const api = useApi();
  const [tab, setTab] = useState<"sessions" | "new">(() => (stored("colonizer.sidebar-tab") === "new" ? "new" : "sessions"));

  useEffect(() => {
    store("colonizer.sidebar-tab", tab);
  }, [tab]);

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

      <StatusRow status={status} error={statusError} onOpenSettings={onOpenSettings} />

      <div role="tablist" aria-label="Sidebar" className="mx-3 mt-2 grid grid-cols-2 gap-1 rounded-lg bg-panel-2 p-1">
        <SidebarTab active={tab === "sessions"} onClick={() => setTab("sessions")}>
          Colonies
          {sessions.length > 0 && <span className="text-faint">{sessions.length}</span>}
        </SidebarTab>
        <SidebarTab active={tab === "new"} onClick={() => setTab("new")}>
          <IconPlus size={13} /> Launch
        </SidebarTab>
      </div>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-2 pb-4 pt-2">
        {tab === "sessions" ? (
          <SessionList
            sessions={sessions}
            loaded={sessionsLoaded}
            selectedId={selectedId}
            onSelect={onSelect}
            onNew={() => setTab("new")}
          />
        ) : (
          <NewSession
            githubConnected={status?.github.connected ?? false}
            statusKnown={status !== null}
            onOpenSettings={onOpenSettings}
            onCreated={(session) => {
              setTab("sessions");
              onCreated(session);
            }}
          />
        )}
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
  onSelect,
  onNew,
}: {
  sessions: Session[];
  loaded: boolean;
  selectedId: string | null;
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
        No colonies yet.
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
              <div className="mt-1 flex gap-2 text-[12px] text-faint">
                <span>{timeAgo(session.updated_at)}</span>
                {session.cost_usd != null && <span>· ${session.cost_usd.toFixed(2)}</span>}
                {session.cleaned_up && <span>· cleaned up</span>}
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
  githubConnected,
  statusKnown,
  onOpenSettings,
  onCreated,
}: {
  githubConnected: boolean;
  statusKnown: boolean;
  onOpenSettings: () => void;
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
    if (!repo || !githubConnected) return;
    let cancelled = false;
    setIssues(null);
    setIssuesError(null);
    api
      .issues(repo)
      .then((list) => !cancelled && setIssues(list))
      .catch((error) => !cancelled && setIssuesError(errorMessage(error)));
    return () => {
      cancelled = true;
    };
  }, [api, repo, githubConnected]);

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

  const query = repoQuery.trim().toLowerCase();
  const matchingRepos = (repos ?? []).filter((r) => r.full_name.toLowerCase().includes(query)).slice(0, 80);
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
      <SectionLabel>Repository</SectionLabel>
      {repo && !picking ? (
        <div className="flex items-center gap-2 rounded-lg border border-border bg-panel py-1.5 pl-3 pr-1.5">
          <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium">{repo}</span>
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
              placeholder="Search, or type owner/repo"
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
              <p className="px-3 py-3 text-[13px] text-muted">No matching repositories</p>
            )}
          </div>
          {repo && (
            <button type="button" className="cursor-pointer px-1 text-[12.5px] text-muted hover:text-text" onClick={() => setPicking(false)}>
              Keep {repo}
            </button>
          )}
        </div>
      )}

      {repo && !picking && (
        <>
          <OpenSessionRow repo={repo} onCreated={onCreated} />
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
                repo={repo}
                issue={issue}
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

function OpenSessionRow({ repo, onCreated }: { repo: string; onCreated: (session: Session) => void }) {
  const api = useApi();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [instructions, setInstructions] = useState("");
  const [starting, setStarting] = useState(false);

  const start = async () => {
    setStarting(true);
    try {
      const session = await api.createSession({ repo, instructions: instructions.trim() || undefined });
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
  open,
  onToggle,
  onCreated,
}: {
  repo: string;
  issue: Issue;
  open: boolean;
  onToggle: () => void;
  onCreated: (session: Session) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [instructions, setInstructions] = useState("");
  const [autopilot, setAutopilot] = useState(false);
  const [starting, setStarting] = useState(false);

  const start = async () => {
    setStarting(true);
    try {
      const session = await api.createSession({
        repo,
        issue: issue.number,
        title: issue.title,
        instructions: instructions.trim() || undefined,
        autopilot,
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
          <div className="flex items-start gap-2.5">
            <Switch checked={autopilot} onChange={setAutopilot} label="Autopilot" />
            <span className="text-[12.5px] leading-snug">
              <span className="font-medium text-text">Autopilot</span>
              <span className="block text-muted">Open the PR automatically when the agent finishes with changes.</span>
            </span>
          </div>
          <Button variant="primary" className="w-full" disabled={starting} onClick={start}>
            {starting ? <Spinner /> : <IconPlus size={15} />} Launch colony
          </Button>
        </div>
      )}
    </li>
  );
}
