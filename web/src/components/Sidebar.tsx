import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { ApiError, heldByFor, heldInBatch } from "../api";
import { errorMessage, useApi, useToast } from "../context";
import { colonyLabel, needsYou, needsYouLabel } from "../notifications";
import { orgEntries } from "../orgs";
import { sortSessions } from "../sessionOrder";
import { formatCost, sessionCost } from "../spend";
import type { HarnessStatus, Issue, OrgInfo, Repo, Session } from "../types";
import { type ImagePull } from "../useImagePull";
import { Avatar } from "./Avatar";
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
import { AttentionBadge, Badge, Button, Spinner, StatusBadge, Switch, cx, inputClass, meshBroken, occupiesSlot, orgOf, sameOrg, seconds, store, stored, timeAgo } from "./ui";

/** Which pane the sidebar shows. Owned by App so Settings's Setup pane can open the launcher. */
export type SidebarTab = "sessions" | "new";

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
  onOpenColony,
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
  attentionStrip,
  tab,
  onTab,
  pull,
}: {
  status: HarnessStatus | null;
  statusError: boolean;
  sessions: Session[];
  sessionsLoaded: boolean;
  selectedId: string | null;
  onSelect: (id: string) => void;
  /** Opening from the strip may also have to drop the org filter: the strip lists every waiting colony whatever the filter shows. App decides; the strip stays presentational. */
  onOpenColony: (session: Session) => void;
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
  /** The notification layer's in-tab switch: when off, no strip — today's sidebar exactly. */
  attentionStrip: boolean;
  /** Lifted to App: Setup's Launch button opens this tab directly. */
  tab: SidebarTab;
  onTab: (tab: SidebarTab) => void;
  /** App's one image-pull poller; the download stays visible here after Setup closes. */
  pull: ImagePull;
}) {
  const api = useApi();

  // The sidebar remembers its own tab across loads; Setup writes nothing here.
  useEffect(() => {
    store("colonizer.sidebar-tab", tab);
  }, [tab]);

  const visible = useMemo(
    () => sortSessions(selectedOrg ? sessions.filter((s) => sameOrg(orgOf(s), selectedOrg)) : sessions),
    [sessions, selectedOrg],
  );

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
        <OrgSwitcher orgs={orgs} sessions={sessions} selected={selectedOrg} onSelect={onSelectOrg} onOpenSettings={onOpenOrgSettings} />
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
      <PullIndicator pull={pull} />

      {attentionStrip && <AttentionStrip sessions={sessions} onOpenColony={onOpenColony} />}

      <div role="tablist" aria-label="Sidebar" className="mx-3 mt-2 grid grid-cols-2 gap-1 rounded-lg bg-panel-2 p-1">
        <SidebarTab active={tab === "sessions"} onClick={() => onTab("sessions")}>
          Colonies
          {visible.length > 0 && <span className="text-faint">{visible.length}</span>}
        </SidebarTab>
        <SidebarTab active={tab === "new"} onClick={() => onTab("new")}>
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
            onNew={() => onTab("new")}
          />
        ) : (
          <NewSession
            org={selectedOrg}
            githubConnected={status?.github.connected ?? false}
            statusKnown={status !== null}
            onOpenSettings={onOpenSettings}
            autopilotDefault={autopilotDefault}
            sessions={sessions}
            onOpenColony={onOpenColony}
            onCreated={(session) => {
              onTab("sessions");
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

function colonyCount(n: number): string {
  return `${n} ${n === 1 ? "colony" : "colonies"}`;
}

/** "3 live · 2 queued"; either half drops out, and neither means show nothing. */
function liveQueuedLabel(live: number, queued: number): string {
  return [live > 0 ? `${live} live` : null, queued > 0 ? `${queued} queued` : null].filter(Boolean).join(" · ");
}

function OrgSwitcher({
  orgs,
  sessions,
  selected,
  onSelect,
  onOpenSettings,
}: {
  orgs: OrgInfo[];
  sessions: Session[];
  selected: string | null;
  onSelect: (org: string | null) => void;
  /** A hidden org is not a workspace choice, but its settings dialog must stay reachable — this is how. */
  onOpenSettings: (org: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [showHidden, setShowHidden] = useState(false);
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

  const { visible, hidden } = orgEntries(orgs, sessions);
  // A hidden org still answers "where am I" if it was selected when it was switched off.
  const current = selected ? [...visible, ...hidden].find((e) => sameOrg(e.org, selected)) : null;
  const totalLive = sessions.filter((s) => occupiesSlot(s.status)).length;
  const totalQueued = sessions.filter((s) => s.status === "queued").length;
  const live = current ? current.live : totalLive;
  const queued = current ? current.queued : totalQueued;
  const counts = liveQueuedLabel(live, queued);
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
        {current ? (
          <Avatar name={current.org} src={current.avatar} size={20} rounded="md" />
        ) : (
          <span className="grid size-5 shrink-0 place-items-center rounded-md bg-panel-3 text-muted">
            <IconOrg size={12} />
          </span>
        )}
        <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium">{current?.org ?? selected ?? "All orgs"}</span>
        {counts && <span className="shrink-0 text-[11.5px] text-info">{counts}</span>}
        <IconChevronDown size={14} className={cx("shrink-0 text-faint transition-transform", open && "rotate-180")} />
      </button>
      {open && (
        <div className="scroll-thin absolute inset-x-0 top-[calc(100%+4px)] z-30 max-h-80 overflow-y-auto rounded-xl border border-border bg-panel p-1 shadow-[var(--shadow)]">
          <ul role="listbox" aria-label="Organisations">
            <OrgOption
              label="All orgs"
              meta={colonyCount(sessions.length)}
              live={totalLive}
              queued={totalQueued}
              pending={0}
              active={!selected}
              icon={
                <span className="grid size-5 shrink-0 place-items-center rounded-md bg-panel-3 text-muted">
                  <IconOrg size={12} />
                </span>
              }
              onClick={() => choose(null)}
            />
            {visible.length > 0 && <li role="separator" className="my-1 border-t border-border" />}
            {visible.map((e) => (
              <OrgOption
                key={e.org}
                label={e.org}
                meta={colonyCount(e.total)}
                live={e.live}
                queued={e.queued}
                pending={e.pending}
                avatar={e.avatar}
                active={sameOrg(selected, e.org)}
                onClick={() => choose(e.org)}
              />
            ))}
          </ul>
          {hidden.length > 0 && (
            <div className="mt-1 border-t border-border pt-1">
              <button
                type="button"
                onClick={() => setShowHidden((v) => !v)}
                aria-expanded={showHidden}
                className="flex w-full cursor-pointer items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-left text-[12px] text-muted hover:bg-panel-2 hover:text-text"
              >
                <IconChevron size={12} className={cx("shrink-0 transition-transform", showHidden && "rotate-90")} />
                Hidden ({hidden.length})
              </button>
              {showHidden && (
                <ul className="pb-1">
                  {hidden.map((e) => (
                    <li key={e.org} className="flex items-center gap-2 rounded-lg px-2.5 py-1.5">
                      <Avatar name={e.org} src={e.avatar} size={20} rounded="md" />
                      <span className="min-w-0 flex-1 truncate text-[13px] text-muted">{e.org}</span>
                      <Badge tone="neutral">Off</Badge>
                      <button
                        type="button"
                        onClick={() => {
                          setOpen(false);
                          onOpenSettings(e.org);
                        }}
                        aria-label={`Settings for ${e.org}`}
                        title={`Settings for ${e.org}`}
                        className="grid size-7 shrink-0 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-3 hover:text-text"
                      >
                        <IconSettings size={14} />
                      </button>
                    </li>
                  ))}
                  <li className="px-2.5 pt-0.5 text-[11.5px] text-faint">Switched off in their settings; their colonies stay listed.</li>
                </ul>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function OrgOption({
  label,
  meta,
  live,
  queued,
  pending,
  avatar,
  icon,
  active,
  onClick,
}: {
  label: string;
  meta: string;
  live: number;
  queued: number;
  pending: number;
  avatar?: string | null;
  /** A fixed tile for the row that is not one org ("All orgs"). */
  icon?: ReactNode;
  active: boolean;
  onClick: () => void;
}) {
  const counts = liveQueuedLabel(live, queued);
  return (
    <li role="option" aria-selected={active}>
      <button
        type="button"
        onClick={onClick}
        className={cx("flex w-full cursor-pointer items-center gap-2 rounded-lg px-2.5 py-2 text-left", active ? "bg-accent-soft" : "hover:bg-panel-2")}
      >
        {icon ?? <Avatar name={label} src={avatar} size={20} rounded="md" />}
        <span className="min-w-0 flex-1">
          <span className="block truncate text-[13.5px] font-medium">{label}</span>
          <span className="block text-[11.5px] text-faint">
            {meta}
            {pending > 0 && ` · ${pending} to review`}
          </span>
        </span>
        {counts && <Badge tone="info">{counts}</Badge>}
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
      state: !mesh || !mesh.enabled ? "off" : meshBroken(mesh) ? "bad" : "ok",
    },
  ];
  // Only a mothership that reports storage health gets the dot; older ones (no `storage`) show nothing new.
  // The reclaim counts ride the same poll: "N reclaimable · M unpushed" points at per-colony cleanup.
  if (status.storage) {
    const reclaim = status.reclaim;
    const pending =
      reclaim && (reclaim.reclaimable > 0 || reclaim.unpushed > 0)
        ? ` · ${reclaim.reclaimable} reclaimable · ${reclaim.unpushed} unpushed`
        : "";
    items.push({ label: `Storage${pending}`, state: status.storage.ok === false ? "bad" : "ok" });
  }
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

/**
 * The in-tab call to action for colonies waiting on a person: sits above the colony list so it is
 * the first thing seen when the tab comes to the front, and each entry opens that colony's chat.
 * Deliberately plain — repository and issue number only, never the issue title or any error text,
 * for the same reason notifications stay dull. The list itself remains the full record; this is a
 * pointer to it, so a missed entry here hides nothing. It counts across every org, so the entry
 * hands over the whole colony and App's `onOpenColony` makes sure the jump lands where the list
 * can follow.
 */
function AttentionStrip({ sessions, onOpenColony }: { sessions: Session[]; onOpenColony: (session: Session) => void }) {
  const needing = useMemo(() => sortSessions(sessions.filter(needsYou)), [sessions]);
  if (needing.length === 0) return null;
  return (
    <div role="region" aria-label={needsYouLabel(needing.length)} className="mx-3 mt-2 rounded-xl border border-warn/40 bg-warn-soft px-2 py-2">
      <p className="px-1.5 text-[11.5px] font-semibold text-warn">{needsYouLabel(needing.length)}</p>
      <ul className="mt-0.5">
        {needing.map((session) => (
          <li key={session.id}>
            <button
              type="button"
              onClick={() => onOpenColony(session)}
              className="flex w-full cursor-pointer items-center gap-2 rounded-lg px-1.5 py-1 text-left hover:bg-panel-2"
            >
              <span className="min-w-0 flex-1 truncate font-mono text-[12px] text-muted">{colonyLabel(session.repo, session.issue)}</span>
              <StatusBadge status={session.status} />
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** The image download as one quiet line under the status dots. It shows only while the pull
 *  runs or has failed — done and cached need no attention — so it never becomes furniture.
 *  Reads App's shared poller, so it keeps counting after Setup closes. */
function PullIndicator({ pull }: { pull: ImagePull }) {
  const { status, error } = pull;
  // Re-render once a second while pulling so the elapsed time moves.
  const [, tick] = useState(0);
  useEffect(() => {
    if (status?.state !== "pulling") return;
    const t = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(t);
  }, [status?.state]);

  if (error || status?.state === "failed") {
    return (
      <button
        type="button"
        onClick={() => void pull.start()}
        className="mx-4 flex w-[calc(100%-2rem)] cursor-pointer items-center gap-2 py-1 text-left text-[12px] text-err [overflow-wrap:anywhere]"
      >
        <span className="size-1.5 shrink-0 rounded-full bg-err" />
        <span className="min-w-0 flex-1">
          Image download failed{status?.error ? `: ${status.error}` : error ? `: ${error}` : ""} — a colony will retry at boot; click to retry now.
        </span>
      </button>
    );
  }
  if (status?.state !== "pulling") return null;
  return (
    <div className="mx-4 flex items-center gap-2 py-1 text-[12px] text-muted" role="status">
      <Spinner className="size-3" />
      <span className="min-w-0 truncate">
        Downloading <span className="font-mono">{status.image}</span> · {seconds(status.started_at)}s
      </span>
    </div>
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
        const sessionSpend = sessionCost(session);
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
                {sessionSpend != null && <span>· {formatCost(sessionSpend)}</span>}
                {session.parent && <span>· stacked</span>}
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

/** The colony launcher: pick a repository, pick issues, send them out. The cockpit's launch view renders it too. */
export function NewSession({
  org,
  githubConnected,
  statusKnown,
  onOpenSettings,
  autopilotDefault,
  sessions = [],
  onOpenColony,
  onCreated,
}: {
  org: string | null;
  githubConnected: boolean;
  statusKnown: boolean;
  onOpenSettings: () => void;
  autopilotDefault: boolean;
  /** The mothership's colony list, for the pre-submit duplicate check (`heldByFor`). */
  sessions?: Session[];
  /** Opens a colony holding an issue, from that issue's inline warning. */
  onOpenColony?: (session: Session) => void;
  onCreated: (session: Session) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [repos, setRepos] = useState<Repo[] | null>(null);
  const [reposError, setReposError] = useState<string | null>(null);
  const [repoQuery, setRepoQuery] = useState("");
  const [repo, setRepo] = useState<string | null>(() => stored("colonizer.repo"));
  const [picking, setPicking] = useState(() => !stored("colonizer.repo"));
  const [issues, setIssues] = useState<Issue[] | null>(null);
  const [issuesError, setIssuesError] = useState<string | null>(null);
  const [issueQuery, setIssueQuery] = useState("");
  const [openIssue, setOpenIssue] = useState<number | null>(null);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [allowDuplicate, setAllowDuplicate] = useState(false);
  const [blockedByDuplicate, setBlockedByDuplicate] = useState(false);
  const [launching, setLaunching] = useState(false);

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
    setSelected(new Set());
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

  const toggleSelected = (number: number, on: boolean) =>
    setSelected((current) => {
      const next = new Set(current);
      if (on) {
        next.add(number);
      } else {
        next.delete(number);
      }
      return next;
    });

  // Selected issues another colony already holds. They are skipped rather than sent, since the
  // mothership would refuse each with a 409 — unless Allow duplicate says to start them anyway.
  const heldSelected = activeRepo ? heldInBatch(sessions, activeRepo, selected) : [];
  const launchCount = allowDuplicate ? selected.size : selected.size - heldSelected.length;

  // One colony per issue, launched in the order they appear. Past the parallel limit the harness
  // queues them, so a batch is a plan rather than a burst.
  const launchSelected = async () => {
    if (!activeRepo) return;
    const held = new Set(allowDuplicate ? [] : heldInBatch(sessions, activeRepo, selected));
    const batch = matchingIssues.filter((i) => selected.has(i.number) && !held.has(i.number));
    setLaunching(true);
    setBlockedByDuplicate(false);
    let started = 0;
    let queued = 0;
    const failures: string[] = [];
    let last: Session | null = null;
    for (const issue of batch) {
      try {
        const session = await api.createSession({
          repo: activeRepo,
          issue: issue.number,
          title: issue.title,
          autopilot: autopilotDefault,
          allow_duplicate: allowDuplicate || undefined,
        });
        if (session.status === "queued") {
          queued += 1;
        } else {
          started += 1;
        }
        last = session;
      } catch (error) {
        // A 409 names the colony already holding the issue; the fix is the override below, not a retry.
        if (error instanceof ApiError && error.status === 409) setBlockedByDuplicate(true);
        failures.push(`#${issue.number}: ${errorMessage(error)}`);
      }
    }
    setLaunching(false);
    setSelected(new Set());
    const summary = [
      started > 0 ? `${started} started` : null,
      queued > 0 ? `${queued} queued` : null,
      held.size > 0 ? `${held.size} already held, skipped` : null,
    ]
      .filter(Boolean)
      .join(", ");
    if (failures.length > 0) {
      toast(`${summary || "Nothing launched"} · ${failures.length} failed — ${failures[0]}`, "error");
    } else {
      toast(`${batch.length} ${batch.length === 1 ? "colony" : "colonies"}: ${summary || "nothing launched"}`);
    }
    // The rest arrive with the sidebar's next poll; this one opens so there is something to watch.
    if (last) onCreated(last);
  };

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
          {selected.size > 0 && (
            <div className="space-y-1.5 rounded-xl border border-border bg-panel px-2.5 py-2 shadow-[var(--shadow)]">
              <div className="flex items-center gap-2">
                <span className="min-w-0 flex-1 text-[12.5px]">
                  {selected.size} selected
                  <span className="block text-[11.5px] text-faint">one colony each, queued past the limit</span>
                  {heldSelected.length > 0 && (
                    <span className="block text-[11.5px] text-warn">
                      {heldSelected.length} already held — skipped unless Allow duplicate
                    </span>
                  )}
                </span>
                <Button size="sm" variant="ghost" disabled={launching} onClick={() => setSelected(new Set())}>
                  Clear
                </Button>
                <Button size="sm" variant="primary" disabled={launching || launchCount === 0} onClick={launchSelected}>
                  {launching ? <Spinner /> : <IconPlus size={14} />} Launch {launchCount}
                </Button>
              </div>
              <label className="flex cursor-pointer items-center gap-2 px-0.5 text-[12px] text-muted">
                <input
                  type="checkbox"
                  checked={allowDuplicate}
                  onChange={(e) => setAllowDuplicate(e.target.checked)}
                  aria-label="Allow a second colony on an issue another colony already holds"
                  className="size-3.5 cursor-pointer accent-[var(--accent)]"
                />
                Allow duplicate — start even where another colony already holds the issue
              </label>
              {blockedByDuplicate && !allowDuplicate && (
                <p className="px-0.5 text-[12px] text-warn">
                  A colony already holds one of these issues — check Allow duplicate to launch anyway.
                </p>
              )}
            </div>
          )}
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
                selected={selected.has(issue.number)}
                onSelect={(on) => toggleSelected(issue.number, on)}
                open={openIssue === issue.number}
                onToggle={() => setOpenIssue(openIssue === issue.number ? null : issue.number)}
                holder={heldByFor(sessions, activeRepo, issue.number)}
                allowDuplicate={allowDuplicate}
                onAllowDuplicate={setAllowDuplicate}
                onOpenColony={onOpenColony}
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
      toast(session.status === "queued" ? `Queued on ${repo} — it starts when a colony finishes` : `Colony launched on ${repo}`);
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
  selected,
  onSelect,
  open,
  onToggle,
  holder,
  allowDuplicate,
  onAllowDuplicate,
  onOpenColony,
  onCreated,
}: {
  repo: string;
  issue: Issue;
  autopilotDefault: boolean;
  selected: boolean;
  onSelect: (on: boolean) => void;
  open: boolean;
  onToggle: () => void;
  /** The colony already holding this issue, if any — launching without the override answers 409. */
  holder: Session | null;
  allowDuplicate: boolean;
  onAllowDuplicate: (on: boolean) => void;
  onOpenColony?: (session: Session) => void;
  onCreated: (session: Session) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [instructions, setInstructions] = useState("");
  const [autopilot, setAutopilot] = useState<boolean | null>(null);
  const [starting, setStarting] = useState(false);
  const [launchError, setLaunchError] = useState<string | null>(null);

  const start = async () => {
    setStarting(true);
    setLaunchError(null);
    try {
      const session = await api.createSession({
        repo,
        issue: issue.number,
        title: issue.title,
        instructions: instructions.trim() || undefined,
        autopilot: autopilot ?? undefined,
        allow_duplicate: allowDuplicate || undefined,
      });
      toast(
        session.status === "queued"
          ? `#${issue.number} is queued — it starts when a colony finishes`
          : `Colony launched for #${issue.number}`,
      );
      onCreated(session);
    } catch (error) {
      // A 409 names the holder; keep its message on screen so the override below reads as the fix.
      if (error instanceof ApiError && error.status === 409) setLaunchError(errorMessage(error));
      else toast(errorMessage(error), "error");
    } finally {
      setStarting(false);
    }
  };

  return (
    <li
      className={cx(
        "rounded-xl border transition-colors",
        open || selected ? "border-border bg-panel shadow-[var(--shadow)]" : "border-transparent hover:bg-panel-2",
      )}
    >
      <div className="flex items-start">
        <label className="flex cursor-pointer items-start py-2.5 pl-3" title="Select for a batch launch">
          <input
            type="checkbox"
            checked={selected}
            onChange={(e) => onSelect(e.target.checked)}
            aria-label={`Select issue #${issue.number} for a batch launch`}
            className="mt-1 size-3.5 cursor-pointer accent-[var(--accent)]"
          />
        </label>
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          className="flex min-w-0 flex-1 cursor-pointer items-start gap-2 py-2.5 pl-2 pr-3 text-left"
        >
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
              {holder && (
                <span className="text-warn" title={`Already held by ${holder.id} (${holder.status})`}>
                  held by {holder.id}
                </span>
              )}
            </span>
          </span>
          <IconChevron size={14} className={cx("mt-1 shrink-0 text-faint transition-transform", open && "rotate-90")} />
        </button>
      </div>
      {open && (
        <div className="space-y-3 border-t border-border px-3 pb-3 pt-2.5">
          {issue.body?.trim() && (
            <p className="line-clamp-6 whitespace-pre-wrap text-[13px] text-muted [overflow-wrap:anywhere]">{issue.body.trim()}</p>
          )}
          <a href={issue.url} target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1 text-[12.5px] text-accent hover:underline">
            View on GitHub <IconExternal size={12} />
          </a>
          {holder && (
            <p className="rounded-lg border border-warn/40 bg-warn-soft px-2.5 py-2 text-[12.5px] text-muted">
              Already held by{" "}
              {onOpenColony ? (
                <button
                  type="button"
                  onClick={() => onOpenColony(holder)}
                  className="cursor-pointer font-mono text-accent hover:underline"
                >
                  {holder.id}
                </button>
              ) : (
                <span className="font-mono">{holder.id}</span>
              )}{" "}
              ({holder.status.replace(/_/g, " ")}
              {holder.pr_url ? (
                <>
                  ,{" "}
                  <a href={holder.pr_url} target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">
                    PR
                  </a>
                </>
              ) : (
                ", no PR yet"
              )}
              ).
            </p>
          )}
          {(holder || launchError) && (
            <label className="flex cursor-pointer items-center gap-2 text-[12.5px] text-muted">
              <input
                type="checkbox"
                checked={allowDuplicate}
                onChange={(e) => onAllowDuplicate(e.target.checked)}
                aria-label={`Allow a second colony on issue #${issue.number}`}
                className="size-3.5 cursor-pointer accent-[var(--accent)]"
              />
              Allow duplicate on #{issue.number}
            </label>
          )}
          {launchError && <p className="text-[12.5px] text-err [overflow-wrap:anywhere]">{launchError}</p>}
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
