// Overview: every workspace and every colony on one page, for when the question is "what is going on
// everywhere" rather than "what is this nest doing". The rail's base button lands here.
//
// The prototype's rows carry a settler count. This one does not: the mothership streams events for a
// single colony at a time, so the only honest per-colony facts here are the ones in the list itself.
import type { ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import { IconAlert, IconCpu, IconMemory, IconServer } from "../components/icons";
import { SESSION_STATUS, type Tone, isLive, orgOf, sameOrg, timeAgo } from "../components/ui";
import { needsYou } from "../notifications";
import type { OrgEntry } from "../orgs";
import { sortSessions } from "../sessionOrder";
import { headlineFor } from "./feed";
import { colonyFacts, hostFacts } from "./host";
import type { HostInfo, Session } from "../types";

const TONE_VAR: Record<Tone, string> = {
  neutral: "var(--faint)",
  info: "var(--info)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  err: "var(--err)",
  accent: "var(--accent)",
};

const RETURNED = new Set(["pr_opened", "merged", "closed", "no_changes"]);

export function OverviewView({
  sessions,
  orgs,
  cost,
  host,
  onOpenOrg,
  onOpenColony,
}: {
  /** Every colony the mothership knows, unfiltered — this page is the cross-workspace view. */
  sessions: Session[];
  orgs: OrgEntry[];
  cost: number | null;
  /** The machine every listed colony boots on, polled with the status; a mothership before issue #205 sends none. */
  host?: HostInfo | null;
  onOpenOrg: (org: string) => void;
  onOpenColony: (id: string) => void;
}): ReactElement {
  const live = sessions.filter((s) => isLive(s.status)).length;
  const need = sessions.filter(needsYou).length;
  const returned = sessions.filter((s) => RETURNED.has(s.status)).length;
  const queued = sessions.filter((s) => s.status === "queued").length;

  return (
    <main className="cockpit min-h-0 overflow-y-auto px-6 pb-10 pt-7">
      <div className="mx-auto flex w-full max-w-[1080px] flex-col gap-5">
        <div className="flex flex-wrap items-baseline justify-between gap-4">
          <div>
            <div className="mb-1.5 font-mono text-[10.5px] tracking-[0.12em] text-faint">OVERVIEW</div>
            <div className="text-[20px] font-semibold tracking-tight">{headlineFor(need, live)}</div>
          </div>
          <div className="flex gap-4.5 font-mono text-xs text-muted tabular-nums">
            <span>
              <span className="text-accent">{live}</span> live
            </span>
            <span>
              <span className={need > 0 ? "text-warn" : "text-muted"}>{need}</span> need you
            </span>
            <span>
              <span className="text-ok">{returned}</span> returned
            </span>
            <span>
              <span className="text-text">{queued}</span> queued
            </span>
            {cost !== null && <span title="what every colony has spent in total">${cost.toFixed(2)} spent</span>}
          </div>
        </div>

        {host && (
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 rounded-xl border border-border bg-panel px-3.5 py-2">
            <span
              title={host.hostname ? `${host.hostname} · host ${host.id}` : `host ${host.id}`}
              className="font-mono text-[11px] font-semibold"
            >
              {host.hostname || host.id.slice(0, 8)}
            </span>
            {hostFacts(host).map((fact, i) => (
              <span key={i} title={fact.title} className="inline-flex items-center gap-1 font-mono text-[11px] text-faint">
                {fact.icon === "server" && <IconServer size={12} className="shrink-0" />}
                {fact.icon === "cpu" && <IconCpu size={12} className="shrink-0" />}
                {fact.icon === "memory" && <IconMemory size={12} className="shrink-0" />}
                {fact.value}
              </span>
            ))}
            {host.kvm_ok === false && (
              <span
                title="KVM is unavailable: this host cannot boot microVMs, so no colony can start here"
                className="inline-flex items-center gap-1 font-mono text-[11px] text-warn"
              >
                <IconAlert size={12} className="shrink-0" />
                no KVM
              </span>
            )}
            <span title="when the mothership last probed the machine" className="ml-auto font-mono text-[11px] text-faint">
              checked {timeAgo(host.checked_at)}
            </span>
          </div>
        )}

        <div className="grid gap-3.5 [grid-template-columns:repeat(auto-fill,minmax(300px,1fr))]">
          {orgs.map((org) => {
            const mine = sortSessions(sessions.filter((s) => sameOrg(orgOf(s), org.org)));
            return (
              <section key={org.org} className="flex flex-col overflow-hidden rounded-2xl border border-border bg-panel">
                <button
                  type="button"
                  onClick={() => onOpenOrg(org.org)}
                  className="grid cursor-pointer grid-cols-[30px_minmax(0,1fr)_auto] items-center gap-2.5 border-b border-border px-3.5 py-3 text-left hover:bg-panel-2"
                >
                  <Avatar name={org.org} src={org.avatar} size={30} rounded="lg" />
                  <span className="min-w-0">
                    <span className="block truncate font-semibold">{org.org}</span>
                    <span className="block font-mono text-[11px] text-faint">
                      {mine.length} {mine.length === 1 ? "colony" : "colonies"} · {org.live} live
                    </span>
                  </span>
                  <span className="whitespace-nowrap font-mono text-[11px] text-accent">open nest →</span>
                </button>

                <div className="flex flex-col">
                  {mine.length === 0 ? (
                    <div className="p-3.5 text-[12px] text-faint">No colonies yet.</div>
                  ) : (
                    mine.map((session) => {
                      const tone = SESSION_STATUS[session.status]?.tone ?? "neutral";
                      const edge = TONE_VAR[tone];
                      const short = `${session.repo.split("/")[1] ?? session.repo}${session.issue != null ? `#${session.issue}` : ""}`;
                      const meta = colonyFacts(session);
                      return (
                        <button
                          key={session.id}
                          type="button"
                          onClick={() => onOpenColony(session.id)}
                          className="grid cursor-pointer grid-cols-[8px_minmax(0,1fr)_auto] items-center gap-2.5 border-b border-border px-3.5 py-2.5 text-left last:border-b-0 hover:bg-panel-2"
                        >
                          <span aria-hidden="true" className="h-2 w-2 rounded-full" style={{ background: edge }} />
                          <span className="min-w-0">
                            <span className="block truncate text-[13px]">{session.issue_title || short}</span>
                            <span className="block truncate font-mono text-[11px] text-faint">
                              {short} · {timeAgo(session.last_activity_at ?? session.updated_at)}
                            </span>
                            {meta.length > 0 && (
                              <span className="mt-0.5 flex flex-wrap items-center gap-x-2.5 gap-y-0.5 font-mono text-[11px] text-faint">
                                {meta.map((fact, i) => (
                                  <span key={i} title={fact.title} className="inline-flex items-center gap-1 whitespace-nowrap">
                                    {fact.icon === "cpu" && <IconCpu size={11} className="shrink-0" />}
                                    {fact.value}
                                  </span>
                                ))}
                              </span>
                            )}
                          </span>
                          <span className="whitespace-nowrap font-mono text-[11px]" style={{ color: edge }}>
                            {SESSION_STATUS[session.status]?.label ?? session.status}
                          </span>
                        </button>
                      );
                    })
                  )}
                </div>
              </section>
            );
          })}
        </div>
      </div>
    </main>
  );
}
