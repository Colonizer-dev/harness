// Loops (loops.rs): saved prompts that launch a colony on a schedule — every N minutes, daily,
// weekly, monthly, or self-paced (each run names the next with loop_next). The page lists them with
// when they run next and how the last run went; "New loop" builds one from a template or scratch;
// a loop's history lists every colony it launched.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { Avatar } from "../components/Avatar";
import { ModelPicker } from "../components/ModelPicker";
import { Button, SESSION_STATUS, Spinner, Switch, cx } from "../components/ui";
import { formatCost, sessionCost } from "../spend";
import type { Loop, LoopCadence, NewLoop, Repo, Session } from "../types";
import { useModels } from "../useModels";
import { DAY_PRESETS, LOOP_TEMPLATES, WEEKDAYS, describeLoop, describeLoopCadence, mapLoopName, nameFromPrompt, relative, toLocalChoice, toUtcLoopCadence, type LoopChoice } from "./loops";

export const LOOP_ORIGIN = "loop:";

/** Whether a colony was launched by a loop. */
export function isLoopColony(session: Pick<Session, "origin">): boolean {
  return Boolean(session.origin?.startsWith(LOOP_ORIGIN));
}

/** The small ↻ badge a loop's colony carries in colony lists. */
export function LoopBadge({ session }: { session: Pick<Session, "origin"> }): ReactElement | null {
  if (!isLoopColony(session)) return null;
  return (
    <span title="launched by a loop" className="ml-1.5 inline-flex shrink-0 items-center rounded-full border border-border px-1.5 text-[10.5px] leading-4 text-muted">
      ↻ loop
    </span>
  );
}

export function LoopsView({
  org,
  repos,
  sessions,
  avatarFor,
  onOpenColony,
}: {
  org: string | null;
  repos: readonly Repo[];
  sessions: readonly Session[];
  avatarFor: (org: string) => string | null;
  onOpenColony: (id: string) => void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [loops, setLoops] = useState<Loop[] | null>(null);
  const [editing, setEditing] = useState<Loop | "new" | null>(null);
  const [history, setHistory] = useState<Loop | null>(null);
  const [now, setNow] = useState(() => Date.now());

  const load = useCallback(() => {
    api.loops().then(setLoops, (e) => toast(errorMessage(e), "error"));
  }, [api, toast]);
  useEffect(() => {
    load();
    const t = setInterval(() => {
      load();
      setNow(Date.now());
    }, 30_000);
    return () => clearInterval(t);
  }, [load]);

  const shown = useMemo(
    () => (loops ?? []).filter((l) => !org || l.org.toLowerCase() === org.toLowerCase()).sort((a, b) => Number(b.enabled) - Number(a.enabled) || a.name.localeCompare(b.name)),
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

  return (
    <main className="cockpit scroll-thin min-h-0 flex-1 overflow-y-auto px-8 pb-24 pt-8">
      <div className="mx-auto max-w-[1100px]">
        <div className="flex flex-wrap items-end gap-4">
          <div className="min-w-0 flex-1">
            <h1 className="m-0 text-[30px] font-semibold tracking-[-0.035em] text-text">Loops</h1>
            <p className="mt-2 text-[14px] text-muted">
              A prompt on a repository that launches a colony on a schedule — or lets each run pick the next. One run at a time. Tip: type{" "}
              <code className="rounded bg-panel-3 px-1 font-mono text-[12.5px]">/loop 1h check CI and fix flakes</code> in the composer (⌘K).
            </p>
          </div>
          <Button variant="primary" onClick={() => setEditing("new")}>
            New loop
          </Button>
        </div>

        <div className="mt-6 overflow-hidden rounded-xl border border-border">
          {loops === null ? (
            <p className="flex items-center gap-2 px-4 py-6 text-[13px] text-muted">
              <Spinner /> Loading loops…
            </p>
          ) : shown.length === 0 ? (
            <div className="px-4 py-10 text-center text-[13.5px] text-muted">
              No loops{org ? ` in ${org}` : ""} yet. Start with a template: triage new issues, keep dependencies current, fix last night's flaky tests.
            </div>
          ) : (
            <ul className="m-0 list-none divide-y divide-border p-0">
              {shown.map((l) => {
                const last = l.last_run ? sessions.find((s) => s.id === l.last_run?.session) : undefined;
                return (
                  <li key={l.id} className={cx("flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3", !l.enabled && "opacity-70")}>
                    <Avatar name={l.org} src={avatarFor(l.org) ?? undefined} size={28} rounded="full" />
                    <div className="min-w-0 flex-1 basis-64">
                      <div className="flex items-center gap-2">
                        <span className="truncate text-[14px] font-medium text-text">{l.name}</span>
                        <span className="truncate font-mono text-[11.5px] text-faint">{l.repo}</span>
                      </div>
                      <div className="mt-0.5 truncate text-[12.5px] text-muted" title={l.last_note ?? undefined}>
                        {describeLoop(l)}
                        {l.enabled && l.next_run_at ? ` · next ${relative(l.next_run_at, now)}` : l.ended_reason ? ` · ${l.ended_reason}` : " · paused"}
                        {l.last_note && !l.ended_reason ? ` · ${l.last_note}` : ""}
                      </div>
                    </div>
                    <div className="w-[170px] shrink-0 text-[12.5px]">
                      {l.last_run ? (
                        <button type="button" onClick={() => onOpenColony(l.last_run!.session)} className="cursor-pointer border-0 bg-transparent p-0 text-left text-muted hover:text-text">
                          run {l.runs} · {last ? SESSION_STATUS[last.status]?.label.toLowerCase() : "…"} · {relative(l.last_run.at, now)}
                        </button>
                      ) : (
                        <span className="text-faint">not run yet</span>
                      )}
                    </div>
                    <div className="flex shrink-0 items-center gap-1.5">
                      <Switch checked={l.enabled} onChange={(on) => void toggle(l, on)} label={`${l.name} enabled`} />
                      <Button size="sm" variant="secondary" onClick={() => void runNow(l)}>
                        Run now
                      </Button>
                      <Button size="sm" variant="ghost" onClick={() => setHistory(l)}>
                        History
                      </Button>
                      <Button size="sm" variant="ghost" onClick={() => setEditing(l)}>
                        Edit
                      </Button>
                      <Button size="sm" variant="ghost" onClick={() => void remove(l)} aria-label={`delete ${l.name}`}>
                        ✕
                      </Button>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </div>
      {editing && <LoopDialog loop={editing === "new" ? null : editing} org={org} repos={repos} onSave={save} onClose={() => setEditing(null)} />}
      {history && <LoopHistory loop={history} onOpenColony={onOpenColony} onClose={() => setHistory(null)} />}
    </main>
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

function LoopDialog({
  loop,
  org,
  repos,
  onSave,
  onClose,
}: {
  loop: Loop | null;
  org: string | null;
  repos: readonly Repo[];
  onSave: (id: string | null, body: NewLoop) => Promise<void>;
  onClose: () => void;
}): ReactElement {
  const ref = useRef<HTMLDialogElement>(null);
  const models = useModels();
  const toast = useToast();
  const scoped = useMemo(() => repos.filter((r) => !org || r.full_name.split("/")[0].toLowerCase() === org.toLowerCase()), [repos, org]);
  const [repo, setRepo] = useState((loop?.repo ?? scoped[0]?.full_name ?? "").replace(/\/\*$/, ""));
  const [prompt, setPrompt] = useState(loop?.prompt ?? "");
  const [name, setName] = useState(loop?.name ?? "");
  // What a run starts: a colony from the prompt, or the repository's architecture map (`owner/*`
  // covers every repository in the org).
  const [kind, setKind] = useState<"colony" | "map">(loop?.kind ?? "colony");
  const [allRepos, setAllRepos] = useState(loop?.kind === "map" && loop.repo.endsWith("/*"));
  const [choice, setChoice] = useState<LoopChoice>(loop ? toLocalChoice(loop.cadence) : { every: "daily", time: "09:00" });
  const [model, setModel] = useState(loop?.model ?? "");
  const [subagentModel, setSubagentModel] = useState(loop?.subagent_model ?? "");
  const [autopilot, setAutopilot] = useState(loop?.autopilot ?? true);
  const [maxRuns, setMaxRuns] = useState(loop?.max_runs ? String(loop.max_runs) : "");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    const d = ref.current;
    if (d && !d.open) d.showModal?.();
  }, []);

  const submit = async () => {
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
        tz_offset_minutes: -new Date().getTimezoneOffset(),
        model: model || null,
        subagent_model: subagentModel || null,
        autopilot,
        max_runs: maxRuns ? Number.parseInt(maxRuns, 10) : null,
        end_at: loop?.end_at ?? null,
        enabled: loop ? loop.enabled || !loop.ended_reason : true,
      });
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  const field = "w-full rounded-lg border border-border bg-transparent px-2.5 py-1.5 text-[13px] text-text outline-none focus:border-border-strong";
  // An `owner/*` loop edits with the org alone in the repository picker; leaving it for a loop on
  // one repository needs a real repository of that org to name.
  const firstInOrg = (owner: string) =>
    repos.find((r) => r.full_name.split("/")[0].toLowerCase() === owner.toLowerCase())?.full_name ?? "";
  return (
    <dialog ref={ref} onClose={onClose} aria-labelledby="loop-dialog-title" className="m-auto w-[min(640px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50">
      <div className="flex items-center gap-3 border-b border-border px-5 py-3">
        <h2 id="loop-dialog-title" className="min-w-0 flex-1 text-[16px] font-semibold">
          {loop ? `Edit "${loop.name}"` : "New loop"}
        </h2>
        <button type="button" onClick={() => ref.current?.close()} aria-label="Close" className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
          ✕
        </button>
      </div>
      <div className="scroll-thin max-h-[70vh] space-y-4 overflow-y-auto px-5 py-4 text-[13px]">
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
                }}
                className="cursor-pointer rounded-full border border-border bg-transparent px-2.5 py-1 text-[12px] text-muted hover:border-border-strong hover:text-text"
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
                }}
                className={cx("cursor-pointer rounded-lg border px-2.5 py-1 text-[12.5px]", kind === k ? "border-accent bg-accent-soft text-text" : "border-border bg-transparent text-muted hover:text-text")}
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
          <p className="text-[12.5px] text-muted">Each run redraws the architecture map the way the Map view's "Redraw map" does — no prompt needed.</p>
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
                className={cx("cursor-pointer rounded-lg border px-2.5 py-1 text-[12.5px]", choice.every === every ? "border-accent bg-accent-soft text-text" : "border-border bg-transparent text-muted hover:text-text")}
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
          {choice.every === "self_paced" && (
            <p className="text-faint">Each run chooses when the next starts (15 minutes to 24 hours) with loop_next; without a choice it runs again in 24 hours.</p>
          )}
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
          <label className="flex items-center gap-2">
            stop after
            <input type="number" min={1} value={maxRuns} onChange={(e) => setMaxRuns(e.target.value)} placeholder="∞" className={cx(field, "w-20")} />
            runs
          </label>
        </div>
        <p className="rounded-lg border border-border bg-panel-2 px-3 py-2 text-[12.5px] text-muted">
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

function LoopHistory({ loop, onOpenColony, onClose }: { loop: Loop; onOpenColony: (id: string) => void; onClose: () => void }): ReactElement {
  const ref = useRef<HTMLDialogElement>(null);
  const api = useApi();
  const [runs, setRuns] = useState<Session[] | null>(null);
  useEffect(() => {
    const d = ref.current;
    if (d && !d.open) d.showModal?.();
    api.loopRuns(loop.id).then(setRuns, () => setRuns([]));
  }, [api, loop.id]);
  return (
    <dialog ref={ref} onClose={onClose} aria-labelledby="loop-history-title" className="m-auto w-[min(720px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50">
      <div className="flex items-center gap-3 border-b border-border px-5 py-3">
        <h2 id="loop-history-title" className="min-w-0 flex-1 truncate text-[16px] font-semibold">
          {loop.name} · history
        </h2>
        <button type="button" onClick={() => ref.current?.close()} aria-label="Close" className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
          ✕
        </button>
      </div>
      <div className="px-5 py-3 text-[12.5px] text-muted">
        {describeLoopCadence(loop.cadence)} · {loop.runs} {loop.runs === 1 ? "run" : "runs"}
        {loop.last_note ? ` · ${loop.last_note}` : ""}
      </div>
      <div className="scroll-thin max-h-[60vh] overflow-y-auto border-t border-border">
        {runs === null ? (
          <p className="flex items-center gap-2 px-5 py-4 text-[13px] text-muted">
            <Spinner /> Loading runs…
          </p>
        ) : runs.length === 0 ? (
          <p className="px-5 py-6 text-[13px] text-faint">No runs yet.</p>
        ) : (
          <ul className="m-0 list-none divide-y divide-border p-0">
            {runs.map((s) => (
              <li key={s.id} className="flex items-center gap-3 px-5 py-2.5 text-[13px]">
                <button type="button" onClick={() => onOpenColony(s.id)} className="min-w-0 flex-1 cursor-pointer truncate border-0 bg-transparent p-0 text-left text-text hover:underline">
                  {s.summary || s.issue_title || s.id}
                </button>
                <span className="w-24 shrink-0 text-muted">{SESSION_STATUS[s.status]?.label ?? s.status}</span>
                <span className="w-16 shrink-0 text-right tabular-nums text-muted">{formatCost(sessionCost(s))}</span>
                <span className="w-16 shrink-0 text-right text-faint">{relative(s.created_at)}</span>
                {s.pr_url ? (
                  <a href={s.pr_url} target="_blank" rel="noreferrer" className="shrink-0 text-accent">
                    PR ↗
                  </a>
                ) : (
                  <span className="w-8 shrink-0" />
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
    </dialog>
  );
}
