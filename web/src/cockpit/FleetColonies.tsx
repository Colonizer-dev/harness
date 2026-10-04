// The fleet colony list (issue #689): the FleetPanel's host stack lists machines; this lists what
// runs on them, directly under it on the Overview. Rows are the pure `FleetColony`s from
// fleetColoniesModel.ts (this host's sessions, plus the members' pushed history); this file is only the
// fetch and the markup. A member pushes only finished colonies, so every imported row is done.
import { useEffect, useMemo, useState, type ReactElement } from "react";

import { SESSION_STATUS } from "../components/ui";
import { useApi } from "../context";
import { formatCost } from "../spend";
import type { FleetHost, Session } from "../types";
import {
  filterColonies, fromImported, fromSession, hostOptions, mergeFleetColonies, totalsBy,
  type FleetColony, type FleetTotal, type WaitingOn,
} from "./fleetColoniesModel";

/** Imported history to read: 5 pages of 100, newest first (older rows live in Settings → Fleet history). */
const PAGE_LIMIT = 100;
const MAX_PAGES = 5;

const WAITING_LABEL: Record<WaitingOn, string> = { answer: "answer", slot: "slot", quota: "quota", ci: "CI", review: "review" };

/**
 * The imported rows: the members' finished colonies, owner-only (a member's `fleet` role reads
 * none). The fetch re-runs when the fleet meaningfully changes — keyed on each host's identity,
 * health and history-sync state, not on the `hosts` array, whose `last_heartbeat` is refreshed every
 * poll (so array identity alone would be a ~5 s poll). A member pushing history moves its
 * `fleet_sync` — `backlog_rows` rises as a colony finishes and falls as the drain sends it
 * (fleet_sync.rs `summary_of`) — which re-publishes the frame with a new key and re-runs this. A
 * failed read (no fleet API, not the owner, offline) yields no imported rows.
 */
export function useFleetColonies(hosts: FleetHost[]): FleetColony[] {
  const api = useApi();
  const [imported, setImported] = useState<FleetColony[]>([]);
  const key = hosts
    .map((h) => [h.id, h.health, h.fleet_sync?.state ?? "", h.fleet_sync?.backlog_rows ?? ""].join("\u0000"))
    .join("\u0001");

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const state = await api.fleet();
        if (state.role !== "owner") return void (!cancelled && setImported([]));
        const urls = new Map(state.members.map((m) => [m.id, m.url]));
        const rows: FleetColony[] = [];
        let cursor: string | undefined;
        for (let page = 0; page < MAX_PAGES; page++) {
          if (cancelled) return;
          const res = await api.fleetHistory({ limit: PAGE_LIMIT, cursor });
          rows.push(...res.colonies.map((entry) => fromImported(entry, urls)));
          if (!res.next_cursor) break;
          cursor = res.next_cursor;
        }
        if (!cancelled) setImported(rows);
      } catch {
        if (!cancelled) setImported([]);
      }
    })();
    return () => { cancelled = true; };
  }, [api, key]);

  return imported;
}

/** Builds this host's rows from the live sessions, folds in the imported history, and renders
 *  nothing for a single-machine install with no imported rows — FleetPanel's same hide rule. */
export function FleetColonies({ hosts, sessions }: { hosts: FleetHost[]; sessions: Session[] }): ReactElement | null {
  const imported = useFleetColonies(hosts);
  const selfHost = hosts[0]?.name ?? "this host";
  const local = useMemo(() => sessions.map((s) => fromSession(s, selfHost)), [sessions, selfHost]);
  const colonies = useMemo(() => mergeFleetColonies(local, imported), [local, imported]);
  if (hosts.length <= 1 && imported.length === 0) return null;
  return <FleetColoniesView colonies={colonies} hosts={hosts} />;
}

/** One filter dropdown; the empty value is "all". */
function Filter({ label, value, options, onChange }: { label: string; value: string; options: string[]; onChange: (v: string) => void }): ReactElement {
  return (
    <label className="inline-flex items-center gap-1.5 text-[11.5px] text-faint">
      {label}
      <select value={value} onChange={(e) => onChange(e.target.value)} className="max-w-[160px] cursor-pointer rounded-md border border-border bg-panel-2 px-1.5 py-1 text-[11.5px] text-text">
        <option value="">All</option>
        {options.map((o) => <option key={o} value={o}>{o}</option>)}
      </select>
    </label>
  );
}

/** One total block: a label and its `key · count · cost` lines ("—" when empty). */
function Totals({ title, totals }: { title: string; totals: FleetTotal[] }): ReactElement {
  return (
    <div className="min-w-0">
      <div className="mb-1 font-mono text-[10.5px] tracking-[0.12em] text-faint">{title.toUpperCase()}</div>
      {totals.length === 0 ? <div className="text-[12px] text-faint">—</div> : totals.map((t) => (
        <div key={t.key} className="flex items-baseline justify-between gap-2 text-[12px] tabular-nums">
          <span className="truncate" title={t.key}>{t.key}</span>
          <span className="shrink-0 text-faint">{`${t.colonies} · ${formatCost(t.costUsd)}`}</span>
        </div>
      ))}
    </div>
  );
}

/** One colony row; the link opens the colony on its host (a new tab for a member, in place here). */
function ColonyRow({ c }: { c: FleetColony }): ReactElement {
  return (
    <tr className="border-t border-border">
      <td className="px-3.5 py-2 font-mono text-[11px]">
        <span className="block max-w-[150px] truncate" title={c.host}>{c.host}</span>
      </td>
      <td className="px-3.5 py-2">
        <span className="block max-w-[380px] truncate" title={c.title}>{c.title}</span>
        <span className="block truncate font-mono text-[11px] text-faint">{`${c.repo}${c.issue != null ? `#${c.issue}` : ""}`}</span>
      </td>
      <td className="px-3.5 py-2 text-muted">{SESSION_STATUS[c.status]?.label ?? c.status}</td>
      <td className={`px-3.5 py-2 ${c.waitingOn ? "text-warn" : "text-faint"}`}>{c.waitingOn ? WAITING_LABEL[c.waitingOn] : "—"}</td>
      <td className="px-3.5 py-2 text-right tabular-nums">{formatCost(c.costUsd)}</td>
      <td className="px-3.5 py-2 text-right">
        {c.url ? (
          <a href={c.url} {...(c.imported ? { target: "_blank", rel: "noopener noreferrer" } : {})} className="whitespace-nowrap text-muted underline underline-offset-[3px] hover:text-text">
            {c.imported ? "Open on host" : "Open"}
          </a>
        ) : <span className="text-faint" title={c.removed ? "this member was removed from the fleet" : "this member gave no URL"}>—</span>}
      </td>
    </tr>
  );
}

/** The fleet colony list: filters, cost totals per repo/host/day, then the rows. Presentational —
 *  `colonies` is already built — so the tests render it directly with no API. */
export function FleetColoniesView({ colonies, hosts }: { colonies: FleetColony[]; hosts: FleetHost[] }): ReactElement {
  const [host, setHost] = useState("");
  const [org, setOrg] = useState("");
  const [repo, setRepo] = useState("");
  const hostsOpts = useMemo(() => hostOptions(colonies, hosts), [colonies, hosts]);
  const orgOpts = useMemo(() => [...new Set(colonies.map((c) => c.org))].sort((a, b) => a.localeCompare(b)), [colonies]);
  const repoOpts = useMemo(() => [...new Set(colonies.map((c) => c.repo))].sort((a, b) => a.localeCompare(b)), [colonies]);
  const filtered = useMemo(() => filterColonies(colonies, { host, org, repo }), [colonies, host, org, repo]);
  const byRepo = useMemo(() => totalsBy(filtered, "repo"), [filtered]);
  const byHost = useMemo(() => totalsBy(filtered, "host", hosts.map((h) => h.name)), [filtered, hosts]);
  const byDay = useMemo(() => totalsBy(filtered, "day"), [filtered]);

  return (
    <div className="flex flex-col overflow-hidden rounded-xl border border-border bg-panel">
      <div className="border-b border-border px-3.5 py-2 font-mono text-[10.5px] tracking-[0.12em] text-faint">
        FLEET COLONIES · {colonies.length}
      </div>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-border px-3.5 py-2">
        <Filter label="Host" value={host} options={hostsOpts} onChange={setHost} />
        <Filter label="Org" value={org} options={orgOpts} onChange={setOrg} />
        <Filter label="Repo" value={repo} options={repoOpts} onChange={setRepo} />
        <span className="ml-auto font-mono text-[11px] text-faint">{`${filtered.length} of ${colonies.length} · older history in Settings → Fleet history`}</span>
      </div>
      <div className="grid gap-4 border-b border-border px-3.5 py-2.5 @min-[1100px]:grid-cols-3">
        <Totals title="By repo" totals={byRepo} />
        <Totals title="By host" totals={byHost} />
        <Totals title="By day" totals={byDay} />
      </div>
      {filtered.length === 0 ? (
        <div className="px-3.5 py-3.5 text-[12.5px] text-faint">No colonies match these filters.</div>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[680px] border-collapse text-[12.5px]">
            <thead>
              <tr className="border-b border-border text-left text-[11px] text-muted">
                <th scope="col" className="px-3.5 pb-2 pt-2.5 font-normal">Host</th>
                <th scope="col" className="px-3.5 pb-2 pt-2.5 font-normal">Colony</th>
                <th scope="col" className="px-3.5 pb-2 pt-2.5 font-normal">Status</th>
                <th scope="col" className="px-3.5 pb-2 pt-2.5 font-normal">Waiting on</th>
                <th scope="col" className="px-3.5 pb-2 pt-2.5 text-right font-normal">Cost</th>
                <th scope="col" className="px-3.5 pb-2 pt-2.5 text-right font-normal">Link</th>
              </tr>
            </thead>
            <tbody>{filtered.map((c) => <ColonyRow key={c.key} c={c} />)}</tbody>
          </table>
        </div>
      )}
    </div>
  );
}
