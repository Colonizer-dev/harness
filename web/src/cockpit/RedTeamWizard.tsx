// The red-team wizard (Workspaces table → Red team): pick who hunts and where, pick the models, then
// review the cost and choose once / weekly / monthly. A one-off run starts armed — it launches the next
// time no colony is live — and a schedule is saved for the mothership's once-a-minute loop to fire.
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { useModels } from "../useModels";
import { ModelPicker } from "../components/ModelPicker";
import { Button, Spinner, cx } from "../components/ui";
import { formatCost } from "../spend";
import type { HunterProbe, RedTeamPreset, RedTeamRun, Repo, Session, StartRedTeamRunRequest } from "../types";
import { PRESETS, WEEKDAYS, activeLine, activeRunFor, estimateCost, historyLine, plural, sortForRedTeam, toUtcCadence, type ScheduleChoice } from "./redTeamPlan";
import { CancelRunButton } from "./RedTeamCancel";
import { IconAnt } from "../components/icons";
import { HackerIcon } from "./HackerIcon";
import strixLogo from "../assets/hunters/strix.png";
import shannonLogo from "../assets/hunters/shannon.jpg";

type Step = 0 | 1 | 2;
const STEPS = ["Hunter", "Models", "Review"] as const;

/** The hunters a run can name. The swarm and Shannon run today; Strix says why it cannot. */
const HUNTERS = [
  {
    id: "swarm",
    name: "Colony swarm",
    blurb: "Colonies split eight focus areas and hunt for reproducible bugs, each with its own brief.",
    logo: null,
  },
  { id: "strix", name: "Strix", blurb: "Open-source AI pentesting agents that validate findings with working PoCs.", logo: strixLogo },
  { id: "shannon", name: "Shannon", blurb: "Keygraph's AI pentester for web apps and APIs.", logo: shannonLogo },
] as const;

type HunterId = (typeof HUNTERS)[number]["id"];

/** What a red team is, in three lines, at the top of the wizard's first step. */
function RedTeamIntro() {
  return (
    <div className="flex gap-3.5 rounded-xl border border-border bg-panel-2 p-4">
      <span className="grid size-11 shrink-0 place-items-center rounded-xl bg-err/10 text-err">
        <HackerIcon size={24} />
      </span>
      <div className="min-w-0 space-y-1.5 text-small-lg leading-snug text-muted">
        <p className="text-body-lg font-semibold text-text">What is a red team?</p>
        <p>
          A red team attacks your own code on purpose, the way an outsider would, so you find the holes first. Hunters read
          the repository, try real exploits inside sealed microVMs, and keep only findings they can reproduce.
        </p>
        <p>Nothing touches your branches unless you let hunters fix what they find, and every fix still arrives as a pull request.</p>
      </div>
    </div>
  );
}

export function RedTeamWizard({
  org,
  open,
  sessions,
  runs,
  onClose,
  onDone,
  onOpenHistory,
  onStart,
  onCancel,
}: {
  org: string | null;
  open: boolean;
  /** Cancels a run (the repository list's Cancel run button). */
  onCancel?: (id: string) => Promise<void>;
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
      {open && org && <WizardBody key={org} org={org} sessions={sessions} runs={runs} onClose={onClose} onDone={onDone} onOpenHistory={onOpenHistory} onStart={onStart} onCancel={onCancel} />}
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
  onCancel,
  initialStep = 0,
  initialPreset = "general",
  initialHunter = "swarm",
}: {
  org: string;
  onStart?: (body: StartRedTeamRunRequest) => Promise<void>;
  onCancel?: (id: string) => Promise<void>;
  sessions: Session[];
  runs: RedTeamRun[];
  onClose: () => void;
  onDone: () => void;
  onOpenHistory: (org: string) => void;
  /** Tests pin a step through it, since static markup cannot click. */
  initialStep?: Step;
  /** Tests pin the preset the same way. */
  initialPreset?: RedTeamPreset;
  /** Tests pin the selected hunter the same way. */
  initialHunter?: "swarm" | "shannon";
}) {
  const api = useApi();
  const toast = useToast();
  const models = useModels();
  const [step, setStep] = useState<Step>(initialStep);
  const [repos, setRepos] = useState<Repo[] | null>(null);
  const [pickedRaw, setPicked] = useState<string[]>([]);
  const [probes, setProbes] = useState<Record<string, HunterProbe | null>>({});
  const [model, setModel] = useState("");
  const [subagentModel, setSubagentModel] = useState("");
  const [swarm, setSwarm] = useState(3);
  const [hunter, setHunter] = useState<HunterId>(initialHunter);
  const [autofix, setAutofix] = useState(false);
  const [preset, setPreset] = useState<RedTeamPreset>(initialPreset);
  const [schedule, setSchedule] = useState<ScheduleChoice>({ every: "once" });
  const [busy, setBusy] = useState(false);
  // Read once when the repositories arrive: a poll's fresh run list must not reset the picks.
  const runsAtOpen = useRef(runs);

  // The org's repositories, most recently pushed first; the first one that is free starts picked.
  // The list's own order (never hunted first, oldest hunt next, active last) is applied at render.
  useEffect(() => {
    let cancelled = false;
    api
      .repos()
      .then((list) => {
        if (cancelled) return;
        const mine = list.filter((r) => !r.archived && r.full_name.split("/")[0]?.toLowerCase() === org.toLowerCase());
        mine.sort((a, b) => (b.pushed_at ?? "").localeCompare(a.pushed_at ?? ""));
        setRepos(mine);
        setPicked(sortForRedTeam(mine, runsAtOpen.current).filter((r) => !activeRunFor(runsAtOpen.current, r.full_name)).slice(0, 1).map((r) => r.full_name));
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

  // A repository that has an active run cannot be picked, even if it was when its run started.
  const picked = useMemo(() => pickedRaw.filter((r) => !activeRunFor(runs, r)), [pickedRaw, runs]);

  // Shannon runs one colony per repository whatever the picker says, so its swarm size is always one.
  const swarmSize = hunter === "shannon" ? 1 : swarm;
  const estimate = useMemo(() => estimateCost(runs, sessions, swarmSize, Math.max(1, picked.length)), [runs, sessions, swarmSize, picked.length]);
  const hunters = swarmSize * picked.length;
  const canNext = step === 0 ? picked.length > 0 : true;

  const submit = async () => {
    setBusy(true);
    try {
      const cadence = toUtcCadence(schedule);
      const shared = { hunter, preset, model: model || null, subagent_model: subagentModel || null, swarm_size: swarmSize, autofix };
      if (cadence) {
        await api.createRedTeamSchedule({ org, repos: picked, cadence, ...shared });
        toast(`Red team scheduled for ${plural(picked.length, "repository", "repositories")} in ${org}`);
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
          <h2 id="redteam-wizard-title" className="flex min-w-0 flex-1 items-center gap-2 text-title-sm font-semibold">
            <HackerIcon size={18} className="shrink-0 text-err" />
            <span className="truncate">Red team · {org}</span>
          </h2>
          <button type="button" onClick={() => onOpenHistory(org)} className="cursor-pointer rounded-md border-0 bg-transparent px-2 py-1 text-small-lg text-muted hover:bg-panel-2 hover:text-text">
            History
          </button>
          <button type="button" aria-label="Close red-team wizard" onClick={onClose} className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
            ✕
          </button>
        </div>
        <ol className="mt-3 flex items-center gap-2 text-small" aria-label="steps">
          {STEPS.map((label, i) => (
            <li key={label} aria-current={step === i ? "step" : undefined} className={cx("flex items-center gap-1.5", step === i ? "text-text" : i < step ? "text-muted" : "text-faint")}>
              <span className={cx("grid size-5 place-items-center rounded-full border text-meta tabular-nums", step === i ? "border-accent bg-accent text-on-accent" : "border-border")}>{i + 1}</span>
              {label}
              {i < STEPS.length - 1 && <span aria-hidden="true" className="mx-1 h-px w-6 bg-border" />}
            </li>
          ))}
        </ol>
      </div>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        {step === 0 && (
          <div className="space-y-5">
            <RedTeamIntro />
            <Field label="Who hunts">
              <div className="grid grid-cols-1 items-stretch gap-2 sm:grid-cols-3">
                {HUNTERS.map((h) => {
                  const selectable = h.id === "swarm" || h.id === "shannon";
                  const probe = probes[h.id];
                  // Short pills that never wrap; what does not fit in one goes on a muted line under it.
                  const note = selectable ? "Ready" : "Coming soon";
                  const detail = h.id === "shannon" ? "runs in a colony" : null;
                  return (
                    <button
                      key={h.id}
                      type="button"
                      aria-pressed={selectable && hunter === h.id}
                      disabled={!selectable}
                      onClick={() => selectable && setHunter(h.id)}
                      title={selectable ? undefined : `${h.name} installs and probes, but red-team runs do not drive its scans yet${probe ? ` — ${probe.probe.detail}` : ""}`}
                      className={cx(
                        "flex h-full flex-col items-start gap-1.5 rounded-xl border p-3 text-left",
                        !selectable
                          ? "cursor-not-allowed border-border bg-panel-2 opacity-70"
                          : hunter === h.id
                            ? "cursor-pointer border-accent bg-accent-soft"
                            : "cursor-pointer border-border bg-panel-2 hover:border-text/30",
                      )}
                    >
                      <span className="flex items-center gap-2 text-body font-semibold">
                        {h.logo ? (
                          <img src={h.logo} alt="" width={22} height={22} className="size-[22px] shrink-0 rounded-md" />
                        ) : (
                          <span className="grid size-[22px] shrink-0 place-items-center rounded-md bg-accent text-on-accent">
                            <IconAnt size={14} />
                          </span>
                        )}
                        <span className="break-words">{h.name}</span>
                      </span>
                      <span className="flex flex-wrap items-center gap-x-2 gap-y-0.5">
                        <span className={cx("whitespace-nowrap rounded-full px-1.5 py-px text-meta-sm font-medium", selectable ? "bg-ok/15 text-ok" : "bg-panel-3 text-muted")}>{note}</span>
                        {detail && <span className="whitespace-nowrap text-meta-lg text-muted">{detail}</span>}
                      </span>
                      <span className="text-small leading-snug text-muted">{h.blurb}</span>
                    </button>
                  );
                })}
              </div>
            </Field>
            <Field label="Preset" hint="What the hunters look for. Security adds a pre-scan before launch and an operator checklist to the report.">
              <div role="radiogroup" aria-label="Preset" className="grid gap-2 sm:grid-cols-2">
                {PRESETS.map((p) => (
                  <button
                    key={p.id}
                    type="button"
                    role="radio"
                    aria-checked={preset === p.id}
                    onClick={() => setPreset(p.id)}
                    className={cx(
                      "flex cursor-pointer flex-col gap-1 rounded-xl border p-3 text-left",
                      preset === p.id ? "border-accent bg-accent-soft" : "border-border bg-panel-2 hover:border-text/30",
                    )}
                  >
                    <span className="text-body font-semibold">{p.name}</span>
                    <span className="text-small leading-snug text-muted">{p.blurb}</span>
                  </button>
                ))}
              </div>
            </Field>
            <Field label="Repositories" hint="One run per repository. A repository that already has an active run is shown disabled and skipped.">
              {repos === null ? (
                <p className="flex items-center gap-2 text-body-sm text-muted">
                  <Spinner /> Loading repositories…
                </p>
              ) : (
                <RepoList
                  org={org}
                  repos={repos}
                  runs={runs}
                  sessions={sessions}
                  picked={picked}
                  onPick={setPicked}
                  onCancel={onCancel}
                  onOpenRun={() => onOpenHistory(org)}
                />
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
            {hunter === "shannon" ? (
              <p className="text-small-lg leading-snug text-muted">Shannon runs one colony per repository; there is no swarm size to set.</p>
            ) : (
              <Field label={`Hunters per repository: ${swarm}`} hint="Each hunter is a full colony with its own microVM and focus area.">
                <input type="range" min={1} max={8} value={swarm} onChange={(e) => setSwarm(Number(e.target.value))} aria-label="Hunters per repository" className="w-full accent-[var(--accent)]" />
              </Field>
            )}
          </div>
        )}

        {step === 2 && (
          <div className="space-y-5">
            <div role="note" className="rounded-xl border border-warn/40 bg-warn/10 p-3.5 text-body-sm leading-snug">
              <p className="font-semibold text-warn">Red-team runs are expensive</p>
              <p className="mt-1 text-text">
                This starts {plural(hunters, "autonomous colony", "autonomous colonies")} ({swarmSize} per repository × {picked.length}), each in long sessions reading and reproducing code
                {schedule.every === "once" ? "" : `, and repeats ${schedule.every === "weekly" ? "every week" : "every month"} until you switch the schedule off`}.
              </p>
              <p className="mt-1 text-muted">
                {estimate
                  ? `Expect roughly ${formatCost(estimate.low)} or more per ${schedule.every === "once" ? "run" : "firing"}, from your ${estimate.basis === "runs" ? "past red-team runs" : "average colony spend"}.`
                  : "No spend history yet to estimate from — watch the first run's cost before scheduling more."}
              </p>
            </div>
            {preset === "security" && (
              <p className="text-small-lg leading-snug text-muted">
                Security preset: before the hunters launch, this machine pre-scans the repository&apos;s committed files and history (no model tokens, no repository code run) and
                hands each lead to the hunter whose focus it matches. The report adds the leads and an operator checklist of what code cannot prove.
              </p>
            )}
            <label className="flex cursor-pointer items-start gap-2.5 text-body-sm">
              <input type="checkbox" checked={autofix} onChange={(e) => setAutofix(e.target.checked)} className="mt-0.5" />
              <span>
                <span className="font-medium">Let hunters fix what they find</span>
                <span className="block text-small text-muted">Off: hunters only report findings and never open, merge or change anything. On costs more and opens pull requests.</span>
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
                    className={cx("cursor-pointer rounded-full border px-3 py-1 text-small-lg", schedule.every === every ? "border-text bg-panel-3 text-text" : "border-border bg-transparent text-muted hover:text-text")}
                  >
                    {every === "once" ? "Once, now" : every === "weekly" ? "Weekly" : "Monthly"}
                  </button>
                ))}
              </div>
              {schedule.every === "once" && <p className="mt-2 text-small text-muted">Starts as soon as no colony is live.</p>}
              {schedule.every === "weekly" && (
                <div className="mt-2 flex items-center gap-2 text-body-sm">
                  <select aria-label="Weekday" value={schedule.weekday} onChange={(e) => setSchedule({ ...schedule, weekday: Number(e.target.value) })} className="rounded-md border border-border bg-panel px-2 py-1">
                    {WEEKDAYS.map((d, i) => (
                      <option key={d} value={i}>
                        {d}
                      </option>
                    ))}
                  </select>
                  at
                  <input aria-label="Time" type="time" value={schedule.time} onChange={(e) => setSchedule({ ...schedule, time: e.target.value })} className="rounded-md border border-border bg-panel px-2 py-1" />
                  <span className="text-small text-muted">your time</span>
                </div>
              )}
              {schedule.every === "monthly" && (
                <div className="mt-2 flex items-center gap-2 text-body-sm">
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
                  <span className="text-small text-muted">{schedule.day > 28 ? "the month's last day when shorter" : "your time"}</span>
                </div>
              )}
            </Field>
          </div>
        )}
      </div>

      <div className="flex shrink-0 items-center gap-2 border-t border-border px-5 py-3">
        <span className="mr-auto text-small-lg text-muted">
          {plural(picked.length, "repository", "repositories")} · {plural(hunters, "hunter")}
          {preset === "security" ? " · security" : ""}
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
      <h3 className="text-small-lg font-semibold text-text">{label}</h3>
      {hint && <p className="mb-2 text-small text-muted">{hint}</p>}
      {!hint && <div className="mb-2" />}
      {children}
    </section>
  );
}

/**
 * The repositories a red team can target (#1145): one row each, with a single muted line to choose by
 * (its red-team history and last push) — or, for a repository with an active run, the row is disabled
 * and says how far the run is, with a link to it and a Cancel run button. "All" skips those rows.
 */
export function RepoList({
  org,
  repos,
  runs,
  sessions,
  picked,
  onPick,
  onCancel,
  onOpenRun,
  now = Date.now(),
}: {
  org: string;
  repos: Repo[];
  runs: RedTeamRun[];
  sessions: Session[];
  picked: string[];
  onPick: (picked: string[] | ((p: string[]) => string[])) => void;
  onCancel?: (id: string) => Promise<void>;
  onOpenRun?: (run: RedTeamRun) => void;
  now?: number;
}) {
  if (repos.length === 0) return <p className="text-body-sm text-muted">No repositories found for {org}.</p>;
  const rows = sortForRedTeam(repos, runs).map((r) => ({ repo: r, active: activeRunFor(runs, r.full_name) }));
  const free = rows.filter((r) => !r.active).map((r) => r.repo.full_name);
  const skipped = rows.filter((r) => r.active);
  const name = (full: string) => full.split("/")[1] ?? full;
  return (
    <div className="max-h-56 space-y-0.5 overflow-y-auto rounded-lg border border-border p-1.5 scroll-thin">
      <div className="flex flex-wrap items-center gap-x-2 rounded-md px-2 py-1 text-small-lg text-muted">
        <label className="flex cursor-pointer items-center gap-2 hover:text-text">
          <input
            type="checkbox"
            disabled={free.length === 0}
            checked={free.length > 0 && free.every((f) => picked.includes(f))}
            onChange={(e) => onPick(e.target.checked ? free : [])}
          />
          All {free.length}
        </label>
        {skipped.length > 0 && (
          <span className="text-meta-lg text-faint">
            {free.length} of {rows.length}: {skipped.map((r) => name(r.repo.full_name)).join(", ")} already {skipped.length === 1 ? "has a run" : "have runs"}
          </span>
        )}
      </div>
      {rows.map(({ repo: r, active }) => (
        <div
          key={r.full_name}
          aria-disabled={active ? true : undefined}
          className={cx("flex flex-wrap items-center gap-x-2 gap-y-1 rounded-md px-2 py-1", active ? "bg-panel-2/60" : "hover:bg-panel-2")}
        >
          <input
            id={`rt-repo-${r.full_name}`}
            type="checkbox"
            aria-label={name(r.full_name)}
            disabled={active !== null}
            checked={!active && picked.includes(r.full_name)}
            onChange={(e) => onPick((p) => (e.target.checked ? [...p, r.full_name] : p.filter((x) => x !== r.full_name)))}
          />
          <label htmlFor={`rt-repo-${r.full_name}`} className={cx("min-w-0 flex-1 basis-32 cursor-pointer", active && "cursor-not-allowed")}>
            <span className={cx("block truncate text-body-sm", active && "text-muted")}>{name(r.full_name)}</span>
            <span className="block truncate text-meta-lg text-faint">{active ? activeLine(active, sessions) : historyLine(runs, r.full_name, r.pushed_at, now)}</span>
          </label>
          {active && (
            <span className="flex shrink-0 items-center gap-1">
              {onOpenRun && (
                <button type="button" onClick={() => onOpenRun(active)} className="cursor-pointer rounded-md border-0 bg-transparent px-2 py-1 text-small text-accent hover:underline">
                  View run
                </button>
              )}
              {onCancel && <CancelRunButton run={active} onCancel={onCancel} />}
            </span>
          )}
        </div>
      ))}
    </div>
  );
}
