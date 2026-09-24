// One org's red-team history (Workspaces table → history, or the wizard's History link): its
// schedules, which can be paused or removed, and every run it has had — live ones first, with what
// they found and what they cost. Replaces the Overview's old red-team card.
import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { Button, Spinner, cx, sameOrg } from "../components/ui";
import { formatCost } from "../spend";
import type { NewRedTeamSchedule, RedTeamRun, RedTeamSchedule, Session } from "../types";
import { describeCadence, runCost } from "./redTeamPlan";

const ACTIVE = new Set(["armed", "waiting", "running", "draining"]);

export function RedTeamHistory({
  org,
  open,
  sessions,
  runs,
  onClose,
  onStop,
  onOpenColony,
  onNew,
}: {
  org: string | null;
  open: boolean;
  sessions: Session[];
  runs: RedTeamRun[];
  onClose: () => void;
  onStop?: (id: string) => Promise<void>;
  onOpenColony: (id: string) => void;
  /** Opens the wizard for this org. */
  onNew: (org: string) => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal?.();
    else if (!open && dialog.open) dialog.close();
  }, [open]);
  return (
    <dialog
      ref={ref}
      onClose={onClose}
      aria-labelledby="redteam-history-title"
      className="m-auto w-[min(760px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50"
    >
      {open && org && (
        <HistoryBody org={org} sessions={sessions} runs={runs} onClose={onClose} onStop={onStop} onOpenColony={onOpenColony} onNew={onNew} />
      )}
    </dialog>
  );
}

export function HistoryBody({
  org,
  sessions,
  runs,
  onClose,
  onStop,
  onOpenColony,
  onNew,
  initialSchedules = null,
}: {
  org: string;
  sessions: Session[];
  runs: RedTeamRun[];
  onClose: () => void;
  onStop?: (id: string) => Promise<void>;
  onOpenColony: (id: string) => void;
  onNew: (org: string) => void;
  /** Tests pass the schedules in, since static markup never runs the fetch. */
  initialSchedules?: RedTeamSchedule[] | null;
}) {
  const api = useApi();
  const toast = useToast();
  const [schedules, setSchedules] = useState<RedTeamSchedule[] | null>(initialSchedules);
  const [pending, setPending] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .redTeamSchedules()
      .then((list) => setSchedules(list.filter((s) => sameOrg(s.org, org))))
      .catch(() => setSchedules([]));
  }, [api, org]);
  useEffect(() => {
    if (initialSchedules === null) load();
  }, [load, initialSchedules]);

  const mine = runs
    .filter((r) => sameOrg(r.org, org))
    .sort((a, b) => Number(ACTIVE.has(b.state)) - Number(ACTIVE.has(a.state)) || b.created_at.localeCompare(a.created_at));
  const live = mine.filter((r) => ACTIVE.has(r.state)).length;
  const spent = mine.reduce((total, r) => total + (runCost(r, sessions) ?? 0), 0);

  const act = async (id: string, what: () => Promise<unknown>) => {
    setPending(id);
    try {
      await what();
      load();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setPending(null);
    }
  };
  const toggle = (s: RedTeamSchedule) => {
    const body: NewRedTeamSchedule = {
      org: s.org,
      repos: s.repos,
      hunter: s.hunter,
      swarm_size: s.swarm_size,
      model: s.model,
      subagent_model: s.subagent_model,
      autofix: s.autofix,
      cadence: s.cadence,
      enabled: !s.enabled,
    };
    return act(s.id, () => api.updateRedTeamSchedule(s.id, body));
  };

  return (
    <div className="flex max-h-[calc(100dvh-24px)] flex-col">
      <div className="flex shrink-0 items-start gap-3 border-b border-border px-5 py-4">
        <div className="min-w-0 flex-1">
          <h2 id="redteam-history-title" className="text-[16px] font-semibold">
            Red-team history · {org}
          </h2>
          <p className="mt-0.5 text-[12.5px] tabular-nums text-muted">
            {mine.length} {mine.length === 1 ? "run" : "runs"} · {live} live · {formatCost(spent)} spent
          </p>
        </div>
        <Button variant="primary" onClick={() => onNew(org)}>
          New red team
        </Button>
        <button type="button" aria-label="Close red-team history" onClick={onClose} className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
          ✕
        </button>
      </div>

      <div className="scroll-thin min-h-0 flex-1 space-y-6 overflow-y-auto px-5 py-4">
        <section aria-label="schedules">
          <h3 className="mb-2 text-[11.5px] font-semibold uppercase tracking-wide text-faint">Schedules</h3>
          {schedules === null ? (
            <p className="flex items-center gap-2 text-[13px] text-muted">
              <Spinner /> Loading…
            </p>
          ) : schedules.length === 0 ? (
            <p className="text-[13px] text-muted">No schedules. Pick Weekly or Monthly in the wizard to add one.</p>
          ) : (
            <ul className="divide-y divide-border rounded-xl border border-border">
              {schedules.map((s) => (
                <li key={s.id} className={cx("flex flex-wrap items-center gap-x-4 gap-y-1 px-3.5 py-3", !s.enabled && "opacity-60")}>
                  <div className="min-w-0 flex-1 basis-60">
                    <div className="text-[13.5px] font-medium">{describeCadence(s.cadence)}</div>
                    <div className="truncate text-[12px] text-muted">
                      {s.repos.map((r) => r.split("/")[1]).join(", ")} · {s.swarm_size} hunters{s.model ? ` · ${s.model}` : ""}
                      {s.autofix ? " · autofix" : ""}
                    </div>
                    <div className="text-[12px] text-faint">
                      {s.enabled ? `Next ${new Date(s.next_run_at).toLocaleString()}` : "Paused"}
                      {s.last_run_at ? ` · last ${new Date(s.last_run_at).toLocaleDateString()}` : ""}
                      {s.last_result ? ` · ${s.last_result}` : ""}
                    </div>
                  </div>
                  <Button size="sm" disabled={pending === s.id} onClick={() => toggle(s)}>
                    {s.enabled ? "Pause" : "Resume"}
                  </Button>
                  <Button size="sm" variant="ghost" disabled={pending === s.id} onClick={() => act(s.id, () => api.deleteRedTeamSchedule(s.id))}>
                    Delete
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section aria-label="runs">
          <h3 className="mb-2 text-[11.5px] font-semibold uppercase tracking-wide text-faint">Runs</h3>
          {mine.length === 0 ? (
            <p className="text-[13px] text-muted">No red-team runs for {org} yet.</p>
          ) : (
            <ul className="divide-y divide-border rounded-xl border border-border">
              {mine.map((r) => {
                const cost = runCost(r, sessions);
                const active = ACTIVE.has(r.state);
                return (
                  <li key={r.id} className="px-3.5 py-3">
                    <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                      <span className="text-[13.5px] font-medium">{r.repo.split("/")[1] ?? r.repo}</span>
                      <span className={cx("rounded-full px-2 py-px text-[11px] font-medium", active ? "bg-accent-soft text-accent" : r.state === "done" ? "bg-ok/15 text-ok" : "bg-panel-3 text-muted")}>{r.state}</span>
                      {r.schedule_id && <span className="text-[11.5px] text-faint">scheduled</span>}
                      <span className="ml-auto text-[12px] tabular-nums text-muted">{new Date(r.created_at).toLocaleString()}</span>
                    </div>
                    <div className="mt-1 flex flex-wrap items-center gap-x-4 gap-y-1 text-[12px] tabular-nums text-muted">
                      <span>{r.hunter === "swarm" || !r.hunter ? "Colony swarm" : r.hunter} · {r.hunters.length || r.swarm_size} hunters</span>
                      {(r.model || r.subagent_model) && <span>{[r.model, r.subagent_model].filter(Boolean).join(" / ")}</span>}
                      <span>
                        {r.counts.found} found · {r.counts.validated} validated · {r.counts.filed} filed{r.counts.rejected > 0 ? ` · ${r.counts.rejected} rejected` : ""}
                      </span>
                      <span>{cost != null ? formatCost(cost) : "—"}</span>
                      {r.gate_reason && <span className="text-warn">{r.gate_reason}</span>}
                    </div>
                    {(r.hunters.length > 0 || (active && onStop)) && (
                      <div className="mt-2 flex flex-wrap items-center gap-1.5">
                        {r.hunters.map((h, i) => (
                          <button
                            key={h.session_id}
                            type="button"
                            onClick={() => {
                              onClose();
                              onOpenColony(h.session_id);
                            }}
                            title={h.focus}
                            className="cursor-pointer rounded-full border border-border bg-transparent px-2 py-0.5 text-[11.5px] text-muted hover:text-text"
                          >
                            hunter {i + 1}
                          </button>
                        ))}
                        {active && onStop && (
                          <Button size="sm" variant="ghost" disabled={pending === r.id} onClick={() => act(r.id, () => onStop(r.id))} className="ml-auto">
                            Stop run
                          </Button>
                        )}
                      </div>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </section>
      </div>
    </div>
  );
}
