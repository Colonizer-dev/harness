// The red-team wizard (Workspaces table → Red team): pick who hunts and where, pick the models, then
// review the cost and choose once / weekly / monthly. A one-off run starts armed — it launches the next
// time no colony is live — and a schedule is saved for the mothership's once-a-minute loop to fire.
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { useModels } from "../useModels";
import { ModelPicker } from "../components/ModelPicker";
import { Button, Spinner, cx } from "../components/ui";
import { formatCost } from "../spend";
import type { HunterProbe, RedTeamRun, Repo, Session, StartRedTeamRunRequest } from "../types";
import { WEEKDAYS, estimateCost, toUtcCadence, type ScheduleChoice } from "./redTeamPlan";

type Step = 0 | 1 | 2;
const STEPS = ["Hunter", "Models", "Review"] as const;

/** The hunters a run can name. Only the swarm runs today; the external ones say why they cannot. */
const HUNTERS = [
  {
    id: "swarm",
    name: "Colony swarm",
    blurb: "Colonies split eight focus areas and hunt for reproducible bugs, each with its own brief.",
  },
  { id: "strix", name: "Strix", blurb: "Open-source AI pentesting agents that validate findings with working PoCs." },
  { id: "shannon", name: "Shannon", blurb: "Keygraph's AI pentester for web apps and APIs." },
] as const;

export function RedTeamWizard({
  org,
  open,
  sessions,
  runs,
  onClose,
  onDone,
  onOpenHistory,
  onStart,
}: {
  org: string | null;
  open: boolean;
  /** App's start, which also drops the new run into its list; the api call when absent. */
  onStart?: (body: StartRedTeamRunRequest) => Promise<void>;
  sessions: Session[];
  runs: RedTeamRun[];
  onClose: () => void;
  /** After a start or a schedule save, so the caller can refresh its runs. */
  onDone: () => void;
  onOpenHistory: (org: string) => void;
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
      aria-labelledby="redteam-wizard-title"
      className="m-auto w-[min(620px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50"
    >
      {open && org && <WizardBody key={org} org={org} sessions={sessions} runs={runs} onClose={onClose} onDone={onDone} onOpenHistory={onOpenHistory} onStart={onStart} />}
    </dialog>
  );
}

export function WizardBody({
  org,
  sessions,
  runs,
  onClose,
  onDone,
  onOpenHistory,
  onStart,
  initialStep = 0,
}: {
  org: string;
  onStart?: (body: StartRedTeamRunRequest) => Promise<void>;
  sessions: Session[];
  runs: RedTeamRun[];
  onClose: () => void;
  onDone: () => void;
  onOpenHistory: (org: string) => void;
  /** Tests pin a step through it, since static markup cannot click. */
  initialStep?: Step;
}) {
  const api = useApi();
  const toast = useToast();
  const models = useModels();
  const [step, setStep] = useState<Step>(initialStep);
  const [repos, setRepos] = useState<Repo[] | null>(null);
  const [picked, setPicked] = useState<string[]>([]);
  const [probes, setProbes] = useState<Record<string, HunterProbe | null>>({});
  const [model, setModel] = useState("");
  const [subagentModel, setSubagentModel] = useState("");
  const [swarm, setSwarm] = useState(3);
  const [autofix, setAutofix] = useState(false);
  const [schedule, setSchedule] = useState<ScheduleChoice>({ every: "once" });
  const [busy, setBusy] = useState(false);
  // Read once when the repositories arrive: a poll's fresh session list must not reset the picks.
  const sessionsAtOpen = useRef(sessions);

  // The org's repositories, most active first; the one with the most colonies starts picked.
  useEffect(() => {
    let cancelled = false;
    api
      .repos()
      .then((list) => {
        if (cancelled) return;
        const mine = list.filter((r) => !r.archived && r.full_name.split("/")[0]?.toLowerCase() === org.toLowerCase());
        const activity = (repo: string) => sessionsAtOpen.current.filter((s) => s.repo === repo).length;
        mine.sort((a, b) => activity(b.full_name) - activity(a.full_name) || (b.pushed_at ?? "").localeCompare(a.pushed_at ?? ""));
        setRepos(mine);
        setPicked(mine.slice(0, 1).map((r) => r.full_name));
      })
      .catch(() => !cancelled && setRepos([]));
    for (const id of ["strix", "shannon"]) {
      api
        .probeHunter(id)
        .then((p) => !cancelled && setProbes((all) => ({ ...all, [id]: p })))
        .catch(() => !cancelled && setProbes((all) => ({ ...all, [id]: null })));
    }
    return () => {
      cancelled = true;
    };
  }, [api, org]);

  const estimate = useMemo(() => estimateCost(runs, sessions, swarm, Math.max(1, picked.length)), [runs, sessions, swarm, picked.length]);
  const hunters = swarm * picked.length;
  const canNext = step === 0 ? picked.length > 0 : true;

  const submit = async () => {
    setBusy(true);
    try {
      const cadence = toUtcCadence(schedule);
      const shared = { hunter: "swarm", model: model || null, subagent_model: subagentModel || null, swarm_size: swarm, autofix };
      if (cadence) {
        await api.createRedTeamSchedule({ org, repos: picked, cadence, ...shared });
        toast(`Red team scheduled for ${picked.length} ${picked.length === 1 ? "repository" : "repositories"} in ${org}`);
      } else {
        const failed: string[] = [];
        for (const repo of picked) {
          try {
            const body: StartRedTeamRunRequest = { repo, arm: true, ...shared };
            if (onStart) await onStart(body);
            else await api.startRedTeamRun(body);
          } catch (e) {
            failed.push(`${repo}: ${errorMessage(e)}`);
          }
        }
        if (failed.length > 0) toast(failed.join("; "), "error");
        if (failed.length < picked.length) toast(`Red team armed: it starts as soon as no colony is live`);
      }
      onDone();
      onClose();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex max-h-[calc(100dvh-24px)] flex-col">
      <div className="shrink-0 border-b border-border px-5 pb-3 pt-4">
        <div className="flex items-center gap-2">
          <h2 id="redteam-wizard-title" className="min-w-0 flex-1 text-[16px] font-semibold">
            Red team · {org}
          </h2>
          <button type="button" onClick={() => onOpenHistory(org)} className="cursor-pointer rounded-md border-0 bg-transparent px-2 py-1 text-[12.5px] text-muted hover:bg-panel-2 hover:text-text">
            History
          </button>
          <button type="button" aria-label="Close red-team wizard" onClick={onClose} className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
            ✕
          </button>
        </div>
        <ol className="mt-3 flex items-center gap-2 text-[12px]" aria-label="steps">
          {STEPS.map((label, i) => (
            <li key={label} aria-current={step === i ? "step" : undefined} className={cx("flex items-center gap-1.5", step === i ? "text-text" : i < step ? "text-muted" : "text-faint")}>
              <span className={cx("grid size-5 place-items-center rounded-full border text-[11px] tabular-nums", step === i ? "border-accent bg-accent text-on-accent" : "border-border")}>{i + 1}</span>
              {label}
              {i < STEPS.length - 1 && <span aria-hidden="true" className="mx-1 h-px w-6 bg-border" />}
            </li>
          ))}
        </ol>
      </div>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        {step === 0 && (
          <div className="space-y-5">
            <Field label="Who hunts">
              <div className="grid gap-2 sm:grid-cols-3">
                {HUNTERS.map((h) => {
                  const runnable = h.id === "swarm";
                  const probe = probes[h.id];
                  const note = runnable
                    ? "Ready"
                    : h.id === "shannon" || probe?.manifest.available === false
                      ? "Coming soon"
                      : probe?.installed
                        ? "Installed · runs coming soon"
                        : "Coming soon";
                  return (
                    <button
                      key={h.id}
                      type="button"
                      aria-pressed={runnable}
                      disabled={!runnable}
                      title={runnable ? undefined : `${h.name} installs and probes, but red-team runs do not drive its scans yet${probe ? ` — ${probe.probe.detail}` : ""}`}
                      className={cx(
                        "flex flex-col gap-1 rounded-xl border p-3 text-left",
                        runnable ? "cursor-default border-accent bg-accent-soft" : "cursor-not-allowed border-border bg-panel-2 opacity-70",
                      )}
                    >
                      <span className="flex items-center justify-between gap-2 text-[13.5px] font-semibold">
                        {h.name}
                        <span className={cx("rounded-full px-1.5 py-px text-[10.5px] font-medium", runnable ? "bg-ok/15 text-ok" : "bg-panel-3 text-muted")}>{note}</span>
                      </span>
                      <span className="text-[12px] leading-snug text-muted">{h.blurb}</span>
                    </button>
                  );
                })}
              </div>
            </Field>
            <Field label="Repositories" hint="One run per repository. A repository that already has an active run is skipped.">
              {repos === null ? (
                <p className="flex items-center gap-2 text-[13px] text-muted">
                  <Spinner /> Loading repositories…
                </p>
              ) : repos.length === 0 ? (
                <p className="text-[13px] text-muted">No repositories found for {org}.</p>
              ) : (
                <div className="max-h-48 space-y-0.5 overflow-y-auto rounded-lg border border-border p-1.5 scroll-thin">
                  <label className="flex cursor-pointer items-center gap-2 rounded-md px-2 py-1 text-[12.5px] text-muted hover:bg-panel-2">
                    <input type="checkbox" checked={picked.length === repos.length} onChange={(e) => setPicked(e.target.checked ? repos.map((r) => r.full_name) : [])} />
                    All {repos.length}
                  </label>
                  {repos.map((r) => (
                    <label key={r.full_name} className="flex cursor-pointer items-center gap-2 rounded-md px-2 py-1 text-[13px] hover:bg-panel-2">
                      <input
                        type="checkbox"
                        checked={picked.includes(r.full_name)}
                        onChange={(e) => setPicked((p) => (e.target.checked ? [...p, r.full_name] : p.filter((x) => x !== r.full_name)))}
                      />
                      <span className="min-w-0 flex-1 truncate">{r.full_name.split("/")[1]}</span>
                      <span className="text-[11.5px] tabular-nums text-faint">{sessions.filter((s) => s.repo === r.full_name).length} colonies</span>
                    </label>
                  ))}
                </div>
              )}
            </Field>
          </div>
        )}

        {step === 1 && (
          <div className="space-y-5">
            <Field label="Hunter model" hint="Each hunter's orchestrator. Default follows the agent module's routing.">
              <ModelPicker value={model} onChange={setModel} models={models} emptyLabel="Agent default" ariaLabel="Hunter model" />
            </Field>
            <Field label="Subagent model" hint="What the hunters delegate reading and reproduction to.">
              <ModelPicker value={subagentModel} onChange={setSubagentModel} models={models} emptyLabel="Agent default" ariaLabel="Subagent model" />
            </Field>
            <Field label={`Hunters per repository: ${swarm}`} hint="Each hunter is a full colony with its own microVM and focus area.">
              <input type="range" min={1} max={8} value={swarm} onChange={(e) => setSwarm(Number(e.target.value))} aria-label="Hunters per repository" className="w-full accent-[var(--accent)]" />
            </Field>
          </div>
        )}

        {step === 2 && (
          <div className="space-y-5">
            <div role="note" className="rounded-xl border border-warn/40 bg-warn/10 p-3.5 text-[13px] leading-snug">
              <p className="font-semibold text-warn">Red-team runs are expensive</p>
              <p className="mt-1 text-text">
                This starts {hunters} autonomous {hunters === 1 ? "colony" : "colonies"} ({swarm} per repository × {picked.length}), each in long sessions reading and reproducing code
                {schedule.every === "once" ? "" : `, and repeats ${schedule.every === "weekly" ? "every week" : "every month"} until you switch the schedule off`}.
              </p>
              <p className="mt-1 text-muted">
                {estimate
                  ? `Expect roughly ${formatCost(estimate.low)} or more per ${schedule.every === "once" ? "run" : "firing"}, from your ${estimate.basis === "runs" ? "past red-team runs" : "average colony spend"}.`
                  : "No spend history yet to estimate from — watch the first run's cost before scheduling more."}
              </p>
            </div>
            <label className="flex cursor-pointer items-start gap-2.5 text-[13px]">
              <input type="checkbox" checked={autofix} onChange={(e) => setAutofix(e.target.checked)} className="mt-0.5" />
              <span>
                <span className="font-medium">Let hunters fix what they find</span>
                <span className="block text-[12px] text-muted">Off: hunters only report findings and never open, merge or change anything. On costs more and opens pull requests.</span>
              </span>
            </label>
            <Field label="When">
              <div role="radiogroup" aria-label="When" className="flex flex-wrap gap-1.5">
                {(["once", "weekly", "monthly"] as const).map((every) => (
                  <button
                    key={every}
                    type="button"
                    role="radio"
                    aria-checked={schedule.every === every}
                    onClick={() => setSchedule(every === "once" ? { every } : every === "weekly" ? { every, weekday: 0, time: "02:00" } : { every, day: 1, time: "02:00" })}
                    className={cx("cursor-pointer rounded-full border px-3 py-1 text-[12.5px]", schedule.every === every ? "border-text bg-panel-3 text-text" : "border-border bg-transparent text-muted hover:text-text")}
                  >
                    {every === "once" ? "Once, now" : every === "weekly" ? "Weekly" : "Monthly"}
                  </button>
                ))}
              </div>
              {schedule.every === "once" && <p className="mt-2 text-[12px] text-muted">Starts as soon as no colony is live.</p>}
              {schedule.every === "weekly" && (
                <div className="mt-2 flex items-center gap-2 text-[13px]">
                  <select aria-label="Weekday" value={schedule.weekday} onChange={(e) => setSchedule({ ...schedule, weekday: Number(e.target.value) })} className="rounded-md border border-border bg-panel px-2 py-1">
                    {WEEKDAYS.map((d, i) => (
                      <option key={d} value={i}>
                        {d}
                      </option>
                    ))}
                  </select>
                  at
                  <input aria-label="Time" type="time" value={schedule.time} onChange={(e) => setSchedule({ ...schedule, time: e.target.value })} className="rounded-md border border-border bg-panel px-2 py-1" />
                  <span className="text-[12px] text-muted">your time</span>
                </div>
              )}
              {schedule.every === "monthly" && (
                <div className="mt-2 flex items-center gap-2 text-[13px]">
                  day
                  <select aria-label="Day of month" value={schedule.day} onChange={(e) => setSchedule({ ...schedule, day: Number(e.target.value) })} className="rounded-md border border-border bg-panel px-2 py-1">
                    {Array.from({ length: 31 }, (_, i) => i + 1).map((d) => (
                      <option key={d} value={d}>
                        {d}
                      </option>
                    ))}
                  </select>
                  at
                  <input aria-label="Time" type="time" value={schedule.time} onChange={(e) => setSchedule({ ...schedule, time: e.target.value })} className="rounded-md border border-border bg-panel px-2 py-1" />
                  <span className="text-[12px] text-muted">{schedule.day > 28 ? "the month's last day when shorter" : "your time"}</span>
                </div>
              )}
            </Field>
          </div>
        )}
      </div>

      <div className="flex shrink-0 items-center gap-2 border-t border-border px-5 py-3">
        <span className="mr-auto text-[12.5px] text-muted">
          {picked.length} {picked.length === 1 ? "repository" : "repositories"} · {hunters} {hunters === 1 ? "hunter" : "hunters"}
        </span>
        {step > 0 && <Button onClick={() => setStep((s) => (s - 1) as Step)}>Back</Button>}
        {step < 2 ? (
          <Button variant="primary" disabled={!canNext} onClick={() => setStep((s) => (s + 1) as Step)}>
            Next
          </Button>
        ) : (
          <Button variant="primary" disabled={busy || picked.length === 0} onClick={submit}>
            {busy && <Spinner />} {schedule.every === "once" ? "Start red team" : "Save schedule"}
          </Button>
        )}
      </div>
    </div>
  );
}

function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <section>
      <h3 className="text-[12.5px] font-semibold text-text">{label}</h3>
      {hint && <p className="mb-2 text-[12px] text-muted">{hint}</p>}
      {!hint && <div className="mb-2" />}
      {children}
    </section>
  );
}
