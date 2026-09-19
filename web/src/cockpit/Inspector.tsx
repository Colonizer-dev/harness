// The pane that slides in beside the nest when you pick the mothership or a chamber: what it is,
// what it costs, what it is waiting for, and the one or two things worth doing about it.
//
// Everything here comes from the colony list and /api/status. There is deliberately no per-colony
// event history: the mothership keeps none for the browser, and a made-up one would read as fact.
import type { ReactElement } from "react";

import { AntAvatar } from "../components/AntAvatar";
import { Avatar } from "../components/Avatar";
import type { SectionId } from "../components/SettingsDialog";
import { SESSION_STATUS, type Tone, isLive, timeAgo } from "../components/ui";
import { needsYou } from "../notifications";
import type { SubagentView } from "../sessionStream";
import type { HarnessStatus, Session, UpdateStatus } from "../types";

const TONE_VAR: Record<Tone, string> = {
  neutral: "var(--faint)",
  info: "var(--info)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  err: "var(--err)",
  accent: "var(--accent)",
};

export type InspectorTarget = { kind: "mothership" } | { kind: "colony"; session: Session };

function Fact({ label, value }: { label: string; value: string }): ReactElement {
  return (
    <div className="bg-panel px-3 py-2.5">
      <div className="font-mono text-[10px] tracking-[0.1em] text-faint">{label}</div>
      <div className="mt-0.5 truncate font-mono text-[12.5px] text-text">{value}</div>
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactElement | ReactElement[] }): ReactElement {
  return (
    <div>
      <div className="mb-2 font-mono text-[10.5px] tracking-[0.12em] text-faint">{title}</div>
      {children}
    </div>
  );
}

const money = (amount: number | null | undefined) => (amount == null ? "—" : `$${amount.toFixed(2)}`);

/** `#149` from a pull-request URL; null when it is not shaped like one. */
function prNumber(url: string): string | null {
  const match = /\/pull\/(\d+)/.exec(url);
  return match ? `#${match[1]}` : null;
}

/** How far the last publish got, in the mothership's own words. */
const STAGE: Record<string, string> = {
  committed: "committed",
  pushed: "pushed",
  pr_opened: "opened",
};

export function Inspector({
  target,
  avatarUrl,
  settlers,
  status,
  liveCount,
  queuedCount,
  needCount,
  spend,
  maxParallel,
  update,
  onClose,
  onOpenColony,
  onStop,
  onResume,
  onLaunch,
  onOpenSettings,
}: {
  target: InspectorTarget;
  /** The colony's org avatar; null when nothing knows one and the initial stands in. */
  avatarUrl: string | null;
  /** Real settlers, present only while this colony's stream is open; empty otherwise. */
  settlers: SubagentView[];
  status: HarnessStatus | null;
  liveCount: number;
  queuedCount: number;
  needCount: number;
  /** What this workspace's colonies have spent in total; the API keeps no daily history. */
  spend: number | null;
  maxParallel: number | null;
  update: UpdateStatus | null;
  onClose: () => void;
  onOpenColony: (id: string) => void;
  onStop: (id: string) => void;
  onResume: (id: string) => void;
  onLaunch: () => void;
  onOpenSettings: (section: SectionId) => void;
}): ReactElement {
  const mothership = target.kind === "mothership";
  const session = target.kind === "colony" ? target.session : null;
  const tone = session ? (SESSION_STATUS[session.status]?.tone ?? "neutral") : "neutral";
  const edge = TONE_VAR[tone];

  // Only rows the mothership actually reported: an absent fact is left out rather than guessed at.
  const connections: { label: string; dot: string; section: SectionId }[] = [];
  if (status) {
    connections.push({
      label: status.github.connected ? `@${status.github.login ?? "github"}` : "github · not connected",
      dot: status.github.connected ? "var(--ok)" : "var(--warn)",
      section: "connections",
    });
    connections.push({
      label: status.claude.configured ? `claude · ${status.claude.kind ?? "configured"}` : "claude · not configured",
      dot: status.claude.configured ? "var(--ok)" : "var(--warn)",
      section: "connections",
    });
    if (status.sandbox.msb_version) {
      connections.push({ label: `microvms · msb ${status.sandbox.msb_version}`, dot: "var(--ok)", section: "runtime" });
    }
    if (status.mesh?.enabled) {
      connections.push({
        label: `mesh · ${status.mesh.nodes ?? 0} nodes`,
        dot: status.mesh.error ? "var(--err)" : "var(--ok)",
        section: "module:mesh",
      });
    }
  }

  return (
    <aside
      className="cockpit flex w-[360px] shrink-0 flex-col overflow-hidden border-l border-border bg-panel"
      style={{ animation: "ck-slide 0.28s cubic-bezier(.2,.7,.2,1) both" }}
      aria-label={mothership ? "mothership" : "colony"}
    >
      <div className="flex items-start gap-3 border-b border-border px-4 pb-3 pt-4">
        {mothership ? (
          <span className="grid h-[34px] w-[34px] shrink-0 place-items-center rounded-[9px] bg-accent-soft text-accent">
            <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 2.8 20 7.4v9.2L12 21.2 4 16.6V7.4z" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round" />
              <circle cx="12" cy="12" r="2.6" fill="currentColor" />
            </svg>
          </span>
        ) : (
          session && <Avatar name={session.repo.split("/")[0]} src={avatarUrl} size={34} rounded="lg" />
        )}
        <div className="min-w-0 flex-1">
          <div className="truncate font-mono text-[11.5px] text-muted">
            {mothership ? "mothership" : `${session?.repo}${session?.issue != null ? `#${session.issue}` : ""}`}
          </div>
          <div className="mt-0.5 text-[14.5px] font-semibold leading-tight text-pretty">
            {mothership ? "the colonizer app on this machine" : session?.issue_title || "no title yet"}
          </div>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="close"
          className="grid h-[26px] w-[26px] shrink-0 cursor-pointer place-items-center rounded-[7px] text-faint hover:bg-panel-2 hover:text-text"
        >
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" aria-hidden="true">
            <path d="M6 6l12 12M18 6 6 18" />
          </svg>
        </button>
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-4 pb-4 pt-3.5">
        {mothership ? (
          <>
            <div className="grid shrink-0 grid-cols-2 gap-px overflow-hidden rounded-xl border border-border bg-border">
              <Fact label="LIVE" value={maxParallel != null ? `${liveCount} / ${maxParallel}` : String(liveCount)} />
              <Fact label="QUEUED" value={String(queuedCount)} />
              <Fact label="NEED YOU" value={String(needCount)} />
              <Fact label="SPENT" value={money(spend)} />
            </div>

            {connections.length > 0 && (
              <Section title="CONNECTIONS">
                <div className="flex flex-col gap-0.5">
                  {connections.map((row) => (
                    <button
                      key={row.label}
                      type="button"
                      onClick={() => onOpenSettings(row.section)}
                      className="flex cursor-pointer items-center gap-2.5 rounded-[9px] bg-panel-2 px-2.5 py-2 text-left font-mono text-[12px] text-muted hover:text-text"
                    >
                      <span aria-hidden="true" className="h-1.5 w-1.5 rounded-full" style={{ background: row.dot }} />
                      <span className="flex-1 truncate">{row.label}</span>
                      <span aria-hidden="true" className="text-faint">
                        ›
                      </span>
                    </button>
                  ))}
                </div>
              </Section>
            )}

            {update && (
              <button
                type="button"
                onClick={() => onOpenSettings("updates")}
                className={`flex cursor-pointer items-center gap-2.5 rounded-xl border bg-panel-2 px-3 py-2.5 text-left ${
                  update.available ? "border-accent" : "border-border"
                }`}
              >
                <span className="min-w-0 flex-1">
                  <span className="block text-[13px] font-semibold">colonizer {update.installed.version}</span>
                  <span className="block truncate text-[12px] text-muted">
                    {update.available && update.latest ? `${update.latest.version} is out` : "up to date"}
                  </span>
                </span>
                <span className="text-[12.5px] font-semibold text-accent">{update.available ? "update" : "about"}</span>
              </button>
            )}
          </>
        ) : (
          session && (
            <>
              <div className="flex items-center gap-2 font-mono text-[11px] tracking-[0.1em]" style={{ color: edge }}>
                <span aria-hidden="true" className="h-[7px] w-[7px] rounded-full" style={{ background: edge }} />
                {SESSION_STATUS[session.status]?.label ?? session.status}
                <span className="tracking-normal text-faint">· {timeAgo(session.created_at)}</span>
              </div>

              {needsYou(session) && (
                <div className="rounded-xl border border-warn bg-warn-soft px-3.5 py-3">
                  <div className="font-mono text-[10.5px] tracking-[0.14em] text-warn">WAITING ON YOU</div>
                  <div className="mt-1.5 text-[13.5px] font-semibold">
                    {session.attention ? "the watchdog flagged this colony" : "the colony asked you a question"}
                  </div>
                  {/* Answering belongs with the question, which is in the colony's own chat. */}
                  <button
                    type="button"
                    onClick={() => onOpenColony(session.id)}
                    className="mt-2.5 w-full cursor-pointer rounded-[9px] border border-border bg-panel px-3 py-2 text-left text-[12.5px] font-semibold hover:border-accent"
                  >
                    open the colony and answer →
                  </button>
                </div>
              )}

              <div className="grid shrink-0 grid-cols-2 gap-px overflow-hidden rounded-xl border border-border bg-border">
                <Fact label="SETTLERS" value={settlers.length > 0 ? String(settlers.length) : "—"} />
                <Fact label="COST" value={money(session.cost_usd)} />
                <Fact label="MESH" value={session.mesh?.name ?? "—"} />
                <Fact label="AGENT" value={session.agent} />
              </div>

              {session.pr_url && (
                // What came back. The prototype lists per-file +/- counts; the API reports no diff
                // stats, so this carries the address and the branch it came from instead of inventing them.
                <div className="flex flex-col gap-1.5 rounded-xl border border-border bg-panel-2 px-3.5 py-3">
                  <div className="flex items-center justify-between gap-2.5">
                    <span className="font-mono text-[10.5px] tracking-[0.12em] text-ok">PULL REQUEST</span>
                    <span className="font-mono text-[11px] text-faint">
                      {[prNumber(session.pr_url), session.publish_stage ? STAGE[session.publish_stage] : null]
                        .filter(Boolean)
                        .join(" · ")}
                    </span>
                  </div>
                  <div className="text-[13.5px] font-semibold leading-snug">{session.issue_title || "no title yet"}</div>
                  <div className="truncate font-mono text-[11.5px] text-muted">
                    {session.branch}
                    {session.base ? ` → ${session.base}` : ""}
                  </div>
                  <a
                    href={session.pr_url}
                    target="_blank"
                    rel="noreferrer"
                    className="text-[12.5px] font-semibold text-accent no-underline hover:underline"
                  >
                    open on github ↗
                  </a>
                </div>
              )}

              {settlers.length > 0 && (
                <Section title="SETTLERS">
                  <div className="flex flex-col gap-0.5">
                    {settlers.map((settler, i) => (
                      <div
                        key={settler.agent.id}
                        className="grid grid-cols-[40px_minmax(0,1fr)_auto] items-center gap-2.5 rounded-[10px] bg-panel-2 px-2 py-1.5"
                      >
                        <span
                          className="grid h-[30px] w-10 place-items-center rounded-lg transition-colors duration-700"
                          style={{ background: settler.state === "done" ? "var(--ok-soft)" : "var(--accent-soft)" }}
                        >
                          <AntAvatar state={settler.state} role={settler.role} size={32} phase={i} ground={false} framed={false} />
                        </span>
                        <span className="min-w-0">
                          <span className="block text-[12.5px] font-semibold text-accent">{settler.name}</span>
                          <span className="block truncate font-mono text-[11px] text-muted">
                            {settler.current?.name ?? settler.last?.name ?? "—"}
                          </span>
                        </span>
                        <span className="whitespace-nowrap font-mono text-[10px] text-faint tabular-nums">
                          {settler.steps} steps
                        </span>
                      </div>
                    ))}
                  </div>
                </Section>
              )}

              <Section title="TIMELINE">
                <div className="flex flex-col gap-1.5">
                  <div className="flex gap-2.5 text-[12px] text-muted">
                    <span className="w-14 shrink-0 font-mono text-[11px] text-faint">started</span>
                    <span className="min-w-0">{timeAgo(session.created_at)}</span>
                  </div>
                  <div className="flex gap-2.5 text-[12px] text-muted">
                    <span className="w-14 shrink-0 font-mono text-[11px] text-faint">last move</span>
                    <span className="min-w-0">{timeAgo(session.last_activity_at ?? session.updated_at)}</span>
                  </div>
                  {session.error && (
                    <div className="flex gap-2.5 text-[12px] text-err">
                      <span className="w-14 shrink-0 font-mono text-[11px]">error</span>
                      <span className="min-w-0 [overflow-wrap:anywhere]">{session.error}</span>
                    </div>
                  )}
                </div>
              </Section>
            </>
          )
        )}
      </div>

      <div className="flex gap-2 border-t border-border px-4 py-3">
        {mothership ? (
          <>
            <button
              type="button"
              onClick={onLaunch}
              className="flex-1 cursor-pointer rounded-[10px] bg-accent px-3 py-2.5 text-[12.5px] font-semibold text-on-accent hover:brightness-110"
            >
              launch a colony
            </button>
            <button
              type="button"
              onClick={() => onOpenSettings("setup")}
              className="cursor-pointer rounded-[10px] border border-border-strong px-3 py-2.5 text-[12.5px] hover:border-accent"
            >
              settings
            </button>
          </>
        ) : (
          session && (
            <>
              <button
                type="button"
                onClick={() => onOpenColony(session.id)}
                className="flex-1 cursor-pointer rounded-[10px] bg-accent px-3 py-2.5 text-[12.5px] font-semibold text-on-accent hover:brightness-110"
              >
                open colony
              </button>
              {isLive(session.status) && (
                <button
                  type="button"
                  onClick={() => onStop(session.id)}
                  title="stop the microvm; the worktree is kept"
                  className="cursor-pointer rounded-[10px] border border-border-strong px-3 py-2.5 text-[12.5px] text-muted hover:text-text"
                >
                  stop
                </button>
              )}
              {/* The same condition the colony view's Resume button uses, so the two cannot disagree
                  about whether a colony can be picked back up. */}
              {!isLive(session.status) && !session.cleaned_up && (session.status === "stopped" || session.status === "failed") && (
                <button
                  type="button"
                  onClick={() => onResume(session.id)}
                  className="cursor-pointer rounded-[10px] border border-border-strong px-3 py-2.5 text-[12.5px] hover:border-accent"
                >
                  resume
                </button>
              )}
              {session.pr_url && (
                <a
                  href={session.pr_url}
                  target="_blank"
                  rel="noreferrer"
                  className="cursor-pointer rounded-[10px] border border-border-strong px-3 py-2.5 text-[12.5px] font-semibold no-underline hover:border-accent"
                >
                  pr ↗
                </a>
              )}
            </>
          )
        )}
      </div>
    </aside>
  );
}
