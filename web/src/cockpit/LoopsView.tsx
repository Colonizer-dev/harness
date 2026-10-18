// Loops (loops.rs): saved prompts that launch a colony on a schedule — every N minutes, daily,
// weekly, monthly, or self-paced (each run names the next with loop_next). The page lists them with
// when they run next and how the last run went; "New loop" builds one from a template or scratch;
// a loop's history lists every colony it launched.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { ModelPicker } from "../components/ModelPicker";
import { Button, SESSION_STATUS, Spinner, Switch, cx } from "../components/ui";
import { formatCost, sessionCost } from "../spend";
import type { Loop, LoopCadence, ModuleInfo, NewLoop, OrgInfo, Repo, Session } from "../types";
import { useModels } from "../useModels";
import { TsAnyLoopCard } from "./TsAnyLoop";
import { DAY_PRESETS, LOOP_TEMPLATES, WEEKDAYS, describeLoopCadence, endDate, endAtError, endAtFromInput, endAtInputValue, mapLoopName, nameFromPrompt, relative, selfPacedWarning, toLocalChoice, toUtcLoopCadence, type LoopChoice } from "./loops";
import { DocsLoopCard } from "./DocsLoopCard";
import { MergeLoopCard } from "./MergeLoopCard";
import { Page } from "./Page";
import { DiskCleanupCard } from "./DiskCleanupLoop";
import { isDiskCleanup } from "./diskCleanup";
import { DetailSection, LoopCard, scheduleLine } from "./LoopCard";
import { IconLoop } from "./loopIcons";
import { plural } from "./loopHistory";
import { SupplyChainLoopCard } from "./SupplyChainLoop";

export const LOOP_ORIGIN = "loop:";

/** Whether a colony was launched by a loop. */
export function isLoopColony(session: Pick<Session, "origin">): boolean {
  return Boolean(session.origin?.startsWith(LOOP_ORIGIN));
}

/** The small ↻ badge a loop's colony carries in colony lists. */
export function LoopBadge({ session }: { session: Pick<Session, "origin"> }): ReactElement | null {
  if (!isLoopColony(session)) return null;
  return (
    <span title="launched by a loop" className="ml-1.5 inline-flex shrink-0 items-center rounded-full border border-border px-1.5 text-meta-sm leading-4 text-muted">
      ↻ loop
    </span>
  );
}

export function LoopsView({
  org,
  orgs,
  repos,
  sessions,
  onOpenColony,
}: {
  org: string | null;
  /** Every org with its settings: which agent module each org's loop colonies launch on. */
  orgs: readonly OrgInfo[];
  repos: readonly Repo[];
  sessions: readonly Session[];
  avatarFor?: (org: string) => string | null;
  onOpenColony: (id: string) => void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [loops, setLoops] = useState<Loop[] | null>(null);
  const [editing, setEditing] = useState<Loop | "new" | null>(null);
  const [template, setTemplate] = useState<number | undefined>(undefined);

  const load = useCallback(() => {
    api.loops().then(setLoops, (e) => toast(errorMessage(e), "error"));
  }, [api, toast]);
  useEffect(() => {
    load();
    const t = setInterval(load, 30_000);
    return () => clearInterval(t);
  }, [load]);

  // The built-in disk cleanup belongs to the host, not an org: it is one of the built-in cards.
  const builtin = useMemo(() => (loops ?? []).find(isDiskCleanup) ?? null, [loops]);
  const mine = useMemo(
    () =>
      (loops ?? [])
        .filter((l) => !isDiskCleanup(l))
        .filter((l) => !org || l.org.toLowerCase() === org.toLowerCase())
        .sort((a, b) => Number(b.enabled) - Number(a.enabled) || a.name.localeCompare(b.name)),
    [loops, org],
  );

  const save = async (id: string | null, body: NewLoop) => {
    const saved = id ? await api.updateLoop(id, body) : await api.createLoop(body);
    toast(id ? `Saved "${saved.name}"` : `Loop "${saved.name}" runs ${describeLoopCadence(saved.cadence)}`);
    setEditing(null);
    load();
  };

  const toggle = async (l: Loop, enabled: boolean) => {
    try {
      await api.updateLoop(l.id, bodyOf(l, { enabled }));
      load();
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const runNow = async (l: Loop) => {
    try {
      const s = await api.runLoopNow(l.id);
      toast({ title: `"${l.name}" started`, body: `Run ${l.runs + 1} on ${l.repo}`, kind: "success", action: { label: "Watch it work", onClick: () => onOpenColony(s.id) } });
      load();
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const remove = async (l: Loop) => {
    if (!window.confirm(`Delete the loop "${l.name}"? Its past colonies stay.`)) return;
    try {
      await api.deleteLoop(l.id);
      load();
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const newLoop = (t?: number) => {
    setTemplate(t);
    setEditing("new");
  };

  return (
    <Page>
      <div className="flex flex-wrap items-end gap-x-6 gap-y-3">
        <div className="min-w-0 flex-1 basis-80">
          <h1 className="m-0 text-display-xl font-semibold tracking-[-0.035em] text-text">Loops</h1>
          <p className="mt-2 text-body-lg text-muted">
            Work that runs on a schedule, without you. Built-in loops look after your repositories; your own loops launch a colony from a prompt. Tip: type{" "}
            <code className="rounded bg-panel-3 px-1 font-mono text-small-lg">/loop 1h check CI and fix flakes</code> in Colonize (⌘K).
          </p>
        </div>
        <Button variant="primary" onClick={() => newLoop()}>
          New loop
        </Button>
      </div>

      <LoopSection title="Built-in" label="Built-in loops" meta="Always here, and yours to switch on.">
        <MergeLoopCard repos={repos} onOpenColony={onOpenColony} />
        <SupplyChainLoopCard onOpenColony={onOpenColony} />
        <TsAnyLoopCard onOpenColony={onOpenColony} />
        <DocsLoopCard onOpenColony={onOpenColony} />
        {builtin && <DiskCleanupCard loop={builtin} onChanged={load} />}
      </LoopSection>

      <LoopSection title="Your loops" label="Your loops" meta={loops && mine.length > 0 ? plural(mine.length, "loop") : undefined}>
        {loops === null ? (
          <p className="flex items-center gap-2 text-body-sm text-muted">
            <Spinner /> Loading loops…
          </p>
        ) : mine.length === 0 ? (
          <div className="col-span-full rounded-xl border border-dashed border-border-strong px-5 py-8 text-center">
            <p className="m-0 text-body-lg font-medium text-text">No loops{org ? ` in ${org}` : ""} yet</p>
            <p className="mx-auto mt-1 max-w-[46ch] text-body-sm text-muted">A loop is a prompt on a repository that runs on a schedule. Start from a template, or write your own.</p>
            <div className="mt-4 flex flex-wrap justify-center gap-2">
              {LOOP_TEMPLATES.map((t, i) => (
                <button key={t.label} type="button" onClick={() => newLoop(i)} className="cursor-pointer rounded-full border border-border bg-panel px-3 py-1.5 text-small-lg text-text hover:border-border-strong">
                  {t.label}
                </button>
              ))}
            </div>
            <Button className="mt-4" variant="primary" onClick={() => newLoop()}>
              New loop
            </Button>
          </div>
        ) : (
          mine.map((l) => <CustomLoopCard key={l.id} loop={l} session={sessions.find((s) => s.id === l.last_run?.session)} onToggle={(on) => void toggle(l, on)} onRun={() => void runNow(l)} onEdit={() => setEditing(l)} onDelete={() => void remove(l)} onOpenColony={onOpenColony} />)
        )}
      </LoopSection>

      {editing && <LoopDialog loop={editing === "new" ? null : editing} template={template} org={org} orgs={orgs} repos={repos} onSave={save} onClose={() => setEditing(null)} />}
    </Page>
  );
}

/** A titled group of loop cards: one grid for the built-in loops, one for the user's own. */
function LoopSection({ title, label, meta, children }: { title: string; label: string; meta?: string; children: ReactNode }): ReactElement {
  return (
    <section aria-label={label} className="mt-10">
      <div className="mb-3 flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h2 className="m-0 text-title font-semibold text-text">{title}</h2>
        {meta && <span className="text-body-sm text-faint">{meta}</span>}
      </div>
      <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">{children}</div>
    </section>
  );
}

/** One of the user's own loops, on the same card as every built-in one. */
export function CustomLoopCard({
  loop: l,
  session,
  onToggle,
  onRun,
  onEdit,
  onDelete,
  onOpenColony,
}: {
  loop: Loop;
  session?: Session;
  onToggle: (on: boolean) => void;
  onRun: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onOpenColony: (id: string) => void;
}): ReactElement {
  const map = (l.kind ?? "colony") === "map";
  const orgWide = l.repo.endsWith("/*");
  const cadence = describeLoopCadence(l.cadence);
  const ended = l.ended_reason;
  return (
    <LoopCard
      historyId={l.id}
      custom
      icon={<IconLoop />}
      name={l.name}
      purpose={map ? (orgWide ? `Keeps the architecture map of every repository in ${l.repo.slice(0, -2)} fresh.` : `Keeps the architecture map of ${l.repo} fresh.`) : nameFromPrompt(l.prompt)}
      enabled={l.enabled}
      onToggle={onToggle}
      schedule={ended ? `Ended · ${ended}` : scheduleLine(`${cadence}${l.end_at ? ` · ends ${endDate(l.end_at)}` : ""}`, l.enabled, l.next_run_at)}
      scope={{ text: orgWide ? `Every repository in ${l.repo.slice(0, -2)}` : l.repo, ready: true }}
      lastNote={l.last_run ? `Run ${l.runs}${session ? ` · ${SESSION_STATUS[session.status]?.label ?? session.status}` : ""}` : undefined}
      attention={ended ? null : l.last_note && /could not|refus|fail/i.test(l.last_note) ? l.last_note : null}
      refreshKey={l.last_run?.session}
      onOpenColony={onOpenColony}
      actions={
        <>
          <Button size="sm" variant="secondary" onClick={onRun}>
            Run now
          </Button>
          <Button size="sm" variant="secondary" onClick={onEdit}>
            Edit
          </Button>
          <Button size="sm" variant="ghost" onClick={onDelete} aria-label={`delete ${l.name}`}>
            Delete
          </Button>
        </>
      }
    >
      {!map && (
        <DetailSection title="Prompt">
          <p className="m-0 whitespace-pre-wrap rounded-lg border border-border bg-panel px-3.5 py-3 text-small-lg text-muted [overflow-wrap:anywhere]">{l.prompt}</p>
          {l.last_note && <p className="m-0 mt-2 text-small-lg text-muted">Last note: {l.last_note}</p>}
        </DetailSection>
      )}
      <DetailSection title="Colonies" meta="launched by this loop">
        <LoopColonies loop={l} onOpenColony={onOpenColony} />
      </DetailSection>
    </LoopCard>
  );
}

/** A loop's settings as the full-replace body PUT wants, with changes applied. */
export function bodyOf(l: Loop, change: Partial<NewLoop> = {}): NewLoop {
  return {
    name: l.name,
    repo: l.repo,
    prompt: l.prompt,
    cadence: l.cadence,
    kind: l.kind ?? "colony",
    needs_github: l.needs_github ?? false,
    tz_offset_minutes: l.tz_offset_minutes,
    model: l.model,
    subagent_model: l.subagent_model,
    autopilot: l.autopilot,
    max_runs: l.max_runs,
    end_at: l.end_at,
    enabled: l.enabled,
    ...change,
  };
}

export function LoopDialog({
  loop,
  template,
  org,
  orgs,
  repos,
  onSave,
  onClose,
}: {
  loop: Loop | null;
  /** The index in LOOP_TEMPLATES a new loop starts from. */
  template?: number;
  org: string | null;
  orgs: readonly OrgInfo[];
  repos: readonly Repo[];
  onSave: (id: string | null, body: NewLoop) => Promise<void>;
  onClose: () => void;
}): ReactElement {
  const ref = useRef<HTMLDialogElement>(null);
  const api = useApi();
  const models = useModels();
  const toast = useToast();
  // What the modules offer, so a self-paced cadence can warn when the org's agent module serves
  // neither loop tool (issue #643). An install that never answers says nothing, not the wrong thing.
  const [modules, setModules] = useState<ModuleInfo[]>([]);
  useEffect(() => {
    let live = true;
    api.modules().then((m) => live && setModules(m), () => {});
    return () => {
      live = false;
    };
  }, [api]);
  const scoped = useMemo(() => repos.filter((r) => !org || r.full_name.split("/")[0].toLowerCase() === org.toLowerCase()), [repos, org]);
  const [repo, setRepo] = useState((loop?.repo ?? scoped[0]?.full_name ?? "").replace(/\/\*$/, ""));
  const start = !loop && template != null ? LOOP_TEMPLATES[template] : undefined;
  const [prompt, setPrompt] = useState(loop?.prompt ?? start?.prompt ?? "");
  const [name, setName] = useState(loop?.name ?? start?.label ?? "");
  // What a run starts: a colony from the prompt, or the repository's architecture map (`owner/*`
  // covers every repository in the org).
  // The built-in disk cleanup never opens this form (it has its own dialog).
  const [kind, setKind] = useState<"colony" | "map">(loop?.kind === "map" ? "map" : "colony");
  const [allRepos, setAllRepos] = useState(loop?.kind === "map" && loop.repo.endsWith("/*"));
  // Whether the loop's work is GitHub's (issue #778): the run then waits for the repository to be
  // reachable and gets the read-only context and the host-proxied write tools. Colony loops only.
  const [needsGithub, setNeedsGithub] = useState(loop?.needs_github ?? start?.needsGithub ?? false);
  const [choice, setChoice] = useState<LoopChoice>(loop ? toLocalChoice(loop.cadence) : (start?.choice ?? { every: "daily", time: "09:00" }));
  const [model, setModel] = useState(loop?.model ?? "");
  const [subagentModel, setSubagentModel] = useState(loop?.subagent_model ?? "");
  const [autopilot, setAutopilot] = useState(loop?.autopilot ?? true);
  const [maxRuns, setMaxRuns] = useState(loop?.max_runs ? String(loop.max_runs) : "");
  // The optional end date, held in the field's own local `datetime-local` shape and sent as UTC.
  const [endAt, setEndAt] = useState(endAtInputValue(loop?.end_at ?? null));
  const [endAtProblem, setEndAtProblem] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    const d = ref.current;
    if (d && !d.open) d.showModal?.();
  }, []);

  const submit = async () => {
    const problem = endAtError(endAt);
    setEndAtProblem(problem);
    if (problem) return;
    setSaving(true);
    try {
      const cadence: LoopCadence = toUtcLoopCadence(choice);
      const scope = kind === "map" && allRepos ? `${repo.split("/")[0]}/*` : repo;
      await onSave(loop?.id ?? null, {
        name: name.trim() || (kind === "map" ? mapLoopName(scope) : nameFromPrompt(prompt)),
        repo: scope,
        prompt: kind === "map" ? "" : prompt,
        cadence,
        kind,
        needs_github: kind === "colony" && needsGithub,
        tz_offset_minutes: -new Date().getTimezoneOffset(),
        model: model || null,
        subagent_model: subagentModel || null,
        autopilot,
        max_runs: maxRuns ? Number.parseInt(maxRuns, 10) : null,
        end_at: endAtFromInput(endAt),
        enabled: loop ? loop.enabled || !loop.ended_reason : true,
      });
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  const field = "w-full rounded-lg border border-border bg-transparent px-2.5 py-1.5 text-body-sm text-text outline-none focus:border-border-strong";
  // An `owner/*` loop edits with the org alone in the repository picker; leaving it for a loop on
  // one repository needs a real repository of that org to name.
  const firstInOrg = (owner: string) =>
    repos.find((r) => r.full_name.split("/")[0].toLowerCase() === owner.toLowerCase())?.full_name ?? "";
  // The org whose agent module will launch this loop's colonies — the repository scope's owner —
  // warned about when its module cannot pace or stop a self-paced loop (issue #643).
  const pacingWarning = kind === "colony" && choice.every === "self_paced" && repo.includes("/")
    ? selfPacedWarning(repo.split("/")[0], orgs, modules)
    : null;
  return (
    <dialog ref={ref} onClose={onClose} aria-labelledby="loop-dialog-title" className="m-auto w-[min(640px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50">
      <div className="flex items-center gap-3 border-b border-border px-5 py-3">
        <h2 id="loop-dialog-title" className="min-w-0 flex-1 text-title-sm font-semibold">
          {loop ? `Edit "${loop.name}"` : "New loop"}
        </h2>
        <button type="button" onClick={() => ref.current?.close()} aria-label="Close" className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
          ✕
        </button>
      </div>
      <div className="scroll-thin max-h-[70vh] space-y-4 overflow-y-auto px-5 py-4 text-body-sm">
        {!loop && (
          <div className="flex flex-wrap gap-1.5">
            {LOOP_TEMPLATES.map((t) => (
              <button
                key={t.label}
                type="button"
                onClick={() => {
                  setPrompt(t.prompt);
                  setName(t.label);
                  setChoice(t.choice);
                  setKind("colony");
                  setNeedsGithub(t.needsGithub);
                }}
                className="cursor-pointer rounded-full border border-border bg-transparent px-2.5 py-1 text-small text-muted hover:border-border-strong hover:text-text"
              >
                {t.label}
              </button>
            ))}
          </div>
        )}
        <fieldset className="space-y-2">
          <legend className="mb-1 text-muted">Runs</legend>
          <div className="flex flex-wrap gap-1.5">
            {(
              [
                ["colony", "A colony from a prompt"],
                ["map", "Refresh the map"],
              ] as const
            ).map(([k, label]) => (
              <button
                key={k}
                type="button"
                aria-pressed={kind === k}
                onClick={() => {
                  setKind(k);
                  if (k === "colony" && !repo.includes("/")) setRepo(firstInOrg(repo));
                  // A map refresh reads the repository, not GitHub; the need is colony-only.
                  if (k === "map") setNeedsGithub(false);
                }}
                className={cx("cursor-pointer rounded-lg border px-2.5 py-1 text-small-lg", kind === k ? "border-accent bg-accent-soft text-text" : "border-border bg-transparent text-muted hover:text-text")}
              >
                {label}
              </button>
            ))}
          </div>
          {kind === "map" && (
            <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5">
              <span className="text-muted">for</span>
              <label className="flex items-center gap-1.5 text-muted">
                <input
                  type="radio"
                  name="loop-map-scope"
                  checked={!allRepos}
                  onChange={() => {
                    setAllRepos(false);
                    if (!repo.includes("/")) setRepo(firstInOrg(repo));
                  }}
                />
                this repository
              </label>
              <label className="flex items-center gap-1.5 text-muted">
                <input type="radio" name="loop-map-scope" checked={allRepos} onChange={() => setAllRepos(true)} />
                all repositories in {repo.split("/")[0]}
              </label>
            </div>
          )}
        </fieldset>
        <label className="block">
          <span className="mb-1 block text-muted">Repository</span>
          <select value={repo} onChange={(e) => setRepo(e.target.value)} className={field}>
            {scoped.map((r) => (
              <option key={r.full_name} value={r.full_name}>
                {r.full_name}
              </option>
            ))}
          </select>
        </label>
        {kind === "map" ? (
          <p className="text-small-lg text-muted">Each run redraws the architecture map the way the Map view's "Redraw map" does — no prompt needed.</p>
        ) : (
          <label className="block">
            <span className="mb-1 block text-muted">What each run does</span>
            <textarea value={prompt} onChange={(e) => setPrompt(e.target.value)} rows={5} className={cx(field, "resize-y font-sans")} placeholder="Check CI on main and fix any flaky test at its cause…" />
          </label>
        )}
        <label className="block">
          <span className="mb-1 block text-muted">Name</span>
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={kind === "map" ? mapLoopName(allRepos ? `${repo.split("/")[0]}/*` : repo) : nameFromPrompt(prompt) || "Loop"}
            className={field}
          />
        </label>
        <fieldset className="space-y-2">
          <legend className="mb-1 text-muted">When</legend>
          <div className="flex flex-wrap gap-1.5">
            {(
              [
                ["interval", "Every…"],
                ["every_days", "Every N days"],
                ["daily", "Daily"],
                ["weekly", "Weekly"],
                ["monthly", "Monthly"],
                ["self_paced", "Self-paced"],
              ] as const
            ).map(([every, label]) => (
              <button
                key={every}
                type="button"
                aria-pressed={choice.every === every}
                onClick={() =>
                  setChoice(
                    every === "interval"
                      ? { every, minutes: 60 }
                      : every === "every_days"
                        ? { every, days: 14, time: "09:00" }
                        : every === "daily"
                          ? { every, time: "09:00" }
                          : every === "weekly"
                            ? { every, weekday: 0, time: "09:00" }
                            : every === "monthly"
                              ? { every, day: 1, time: "09:00" }
                              : { every },
                  )
                }
                className={cx("cursor-pointer rounded-lg border px-2.5 py-1 text-small-lg", choice.every === every ? "border-accent bg-accent-soft text-text" : "border-border bg-transparent text-muted hover:text-text")}
              >
                {label}
              </button>
            ))}
          </div>
          {choice.every === "interval" && (
            <label className="flex items-center gap-2">
              every
              <input type="number" min={15} max={10080} value={choice.minutes} onChange={(e) => setChoice({ every: "interval", minutes: Number(e.target.value) })} className={cx(field, "w-24")} />
              minutes (at least 15)
            </label>
          )}
          {choice.every === "every_days" && (
            <div className="flex flex-wrap items-center gap-2">
              every
              <select
                aria-label="days between runs"
                value={DAY_PRESETS.includes(choice.days) ? choice.days : "custom"}
                // A non-preset day count, so picking "custom" shows the input instead of the menu.
                onChange={(e) => setChoice(e.target.value === "custom" ? { ...choice, days: 45 } : { ...choice, days: Number(e.target.value) })}
                className={cx(field, "w-32")}
              >
                {DAY_PRESETS.map((d) => (
                  <option key={d} value={d}>
                    {d} days
                  </option>
                ))}
                <option value="custom">custom (days)</option>
              </select>
              {!DAY_PRESETS.includes(choice.days) && (
                <input type="number" min={1} max={365} aria-label="days between runs" value={choice.days} onChange={(e) => setChoice({ ...choice, days: Number(e.target.value) })} className={cx(field, "w-20")} />
              )}
              at
              <input type="time" value={choice.time} onChange={(e) => setChoice({ ...choice, time: e.target.value })} className={cx(field, "w-32")} />
              <span className="text-faint">your local time</span>
            </div>
          )}
          {(choice.every === "daily" || choice.every === "weekly" || choice.every === "monthly") && (
            <div className="flex flex-wrap items-center gap-2">
              {choice.every === "weekly" && (
                <select value={choice.weekday} onChange={(e) => setChoice({ ...choice, weekday: Number(e.target.value) })} className={cx(field, "w-36")}>
                  {WEEKDAYS.map((d, i) => (
                    <option key={d} value={i}>
                      {d}
                    </option>
                  ))}
                </select>
              )}
              {choice.every === "monthly" && (
                <label className="flex items-center gap-1.5">
                  day
                  <input type="number" min={1} max={31} value={choice.day} onChange={(e) => setChoice({ ...choice, day: Number(e.target.value) })} className={cx(field, "w-20")} />
                </label>
              )}
              at
              <input type="time" value={choice.time} onChange={(e) => setChoice({ ...choice, time: e.target.value })} className={cx(field, "w-32")} />
              <span className="text-faint">your local time</span>
            </div>
          )}
          {choice.every === "self_paced" && kind === "colony" && (pacingWarning ? (
            <p className="text-small-lg text-warn">{pacingWarning}</p>
          ) : (
            <p className="text-faint">Each run chooses when the next starts (15 minutes to 24 hours) with loop_next; without a choice it runs again in 24 hours.</p>
          ))}
        </fieldset>
        <div className="grid gap-3 sm:grid-cols-2">
          <label className="block">
            <span className="mb-1 block text-muted">Model</span>
            <ModelPicker value={model} onChange={setModel} models={models} emptyLabel="Routing decides" ariaLabel="loop model" />
          </label>
          <label className="block">
            <span className="mb-1 block text-muted">Subagent model</span>
            <ModelPicker value={subagentModel} onChange={setSubagentModel} models={models} emptyLabel="Agent default" ariaLabel="loop subagent model" />
          </label>
        </div>
        <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
          <span className="flex items-center gap-2">
            <Switch checked={autopilot} onChange={setAutopilot} label="Autopilot" /> Autopilot: open the pull request without asking
          </span>
          {kind === "colony" && (
            <span className="flex items-center gap-2" title="The run waits until the mothership can reach the repository, then reads /colonizer/github and labels or comments through the host">
              <Switch checked={needsGithub} onChange={setNeedsGithub} label="Needs GitHub" /> Needs GitHub: read issues, CI and merged PRs, and label or comment on issues
            </span>
          )}
          <label className="flex items-center gap-2">
            stop after
            <input type="number" min={1} value={maxRuns} onChange={(e) => setMaxRuns(e.target.value)} placeholder="∞" className={cx(field, "w-20")} />
            runs
          </label>
          <label className="flex items-center gap-2">
            ends
            <input
              type="datetime-local"
              aria-label="Ends"
              value={endAt}
              onChange={(e) => {
                setEndAt(e.target.value);
                if (endAtProblem) setEndAtProblem(null);
              }}
              className={cx(field, "w-56")}
            />
            <span className="text-faint">your local time</span>
          </label>
        </div>
        {endAtProblem && (
          <p role="alert" className="m-0 text-small-lg text-err">
            {endAtProblem}
          </p>
        )}
        <p className="rounded-lg border border-border bg-panel-2 px-3 py-2 text-small-lg text-muted">
          Each run is a full colony with its own microVM and model spend. A frequent loop on a large repository adds up — start daily, and let the loop stop itself (loop_stop) when its goal is met.
        </p>
      </div>
      <div className="flex items-center justify-end gap-2 border-t border-border px-5 py-3">
        <Button onClick={() => ref.current?.close()}>Cancel</Button>
        <Button variant="primary" disabled={saving || !repo || (kind === "colony" && !prompt.trim())} onClick={() => void submit()}>
          {saving && <Spinner />} {loop ? "Save" : "Create loop"}
        </Button>
      </div>
    </dialog>
  );
}

/** The colonies a loop launched, newest first, with their state, cost and pull request. */
function LoopColonies({ loop, onOpenColony }: { loop: Loop; onOpenColony: (id: string) => void }): ReactElement {
  const api = useApi();
  const [runs, setRuns] = useState<Session[] | null>(null);
  useEffect(() => {
    api.loopRuns(loop.id).then(setRuns, () => setRuns([]));
  }, [api, loop.id]);
  if (runs === null) {
    return (
      <p className="flex items-center gap-2 text-body-sm text-muted">
        <Spinner /> Loading colonies…
      </p>
    );
  }
  if (runs.length === 0) return <p className="m-0 text-body-sm text-faint">No colonies yet.</p>;
  return (
    <ul className="m-0 list-none divide-y divide-border overflow-hidden rounded-xl border border-border bg-panel p-0">
      {runs.slice(0, 20).map((s) => (
        <li key={s.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3.5 py-2.5 text-small-lg">
          <button type="button" onClick={() => onOpenColony(s.id)} className="min-w-0 flex-1 basis-48 cursor-pointer border-0 bg-transparent p-0 text-left text-text hover:underline [overflow-wrap:anywhere]">
            {s.summary || s.issue_title || s.id}
          </button>
          <span className="text-muted">{SESSION_STATUS[s.status]?.label ?? s.status}</span>
          <span className="tabular-nums text-muted">{formatCost(sessionCost(s))}</span>
          <span className="text-faint">{relative(s.created_at)}</span>
          {s.pr_url && (
            <a href={s.pr_url} target="_blank" rel="noreferrer" className="text-accent">
              PR ↗
            </a>
          )}
        </li>
      ))}
    </ul>
  );
}
