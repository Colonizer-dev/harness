// The host: the machine every colony here boots on, in the v3 dashboard idiom. Everything is a reading
// the mothership already reports — GET /api/status `host`, `runtime`, `sandbox` and `mesh`, GET
// /api/storage, the fleet list and each colony's own boot size and disk footprint. The mothership keeps
// no host history, so the trend lines are the readings taken since this page opened (hostHistory.ts).
import { useEffect, useState, type ReactElement, type ReactNode } from "react";

import { useApi } from "../context";
import type { SectionId } from "../components/SettingsDialog";
import { SESSION_STATUS, isLive, timeAgo } from "../components/ui";
import type { FleetHost, HarnessStatus, Session, StorageSummary } from "../types";
import { KpiStrip, Rules, Section, type KpiDef } from "./DashChart";
import { sparkPoints, TONE_VAR } from "./dash";
import { formatBytes, formatUptime } from "./host";
import { parseSize, useHostHistory, type HostSample } from "./hostHistory";

const pct = (v: number | null | undefined) => (v == null || !Number.isFinite(v) ? "—" : `${Math.round(v * 100)}%`);

/** A 0–1 reading's tone: calm, then warn from 75%, bad from 90%. */
function loadTone(v: number | null): "good" | "bad" | "flat" {
  if (v == null) return "flat";
  return v >= 0.9 ? "bad" : "flat";
}

function trend(history: readonly HostSample[], pick: (s: HostSample) => number | null): string | undefined {
  return history.length >= 2 ? sparkPoints(history.map(pick), 100, 28, 1) : undefined;
}

/** What the live colonies here were booted with, against the machine. */
export function allocation(sessions: readonly Session[]): { colonies: number; cpus: number; memory: number; unknown: number } {
  let cpus = 0;
  let memory = 0;
  let unknown = 0;
  const live = sessions.filter((s) => isLive(s.status));
  for (const s of live) {
    const mem = parseSize(s.boot_memory);
    if (s.boot_cpus == null && mem == null) unknown += 1;
    cpus += s.boot_cpus ?? 0;
    memory += mem ?? 0;
  }
  return { colonies: live.length, cpus, memory, unknown };
}

/** One resource: label, numbers, and a used-of-capacity bar that turns warn/err as it fills. */
function Meter({ label, used, total, detail }: { label: string; used: number | null; total: number | null; detail: ReactNode }): ReactElement {
  const ratio = used != null && total != null && total > 0 ? Math.min(1, used / total) : null;
  const color = ratio == null ? "var(--panel-3)" : ratio >= 0.9 ? "var(--err)" : ratio >= 0.75 ? "var(--warn)" : "var(--chart-1)";
  return (
    <div className="grid grid-cols-[120px_minmax(0,1fr)_auto] items-center gap-4 border-t border-border py-3.5 first:border-t-0 max-sm:grid-cols-[minmax(0,1fr)_auto]">
      <span className="text-[13.5px] text-text">{label}</span>
      <span className="h-1.5 overflow-hidden rounded-full bg-panel-3 max-sm:col-span-2 max-sm:row-start-2">
        <span className="block h-full rounded-full transition-[width] duration-700 ease-out" style={{ width: `${(ratio ?? 0) * 100}%`, background: color }} />
      </span>
      <span className="whitespace-nowrap text-right text-[13px] tabular-nums text-muted">{detail}</span>
    </div>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }): ReactElement {
  return (
    <div className="flex min-w-0 flex-col gap-1 border-t border-border py-3 [&:nth-child(-n+2)]:border-t-0 sm:[&:nth-child(-n+3)]:border-t-0">
      <span className="text-[12.5px] text-faint">{label}</span>
      <span className="min-w-0 truncate text-[13.5px] text-text">{children}</span>
    </div>
  );
}

function ok(flag: boolean | undefined | null, yes: string, no: string): ReactElement {
  return <span className={flag ? "text-text" : "text-err"}>{flag ? yes : no}</span>;
}

export function HostView({
  status,
  fleet = [],
  sessions,
  liveStorage = null,
  onOpenColony,
  onOpenSettings,
}: {
  status: HarnessStatus | null;
  fleet?: FleetHost[];
  sessions: Session[];
  /** A storage frame the stream pushed; replaces the page's own fetch while present. */
  liveStorage?: StorageSummary | null;
  onOpenColony: (id: string) => void;
  onOpenSettings?: (section: SectionId) => void;
}): ReactElement {
  const api = useApi();
  const history = useHostHistory();
  const [fetched, setFetched] = useState<StorageSummary | null>(null);
  useEffect(() => {
    let cancelled = false;
    api
      .storageSummary()
      .then((s) => !cancelled && setFetched(s))
      .catch(() => {
        /* the storage section says it has no reading */
      });
    return () => {
      cancelled = true;
    };
  }, [api]);
  const storage = liveStorage ?? fetched;

  const host = status?.host ?? null;
  const runtime = status?.runtime;
  const sandbox = status?.sandbox;
  const mesh = status?.mesh;
  const last = history[history.length - 1] ?? null;
  const alloc = allocation(sessions);
  const coloniesDisk = sessions.reduce((t, s) => t + (s.host_disk_bytes ?? 0), 0);
  const liveHere = sessions.filter((s) => isLive(s.status)).sort((a, b) => (b.boot_cpus ?? 0) - (a.boot_cpus ?? 0));
  const queued = sessions.filter((s) => s.status === "queued").length;
  const os = runtime?.os ? `${runtime.os.name}${runtime.os.version ? ` ${runtime.os.version}` : ""}` : null;

  if (!host) {
    return (
      <main className="cockpit min-h-0 overflow-y-auto px-6 pb-24 pt-10">
        <div className="mx-auto w-full max-w-[1080px]">
          <h1 className="m-0 text-[30px] font-semibold leading-[1.15] tracking-[-0.035em]">Host</h1>
          <p className="mt-2 text-[14px] text-muted">{status ? "This mothership does not report its host yet (it needs a build from issue #205 on)." : "Waiting for the mothership…"}</p>
        </div>
      </main>
    );
  }

  const cpuRatio = last?.cpu ?? (host.load && host.cpu_cores ? host.load[0] / host.cpu_cores : null);
  const memRatio = host.memory_total_bytes ? (host.memory_used_bytes ?? 0) / host.memory_total_bytes : null;
  const diskRatio = host.disk_total_bytes ? (host.disk_used_bytes ?? 0) / host.disk_total_bytes : null;

  const kpis: KpiDef[] = [
    {
      label: "CPU load",
      value: host.load ? host.load[0].toFixed(2) : "—",
      valueNum: host.load?.[0],
      formatNum: (n) => n.toFixed(2),
      delta: cpuRatio != null ? pct(cpuRatio) : undefined,
      deltaTone: loadTone(cpuRatio),
      spark: trend(history, (s) => s.cpu),
      sub: host.cpu_cores != null ? `1-min load on ${host.cpu_cores} cores` : "1-min load",
      hint: "1-minute load average; the % is load ÷ cores",
    },
    {
      label: "Memory",
      value: host.memory_used_bytes != null ? formatBytes(host.memory_used_bytes) : "—",
      delta: memRatio != null ? pct(memRatio) : undefined,
      deltaTone: loadTone(memRatio),
      spark: trend(history, (s) => s.memory),
      sub: host.memory_total_bytes != null ? `of ${formatBytes(host.memory_total_bytes)}` : undefined,
      hint: "memory in use on the host",
    },
    {
      label: "Disk",
      value: host.disk_free_bytes != null ? `${formatBytes(host.disk_free_bytes)} free` : "—",
      delta: diskRatio != null ? `${pct(diskRatio)} used` : undefined,
      deltaTone: loadTone(diskRatio),
      spark: trend(history, (s) => s.disk),
      sub: host.disk_total_bytes != null ? `of ${formatBytes(host.disk_total_bytes)}` : undefined,
      hint: "the data directory's volume",
    },
    {
      label: "microVMs",
      value: `${host.microvms_live}/${host.microvms_ceiling}`,
      spark: trend(history, (s) => s.microvms),
      sub: queued > 0 ? `${queued} queued` : "none queued",
      hint: "microVMs booted / the parallel limit",
    },
  ];

  const storageRows = storage
    ? [
        { label: "Worktrees", bytes: storage.totals.worktrees_bytes, color: "var(--chart-1)" },
        { label: "Repositories", bytes: storage.totals.repos_bytes, color: "var(--chart-2)" },
        { label: "Colony sessions", bytes: storage.totals.sessions_bytes, color: "var(--chart-4)" },
        ...(storage.totals.microsandbox_bytes != null ? [{ label: "microsandbox (images)", bytes: storage.totals.microsandbox_bytes, color: "var(--chart-3)" }] : []),
      ]
    : [];
  const storageTotal = storageRows.reduce((t, r) => t + r.bytes, 0);
  const reclaimBytes = storage ? storage.reclaimable.reduce((t, r) => t + r.bytes, 0) : 0;

  return (
    <main className="cockpit min-h-0 overflow-y-auto px-6 pb-24 pt-10">
      <div className="mx-auto flex w-full max-w-[1080px] flex-col gap-10">
        <div>
          <h1 className="m-0 truncate text-[30px] font-semibold leading-[1.15] tracking-[-0.035em]">{host.hostname ?? "Host"}</h1>
          <div className="mt-2 text-[14px] text-muted">
            {[os, runtime?.platform, host.uptime_secs != null ? `up ${formatUptime(host.uptime_secs)}` : null, `checked ${timeAgo(host.checked_at)}`].filter(Boolean).join(" · ")}
          </div>
        </div>

        <KpiStrip items={kpis} note={history.length < 2 ? "Trend lines fill in while this page is open — the mothership keeps no host history." : `Trends: the last ${history.length} readings since this page opened.`} />

        <Section title="Resources" meta={host.kvm_ok === false ? <span className="text-err">KVM unavailable — colonies cannot boot here</span> : undefined}>
          <Rules>
            <Meter
              label="CPU"
              used={host.load?.[0] ?? null}
              total={host.cpu_cores ?? null}
              detail={host.load ? `${host.load.map((l) => l.toFixed(2)).join(" · ")} (1/5/15 min) · ${host.cpu_cores ?? "?"} cores` : "—"}
            />
            <Meter
              label="Memory"
              used={host.memory_used_bytes ?? null}
              total={host.memory_total_bytes ?? null}
              detail={host.memory_total_bytes != null ? `${formatBytes(host.memory_used_bytes ?? 0)} of ${formatBytes(host.memory_total_bytes)}` : "—"}
            />
            <Meter
              label="Disk"
              used={host.disk_used_bytes ?? null}
              total={host.disk_total_bytes ?? null}
              detail={host.disk_total_bytes != null ? `${formatBytes(host.disk_used_bytes ?? 0)} used · ${formatBytes(host.disk_free_bytes ?? 0)} free` : "—"}
            />
            <Meter label="microVM slots" used={host.microvms_live} total={host.microvms_ceiling} detail={`${host.microvms_live} of ${host.microvms_ceiling}${queued ? ` · ${queued} queued` : ""}`} />
          </Rules>
        </Section>

        <Section
          title="Colonies on this host"
          meta={`${alloc.colonies} live${alloc.unknown ? ` · ${alloc.unknown} booted before sizes were recorded` : ""}`}
        >
          <Rules>
            <Meter label="vCPUs booted" used={alloc.cpus} total={host.cpu_cores ?? null} detail={`${alloc.cpus} of ${host.cpu_cores ?? "?"} cores`} />
            <Meter
              label="Memory booted"
              used={alloc.memory}
              total={host.memory_total_bytes ?? null}
              detail={host.memory_total_bytes != null ? `${formatBytes(alloc.memory)} of ${formatBytes(host.memory_total_bytes)}` : formatBytes(alloc.memory)}
            />
            <Meter label="Colony disk" used={coloniesDisk} total={host.disk_total_bytes ?? null} detail={`${formatBytes(coloniesDisk)} across ${sessions.filter((s) => s.host_disk_bytes).length} colonies`} />
          </Rules>
          {liveHere.length > 0 && (
            <div className="mt-4 border-y border-border">
              <div className="grid grid-cols-[minmax(0,1fr)_130px_56px_64px_72px] gap-4 border-b border-border py-2.5 text-[12.5px] text-muted">
                <span>Colony</span>
                <span>Status</span>
                <span className="text-right">vCPU</span>
                <span className="text-right">Memory</span>
                <span className="text-right">Disk</span>
              </div>
              {liveHere.map((s) => {
                const st = SESSION_STATUS[s.status];
                return (
                  <button
                    key={s.id}
                    type="button"
                    onClick={() => onOpenColony(s.id)}
                    className="-mt-px grid w-full cursor-pointer grid-cols-[minmax(0,1fr)_130px_56px_64px_72px] items-center gap-4 border-0 border-t border-solid border-border bg-transparent py-3 text-left text-[13.5px] tabular-nums transition-colors hover:bg-panel-2"
                  >
                    <span className="min-w-0 truncate">
                      {s.issue_title || s.repo} <span className="font-mono text-[12px] text-faint">{s.repo.split("/").pop()}#{s.issue ?? ""}</span>
                    </span>
                    <span className="text-[13px]" style={{ color: TONE_VAR[st.tone] }}>
                      {st.label}
                    </span>
                    <span className="text-right text-muted">{s.boot_cpus ?? "—"}</span>
                    <span className="text-right text-muted">{s.boot_memory ?? "—"}</span>
                    <span className="text-right text-muted">{s.host_disk_bytes != null ? formatBytes(s.host_disk_bytes) : "—"}</span>
                  </button>
                );
              })}
            </div>
          )}
        </Section>

        <Section
          title="Storage"
          meta={storage ? (storage.admission_paused ? <span className="text-err">below the free-space floor — new colonies are held</span> : storage.free_bytes != null ? `${formatBytes(storage.free_bytes)} free` : undefined) : "no reading yet"}
          right={
            onOpenSettings && (
              <button type="button" onClick={() => onOpenSettings("module:sandbox")} className="cursor-pointer border-0 bg-transparent p-0 text-[13px] text-muted hover:text-text">
                Storage settings
              </button>
            )
          }
        >
          {storage && (
            <Rules>
              <div className="flex h-2 overflow-hidden rounded-full bg-panel-3" style={{ margin: "16px 0 6px" }} role="img" aria-label="data directory by category">
                {storageRows.map((r) => (storageTotal > 0 && r.bytes > 0 ? <span key={r.label} style={{ width: `${(r.bytes / storageTotal) * 100}%`, background: r.color }} /> : null))}
              </div>
              <div className="grid grid-cols-2 gap-x-6 pb-3 sm:grid-cols-4">
                {storageRows.map((r) => (
                  <div key={r.label} className="flex items-center gap-2 py-2 text-[13px]">
                    <span aria-hidden="true" className="h-2 w-2 rounded-sm" style={{ background: r.color }} />
                    <span className="text-muted">{r.label}</span>
                    <span className="ml-auto tabular-nums text-text">{formatBytes(r.bytes)}</span>
                  </div>
                ))}
              </div>
              <div className="border-t border-border py-3 text-[13px] text-muted">
                {storage.reclaimable.length} finished {storage.reclaimable.length === 1 ? "colony" : "colonies"} reclaimable ({formatBytes(reclaimBytes)}) · {storage.unpushed.length} unpushed kept for you · {storage.orphans.length} orphaned worktrees
                {storage.enabled ? ` · auto-reclaim after ${Math.round(storage.retention_secs / 3600)}h` : " · auto-reclaim off"}
              </div>
            </Rules>
          )}
        </Section>

        <Section title="Runtime">
          <Rules>
            <div className="grid grid-cols-2 gap-x-6 sm:grid-cols-3">
              <Fact label="Sandbox">{sandbox?.msb_version ? `microsandbox ${sandbox.msb_version}` : "—"}</Fact>
              <Fact label="Colony image">{sandbox?.image ?? "—"}</Fact>
              <Fact label="Colony size">{[sandbox?.cpus != null ? `${sandbox.cpus} vCPU` : null, sandbox?.memory, sandbox?.max_parallel != null ? `${sandbox.max_parallel} in parallel` : null].filter(Boolean).join(" · ") || "—"}</Fact>
              <Fact label="KVM">{runtime?.kvm == null ? "not needed on this platform" : ok(runtime.kvm.ok, "ready", runtime.kvm.error ?? "unavailable")}</Fact>
              <Fact label="git">{runtime ? ok(runtime.git.ok, runtime.git.version ?? "ok", runtime.git.error ?? "missing") : "—"}</Fact>
              <Fact label="GitHub CLI">{runtime ? ok(runtime.gh.ok, runtime.gh.version ?? "ok", runtime.gh.error ?? "missing") : "—"}</Fact>
              <Fact label="Mesh">{mesh ? (mesh.enabled ? [mesh.provider, mesh.state, mesh.nodes != null ? `${mesh.nodes} ${mesh.nodes === 1 ? "node" : "nodes"}` : null].filter(Boolean).join(" · ") : "off") : "—"}</Fact>
              <Fact label="Claude Code (host)">{runtime ? (runtime.host_claude_bin ? <span className="font-mono text-[12.5px]">{runtime.host_claude_bin}</span> : <span className="text-faint">{runtime.host_claude_bin_error ?? "not found"}</span>) : "—"}</Fact>
              <Fact label="Host id">
                <span className="font-mono text-[12.5px] text-muted">{host.id}</span>
              </Fact>
            </div>
          </Rules>
        </Section>

        {fleet.length > 1 && (
          <Section title="Fleet" meta={`${fleet.filter((h) => h.health === "online").length} of ${fleet.length} online`}>
            <div className="border-y border-border">
              <div className="grid grid-cols-[minmax(0,1fr)_120px_72px_64px_84px_96px] gap-4 border-b border-border py-2.5 text-[12.5px] text-muted">
                <span>Host</span>
                <span>Platform</span>
                <span className="text-right">Slots</span>
                <span className="text-right">Queue</span>
                <span className="text-right">Disk free</span>
                <span className="text-right">Heartbeat</span>
              </div>
              {fleet.map((h) => (
                <div key={h.id} className="-mt-px grid grid-cols-[minmax(0,1fr)_120px_72px_64px_84px_96px] items-center gap-4 border-t border-border py-3 text-[13.5px] tabular-nums">
                  <span className="flex min-w-0 items-center gap-2">
                    <span aria-hidden="true" className={`h-1.5 w-1.5 shrink-0 rounded-full ${h.health === "online" ? "bg-ok" : "bg-err"}`} />
                    <span className="truncate">{h.name}</span>
                    {h.version && <span className="font-mono text-[11.5px] text-faint">{h.version}</span>}
                  </span>
                  <span className="truncate text-[13px] text-muted">{h.platform}</span>
                  <span className="text-right">
                    {h.slots_in_use}/{h.slots_ceiling}
                  </span>
                  <span className="text-right text-muted">{h.queue_depth}</span>
                  <span className="text-right text-muted">{h.disk_free_bytes != null ? formatBytes(h.disk_free_bytes) : "—"}</span>
                  <span className="text-right text-[13px] text-faint">{h.last_heartbeat ? timeAgo(h.last_heartbeat) : "never"}</span>
                </div>
              ))}
            </div>
          </Section>
        )}
      </div>
    </main>
  );
}
