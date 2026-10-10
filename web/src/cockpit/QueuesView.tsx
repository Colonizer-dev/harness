// Queues (issue #1127): what is waiting to start, why each colony waits, and what you can do about
// it. One self-fetched list like the Inbox — filters live in the address next to `?org=` — plus a
// per-host capacity strip, and bulk resume/stop/restart that reports the server's refusals as they
// landed rather than pretending every call went through.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { Page } from "./Page";

import type { QueuesPayload, QueuesFilters, QueuesGroup, QueuesHost, QueuesRow } from "../features/queues/types";
import { groupRows, reasonLabel, reasonTone, repoKey, summarizeActions, type ActionResult, type ActionOutcome } from "../features/queues/model";
import { QUEUES_FILTER_KEYS, queuesFiltersFromSearch, queuesSearchFromFilters } from "../features/queues/url";
import { errorMessage, useApi, useToast } from "../context";
import { Badge, Button, Spinner, StatusBadge, timeAgo } from "../components/ui";
import type { Session } from "../types";
import { FilterSelect, SearchBox, Segmented, optionsBy } from "./ListControls";
import { currentLocation, navigate } from "../router";

/** How often the page re-reads the queue while it is open (the Inbox's own rhythm). */
const REFRESH_MS = 15_000;
/** The quiet window live session frames collapse into before the page re-reads (sessionStream's idea). */
const REFRESH_DEBOUNCE_MS = 400;

/**
 * The Queues page. `sessions` is the live colony list: the WebSocket keeps it fresher than any
 * poll, and every change to it nudges a re-read through a short quiet window. `initialFilters`
 * overrides reading the filters out of the address on first render (the static tests' way in), and
 * `initialPayload` seeds the page so a render with no effects still shows rows.
 */
export function QueuesView({
  sessions = [],
  initialFilters = null,
  initialPayload = null,
}: {
  /** Live colony frames (from App's WebSocket); changes to it refresh the queue. */
  sessions?: Session[];
  /** The filters the page starts with, instead of the address's own (tests, deep links). */
  initialFilters?: QueuesFilters | null;
  /** The payload the page starts with, before the first fetch lands (tests, static renders). */
  initialPayload?: QueuesPayload | null;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [payload, setPayload] = useState<QueuesPayload | null>(initialPayload);
  const [filters, setFilters] = useState<QueuesFilters>(() => initialFilters ?? queuesFiltersFromSearch(currentLocation().search));
  const [selected, setSelected] = useState<ReadonlySet<string>>(() => new Set());
  const [outcome, setOutcome] = useState<(ActionOutcome & { verb: "resume" | "stop" | "restart" }) | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    api.queues(filters).then(setPayload, (e) => toast(errorMessage(e), "error"));
  }, [api, toast, filters]);

  // Self-fetch like the Inbox: now, then on the quiet rhythm while the page is open.
  useEffect(() => {
    load();
    const timer = window.setInterval(load, REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [load]);

  // Live frames say a colony moved: re-read through the quiet window, so a burst of frames costs
  // one fetch. `load` is reached through a ref so a filter change does not schedule a second read.
  const loadRef = useRef(load);
  loadRef.current = load;
  const mounted = useRef(false);
  useEffect(() => {
    if (!mounted.current) {
      mounted.current = true;
      return;
    }
    const timer = window.setTimeout(() => loadRef.current(), REFRESH_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [sessions]);

  // Filters are state and address at once: every change rewrites this page's own keys in place,
  // leaving `?org=`, `?mock=1` and friends exactly as the route table left them.
  const setFiltersAndShare = (patch: Partial<QueuesFilters>) => {
    const next = { ...filters, ...patch };
    setFilters(next);
    const here = currentLocation();
    const params = new URLSearchParams(here.search);
    for (const key of QUEUES_FILTER_KEYS) params.delete(key);
    for (const [key, value] of new URLSearchParams(queuesSearchFromFilters(next))) params.set(key, value);
    const search = params.toString();
    navigate(`${here.pathname}${search ? `?${search}` : ""}${here.hash}`, { replace: true });
  };
  const setFilter = (key: keyof QueuesFilters, value: string) => setFiltersAndShare({ [key]: value || undefined });

  const rows = payload?.rows ?? [];

  const toggle = (id: string, on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });

  /** One action over the named colonies: settled per colony, refusals kept with the server's words. */
  const run = async (verb: "resume" | "stop" | "restart", ids: string[]) => {
    if (ids.length === 0) return;
    setBusy(true);
    let results: ActionResult[];
    if (verb === "restart") {
      // One call takes every id; the answer splits them into restarting and skipped-with-reason.
      try {
        const answer = await api.restartOnNewVersion({ ids });
        results = [
          ...answer.restarting.map((id) => ({ id, ok: true })),
          ...answer.skipped.map((skip) => ({ id: skip.id, ok: false, error: skip.reason })),
        ];
      } catch (e) {
        results = ids.map((id) => ({ id, ok: false, error: errorMessage(e) }));
      }
    } else {
      const press = (id: string) => (verb === "resume" ? api.resumeSession(id) : api.stopSession(id));
      const settled = await Promise.allSettled(ids.map(press));
      results = ids.map((id, at) => {
        const one = settled[at];
        return one.status === "fulfilled" ? { id, ok: true as const } : { id, ok: false as const, error: errorMessage(one.reason) };
      });
    }
    setOutcome({ ...summarizeActions(results), verb });
    setBusy(false);
    load();
  };

  /** The bulk bar's action: everything selected. */
  const bulk = (verb: "resume" | "stop" | "restart") => run(verb, [...selected]);

  const groups = useMemo(() => groupRows(rows, filters.group ?? "none"), [rows, filters.group]);
  const hosts = payload?.hosts ?? [];

  return (
    <Page width="wide" frameClassName="flex flex-col gap-5">
      <div className="mb-1">
        <h1 className="m-0 text-display-xl font-semibold leading-[1.15] tracking-[-0.035em]">Queues</h1>
        <div className="mt-2 text-body-lg text-muted">
          {payload ? `${payload.queue_depth} waiting · ` : ""}what's waiting, why, and what you can do about it
        </div>
      </div>

      {payload?.draining ? (
        <div
          role="status"
          className="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-md border border-warn bg-warn-soft px-3 py-2 text-small-lg text-warn"
        >
          Draining for an update or restart: new colonies stay queued until it finishes.
        </div>
      ) : null}
      {payload?.external_writes_blocked ? (
        <div role="status" className="rounded-md border border-border bg-panel px-3 py-2 text-small-lg text-muted">
          External writes are blocked, so colonies queue up but their publishes are refused until the block is lifted.
        </div>
      ) : null}

      {payload === null ? (
        <p className="flex items-center gap-2 text-body-sm text-muted">
          <Spinner /> Loading the queues…
        </p>
      ) : (
        <>
          <section aria-label="Hosts" className="flex flex-col gap-2">
            {hosts.map((host) => (
              <HostCard key={host.name} host={host} />
            ))}
          </section>

          <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
            <SearchBox label="search the queue" placeholder="search" value={filters.q ?? ""} onChange={(v) => setFilter("q", v)} className="w-full sm:w-52" />
            <FilterSelect label="host" allLabel="all hosts" value={filters.host ?? "all"} onChange={(v) => setFilter("host", v)} options={optionsBy(hosts, (h) => h.name)} />
            <FilterSelect label="reason" allLabel="all reasons" value={filters.reason ?? "all"} onChange={(v) => setFilter("reason", v)} options={optionsBy(rows, (r) => r.reason)} />
            <FilterSelect label="repo" allLabel="all repos" value={filters.repo ?? "all"} onChange={(v) => setFilter("repo", v)} options={optionsBy(rows, (r) => repoKey(r))} />
            <FilterSelect label="agent" allLabel="all agents" value={filters.agent ?? "all"} onChange={(v) => setFilter("agent", v)} options={optionsBy(rows, (r) => r.agent)} />
            <span className="ml-auto" />
            <Segmented<QueuesGroup>
              label="group rows"
              value={filters.group ?? "none"}
              onChange={(group) => setFiltersAndShare({ group })}
              options={[
                { value: "none", label: "Flat" },
                { value: "host", label: "Host" },
                { value: "reason", label: "Reason" },
                { value: "repo", label: "Repo" },
              ]}
            />
          </div>

          {selected.size > 0 ? (
            <div className="flex flex-wrap items-center gap-2 rounded-lg border border-border bg-panel px-3 py-2">
              <span className="text-body-sm text-muted">{selected.size} selected</span>
              <Button size="sm" disabled={busy} onClick={() => void bulk("resume")}>
                Resume selected
              </Button>
              <Button size="sm" variant="danger" disabled={busy} onClick={() => void bulk("stop")}>
                Stop selected
              </Button>
              <Button size="sm" disabled={busy} onClick={() => void bulk("restart")}>
                Restart on new version
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setSelected(new Set())}>
                clear
              </Button>
            </div>
          ) : null}

          {outcome ? (
            <div role="status" className="flex flex-wrap items-center gap-x-3 gap-y-1 text-small text-muted">
              <span>
                {outcome.done.length > 0
                  ? `${OUTCOME_VERB[outcome.verb]} ${outcome.done.length} colon${outcome.done.length === 1 ? "y" : "ies"}`
                  : `Nothing ${OUTCOME_VERB[outcome.verb]}`}
                {outcome.refused.length > 0 ? " ·" : ""}
              </span>
              {outcome.refused.map((refusal) => (
                <span key={refusal.id} className="text-err">
                  {refusal.id}: {refusal.error}
                </span>
              ))}
              <button type="button" onClick={() => setOutcome(null)} className="cursor-pointer border-0 bg-transparent p-0 text-faint hover:text-text" aria-label="dismiss the results">
                ×
              </button>
            </div>
          ) : null}

          {rows.length === 0 ? (
            <div className="border-y border-border py-3.5 text-body-sm text-muted">nothing is queued — every colony is running, parked or done</div>
          ) : (
            groups.map((group) => (
              <section key={group.key} aria-label={group.label || undefined}>
                {filters.group && filters.group !== "none" ? (
                  <h2 className="mb-1 mt-4 text-title font-semibold text-text">
                    {group.label} <span className="font-normal text-faint">{group.rows.length}</span>
                  </h2>
                ) : null}
                <div>
                  {group.rows.map((row) => (
                    <QueueRow key={row.id} row={row} selected={selected.has(row.id)} onToggle={(on) => toggle(row.id, on)} onAction={(verb) => void run(verb, [row.id])} busy={busy} />
                  ))}
                </div>
              </section>
            ))
          )}
        </>
      )}
    </Page>
  );
}

/** One host: name, a slots-used bar against its ceiling, what waits on it, and how deep the queue is. */
function HostCard({ host }: { host: QueuesHost }): ReactElement {
  const pct = host.slots_ceiling > 0 ? Math.min(100, Math.round((host.slots_in_use * 100) / host.slots_ceiling)) : 100;
  const count = (label: string, n: number | null) => (n == null ? "—" : `${n} ${label}`);
  return (
    <div className={`flex flex-wrap items-center gap-x-4 gap-y-2 rounded-lg border px-3.5 py-2.5 ${host.over_ceiling ? "border-err bg-err-soft" : "border-border bg-panel"} ${host.reachable ? "" : "opacity-60"}`}>
      <span className="text-body font-medium text-text">{host.name}</span>
      {host.over_ceiling ? <Badge tone="err">over ceiling</Badge> : null}
      {!host.reachable ? <Badge>unreachable</Badge> : null}
      <div className="flex min-w-44 flex-1 items-center gap-2">
        <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-panel-3">
          <div className={`h-full rounded-full ${host.over_ceiling ? "bg-err" : "bg-ok"}`} style={{ width: `${pct}%` }} />
        </div>
        <span className="font-mono text-meta tabular-nums text-muted">
          {host.slots_in_use}/{host.slots_ceiling}
        </span>
      </div>
      <span className="text-meta text-muted">
        {count("queued", host.queued)} · {count("parked", host.parked)}
      </span>
      <span className="text-meta text-faint">depth {host.queue_depth}</span>
    </div>
  );
}

const COLONY_HREF = (id: string) => `/colonies/${encodeURIComponent(id)}`;

/** The row's name: `org/repo#issue`, or just `org/repo` for a colony launched without an issue. The
 *  server's `repo` may already carry the org, so the name goes through `repoKey` to avoid doubling it. */
const rowName = ({ org, repo, issue }: Pick<QueuesRow, "org" | "repo" | "issue">): string =>
  `${repoKey({ org, repo })}${issue == null ? "" : `#${issue}`}`;

/** What the results line says a bulk action did. */
const OUTCOME_VERB: Record<"resume" | "stop" | "restart", string> = {
  resume: "Resumed",
  stop: "Stopped",
  restart: "Restarting",
};

/** One waiting colony: select, the colony link and its badges, the why, and the actions it accepts. */
function QueueRow({
  row,
  selected,
  onToggle,
  onAction,
  busy,
}: {
  row: QueuesRow;
  selected: boolean;
  onToggle: (on: boolean) => void;
  onAction: (verb: "resume" | "stop" | "restart") => void;
  busy: boolean;
}): ReactElement {
  const href = COLONY_HREF(row.id);
  // A plain anchor so the address bar, middle-click and copy all work; the click itself stays in the SPA.
  const open = (e: { preventDefault: () => void }) => {
    e.preventDefault();
    navigate(href);
  };
  const held = row.actions.resume || row.actions.stop || row.actions.restart;
  const action = (verb: "resume" | "stop" | "restart", label: string, danger = false) =>
    row.actions[verb] ? (
      <Button size="sm" variant={danger ? "danger" : "secondary"} disabled={busy} onClick={() => onAction(verb)}>
        {label}
      </Button>
    ) : (
      <Button size="sm" variant={danger ? "danger" : "secondary"} disabled title={row.why_not[verb]}>
        {label}
      </Button>
    );
  return (
    <div className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-start gap-x-3 gap-y-1 border-b border-border py-2.5">
      <input type="checkbox" aria-label={`select ${rowName(row)}`} checked={selected} onChange={(e) => onToggle(e.target.checked)} className="mt-1.5 size-4 cursor-pointer accent-[var(--accent)]" />
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <a href={href} onClick={open} className="font-mono text-small-lg text-text underline decoration-border underline-offset-2 hover:decoration-text">
            {rowName(row)}
          </a>
          {row.title ? <span className="min-w-0 truncate text-body text-muted">{row.title}</span> : null}
          <StatusBadge session={{ status: row.status, suspended: null, pending_answer: null, prewarm: undefined, superseded: undefined }} />
          {row.held ? <Badge title="a merge superseded this colony; keep it to let it start">held · superseded</Badge> : null}
          {row.policy_hold ? <Badge tone="err">release policy hold</Badge> : null}
          {row.priority !== 0 ? (
            <span className="font-mono text-meta tabular-nums text-faint" title="queue priority, higher first">
              P{row.priority}
            </span>
          ) : null}
        </div>
        <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 font-mono text-meta text-faint">
          {row.branch ? <span className="truncate">{row.branch}</span> : null}
          <span>{timeAgo(row.created_at)}</span>
          {row.agent ? <span>{row.agent}</span> : null}
          {row.host ? <span>{row.host}</span> : null}
        </div>
        {row.detail ? <div className="mt-1 text-small text-muted">{row.detail}</div> : null}
      </div>
      <div className="flex flex-wrap items-center justify-end gap-2">
        <Badge tone={reasonTone(row)}>{reasonLabel(row)}</Badge>
        {held ? (
          <>
            {action("resume", "Resume")}
            {action("stop", "Stop", true)}
            {action("restart", "Restart")}
          </>
        ) : (
          <span className="text-meta text-faint" title={row.why_not.resume ?? row.why_not.stop ?? row.why_not.restart}>
            no action
          </span>
        )}
      </div>
    </div>
  );
}
