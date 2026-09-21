// Red-team raids (issue #212): launch a swarm of hunter colonies at a repository, and watch the
// raid as it finds, validates and files. One section of the overview, not a page: the launch form
// up top, each recent run below with its hunters and the counts they came back with. The server's
// 409 reasons (colonies still live, a run already active) render verbatim inline, so the gate
// explains itself.
import { useState, type ReactElement } from "react";

import { Badge, Button, SESSION_STATUS, cx, inputClass, type Tone } from "../components/ui";
import { RED_TEAM_STATE, gateMessage, isActive, isGated, liveCount } from "../redTeam";
import type { RedTeamHunter, RedTeamRun, Session, SessionStatus, StartRedTeamRunRequest } from "../types";

const TONE_VAR: Record<Tone, string> = {
  neutral: "var(--faint)",
  info: "var(--info)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  err: "var(--err)",
  accent: "var(--accent)",
};

export function RedTeamCard({
  runs,
  sessions,
  onStart,
  onStop,
  onOpenColony,
}: {
  runs: RedTeamRun[];
  sessions: Session[];
  /** Absent (unwired Overview, a test) just disables the launch buttons. */
  onStart?: (body: StartRedTeamRunRequest) => Promise<void>;
  onStop?: (id: string) => Promise<void>;
  onOpenColony: (id: string) => void;
}): ReactElement {
  const [repo, setRepo] = useState("");
  const [swarm, setSwarm] = useState(3);
  const [modules, setModules] = useState("");
  const [autofix, setAutofix] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState<string | null>(null);

  const live = liveCount(sessions);
  const sorted = [...runs].sort((a, b) => b.created_at.localeCompare(a.created_at));

  const launch = async (arm: boolean) => {
    const target = repo.trim();
    if (!target) {
      setError("pick a repository to raid");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      if (!onStart) return;
      // The 409 carries the server's own reason — colonies live, a run already active — and that
      // reason is the UI's answer to "why won't the nest let the swarm out?".
      await onStart({
        repo: target,
        swarm_size: Number.isFinite(swarm) ? Math.min(8, Math.max(1, swarm)) : 3,
        modules: modules.trim() ? modules.split(",").map((m) => m.trim()).filter(Boolean) : undefined,
        autofix,
        arm,
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : "the raid could not start");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="overflow-hidden rounded-2xl border border-border bg-panel">
      <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 border-b border-border px-4 py-3">
        <div>
          <div className="font-mono text-[10px] tracking-[0.12em] text-faint">RED TEAM</div>
          <div className="mt-0.5 text-[14px] font-semibold">Adversarial raids</div>
        </div>
        <div className="font-mono text-[11px] text-muted tabular-nums">
          <span className="text-err">{live}</span> live · {sorted.length} {sorted.length === 1 ? "run" : "runs"}
        </div>
      </div>

      <div className="flex flex-col gap-2 px-4 py-3">
        <div className="flex flex-wrap items-end gap-2">
          <label className="flex min-w-0 flex-1 flex-col gap-1">
            <span className="font-mono text-[10px] tracking-[0.08em] text-faint">REPOSITORY</span>
            <input
              value={repo}
              onChange={(e) => setRepo(e.target.value)}
              placeholder="owner/name"
              spellCheck={false}
              autoComplete="off"
              className={cx(inputClass, "font-mono")}
            />
          </label>
          <label className="flex flex-col gap-1">
            <span className="font-mono text-[10px] tracking-[0.08em] text-faint">SWARM</span>
            <input
              type="number"
              min={1}
              max={8}
              value={swarm}
              onChange={(e) => setSwarm(Number(e.target.value))}
              aria-label="Swarm size"
              className={cx(inputClass, "w-20 font-mono")}
            />
          </label>
        </div>
        <input
          value={modules}
          onChange={(e) => setModules(e.target.value)}
          placeholder="hunter modules, comma-separated · a module manifest lands with #216"
          spellCheck={false}
          autoComplete="off"
          className={cx(inputClass, "font-mono")}
        />
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
          <label className="flex cursor-pointer items-center gap-1.5 text-[12.5px] text-muted">
            <input
              type="checkbox"
              checked={autofix}
              onChange={(e) => setAutofix(e.target.checked)}
              className="size-3.5 accent-[var(--accent)]"
            />
            Autofix
          </label>
          <span className="text-[11.5px] text-faint">— a raid never merges unless this is on</span>
          <span className="grow" />
          <Button size="sm" disabled={busy || !onStart} onClick={() => void launch(true)} title="Start gated: the swarm waits by the edge until no colony is live">
            Arm
          </Button>
          <Button size="sm" variant="primary" disabled={busy || !onStart} onClick={() => void launch(false)}>
            Start raid
          </Button>
        </div>
        {error && (
          <p role="alert" className="mt-0.5 text-[12.5px] text-err">
            {error}
          </p>
        )}
      </div>

      <div className="flex flex-col">
        {sorted.length === 0 ? (
          <div className="border-t border-border px-4 py-3 text-[12.5px] text-faint">No raids yet — arm one above.</div>
        ) : (
          sorted.map((run) => {
            const meta = RED_TEAM_STATE[run.state];
            const gated = isGated(run);
            const expanded = open === run.id;
            return (
              <div key={run.id} className="border-t border-border px-4 py-2.5">
                <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1">
                  <Badge tone={meta.tone}>{meta.label}</Badge>
                  <span className="font-mono text-[11.5px] text-muted">{run.repo}</span>
                  {run.autofix && (
                    <span className="font-mono text-[10.5px] text-faint" title="swarm may merge its finds">
                      merges on
                    </span>
                  )}
                  <span className="ml-auto whitespace-nowrap font-mono text-[11px] text-muted tabular-nums">
                    found {run.counts.found} · validated {run.counts.validated} · rejected {run.counts.rejected} · filed {run.counts.filed}
                  </span>
                  {isActive(run) && onStop && (
                    <Button size="sm" variant="danger" onClick={() => void onStop(run.id).catch(() => {})}>
                      Stop
                    </Button>
                  )}
                  <button
                    type="button"
                    onClick={() => setOpen(expanded ? null : run.id)}
                    aria-expanded={expanded}
                    className="font-mono text-[11px] text-accent hover:underline"
                  >
                    {run.hunters.length} {run.hunters.length === 1 ? "hunter" : "hunters"} {expanded ? "▾" : "▸"}
                  </button>
                </div>
                {gated && <p className="mt-1 text-[12px] text-warn">{gateMessage(run, live)}</p>}
                {expanded && (
                  <ul className="mt-1 flex flex-col gap-0.5">
                    {run.hunters.map((hunter) => (
                      <HunterRow
                        key={hunter.session_id}
                        hunter={hunter}
                        status={sessions.find((s) => s.id === hunter.session_id)?.status ?? null}
                        onOpenColony={onOpenColony}
                      />
                    ))}
                  </ul>
                )}
              </div>
            );
          })
        )}
      </div>
    </section>
  );
}

/** One hunter: title, module@version and focus, plus its live session status when it has one. */
function HunterRow({
  hunter,
  status,
  onOpenColony,
}: {
  hunter: RedTeamHunter;
  status: SessionStatus | null;
  onOpenColony: (id: string) => void;
}): ReactElement {
  const meta = status ? SESSION_STATUS[status] : null;
  const clickable = status !== null;
  return (
    <li>
      <button
        type="button"
        disabled={!clickable}
        onClick={() => clickable && onOpenColony(hunter.session_id)}
        title={clickable ? "open the colony" : "no colony behind this hunter"}
        className="grid w-full cursor-pointer grid-cols-[8px_minmax(0,1fr)_auto] items-center gap-2 rounded-lg px-2 py-1 text-left hover:bg-panel-2 disabled:cursor-default disabled:hover:bg-transparent"
      >
        <span aria-hidden="true" className="h-1.5 w-1.5 shrink-0 rounded-full" style={{ background: meta ? TONE_VAR[meta.tone] : "var(--faint)" }} />
        <span className="min-w-0">
          <span className="block truncate text-[12.5px]">{hunter.title}</span>
          <span className="block truncate font-mono text-[10.5px] text-faint">
            {hunter.module}
            {hunter.version ? `@${hunter.version}` : ""} · {hunter.focus}
          </span>
        </span>
        <span className="whitespace-nowrap font-mono text-[10.5px] text-muted">{meta?.label ?? "no colony"}</span>
      </button>
    </li>
  );
}