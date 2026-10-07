// The one card every loop on the Loops page is drawn with (issue #1199), built-in or your own: icon
// and name, a switch, one line of purpose, when it runs, what it covers, how its last run went and a
// 7-day strip of its runs. Everything else opens in a detail drawer: the 7/30/90-day charts, the last
// run grouped by outcome, the settings and the run list. The loop-specific parts (its settings, its
// report, its buttons) are handed in as children; the shape is not negotiable.
import { useCallback, useEffect, useRef, useState, type ReactElement, type ReactNode } from "react";
import { useApi } from "../context";
import { Badge, Button, Spinner, Switch, cx } from "../components/ui";
import type { LoopHistory, LoopHistoryRun, LoopOutcome } from "../types";
import { AreaChart, RangePicker } from "./DashChart";
import type { RangeDays } from "./dash";
import { OUTCOME, dayLabel, dayInitial, dayShort, groupItems, lineSubject, money, outcomeSeries, plural, stripBars, stripTotals, type DetailGroup, type DetailGroupDef, type DetailItem } from "./loopHistory";
import { relative } from "./loops";

/** The loop's history, refreshed every minute and whenever `refreshKey` changes. `null` while loading or when the server has none. */
export function useLoopHistory(id: string, days: number, refreshKey?: unknown): LoopHistory | null {
  const api = useApi();
  const [history, setHistory] = useState<LoopHistory | null>(null);
  const load = useCallback(() => {
    let live = true;
    api.loopHistory(id, days).then(
      (h) => live && setHistory(h),
      () => live && setHistory(null),
    );
    return () => {
      live = false;
    };
  }, [api, id, days]);
  useEffect(() => {
    const stop = load();
    const t = setInterval(load, 60_000);
    return () => {
      stop();
      clearInterval(t);
    };
  }, [load, refreshKey]);
  return history;
}

/** An outcome as a pill: the same words and colours on a card, a run list and a legend. */
export function OutcomePill({ outcome }: { outcome: LoopOutcome }): ReactElement {
  const o = OUTCOME[outcome];
  return (
    <Badge tone={o.tone} pulse={outcome === "running"}>
      {o.label}
    </Badge>
  );
}

/** The 7-day strip: a bar per day (a bar per run for a loop that runs more than twice a day), coloured by outcome, with a hairline under it that darkens with the day's spend. */
export function LoopStrip({ history, onClick }: { history: LoopHistory | null; onClick?: () => void }): ReactElement {
  if (!history) {
    return <div className="h-14 animate-pulse rounded-lg bg-panel-2" aria-hidden="true" />;
  }
  const { mode, bars } = stripBars(history);
  const empty = history.totals.runs === 0;
  return (
    <div data-strip={mode} className="relative z-[1]">
      <div role="img" aria-label={`Last ${history.days} days: ${stripTotals(history)}`} onClick={onClick} className={cx(onClick && "cursor-pointer")}>
        <div className={cx("flex h-10 items-end", mode === "day" ? "gap-2" : "gap-px")}>
          {empty && <div className="h-px w-full bg-border-strong" />}
          {bars.map((b) => (
            <div key={b.key} title={b.label} className={cx("flex h-full min-w-0 flex-1 items-end", mode === "day" && "justify-center")}>
              {b.segments.length === 0 ? (
                <div className="h-[3px] w-full rounded-full bg-border-strong" />
              ) : (
                <div className={cx("flex w-full flex-col-reverse overflow-hidden", mode === "day" ? "max-w-7 rounded-[4px]" : "rounded-[1px]")} style={{ height: `${Math.max(b.height, 0.12) * 100}%` }}>
                  {b.segments.map((s) => (
                    <div key={s.outcome} style={{ flex: s.n, background: OUTCOME[s.outcome].color, opacity: s.outcome === "skipped" ? 0.45 : 1 }} />
                  ))}
                </div>
              )}
            </div>
          ))}
        </div>
        {!empty && (
          <div className={cx("mt-1.5 flex", mode === "day" ? "gap-2" : "gap-px")} aria-hidden="true">
            {bars.map((b) => (
              <div key={b.key} className={cx("h-[3px] min-w-0 flex-1 rounded-full bg-accent", mode === "day" && "mx-auto max-w-7")} style={{ opacity: b.cost > 0 ? 0.2 + 0.8 * b.cost : 0.08 }} />
            ))}
          </div>
        )}
        {mode === "day" && (
          <div className="mt-1 flex gap-2 text-meta text-faint" aria-hidden="true">
            {history.buckets.map((b, i) => (
              <span key={b.day} className={cx("min-w-0 flex-1 text-center", i === history.buckets.length - 1 && "font-medium text-muted")}>
                {dayInitial(b.day)}
              </span>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

export interface LoopCardProps {
  /** The id the loop's history is kept under. */
  historyId: string;
  icon: ReactNode;
  name: string;
  /** One line: what the loop does for you. */
  purpose: string;
  /** Built-in, or one of the user's own. */
  custom?: boolean;
  enabled: boolean;
  onToggle?: (on: boolean) => void;
  busy?: boolean;
  running?: boolean;
  /** "Every hour · next in 28m", or why it will not run. */
  schedule: string;
  /** What it covers; `ready` is false when nothing is set up yet. */
  scope: { text: string; ready: boolean };
  /** Replaces the history's own last-run line (a custom loop names its colony's state instead). */
  lastNote?: string;
  /** Something that needs a person, shown in the card in warn. */
  attention?: string | null;
  /** Anything that should re-read the history: a finished run. */
  refreshKey?: unknown;
  /** The drawer's buttons (Run now, Dry run, Edit…). */
  actions?: ReactNode;
  /** The drawer's sections after the activity charts: last run, settings. */
  children: ReactNode;
  /** Opens the drawer on mount (a link or a test). */
  defaultOpen?: boolean;
  /** Bump this number to open the drawer from outside (a switch that needs a preview first). */
  openSignal?: number;
  onOpenColony?: (id: string) => void;
}

/** The loop card. */
export function LoopCard(props: LoopCardProps): ReactElement {
  const { historyId, icon, name, purpose, custom, enabled, onToggle, busy, running, schedule, scope, lastNote, attention, refreshKey } = props;
  const [open, setOpen] = useState(props.defaultOpen ?? false);
  useEffect(() => {
    if (props.openSignal) setOpen(true);
  }, [props.openSignal]);
  const week = useLoopHistory(historyId, 7, refreshKey);
  const last = week?.last ?? null;
  const state: { label: string; tone: "ok" | "warn" | "neutral" } = !scope.ready ? { label: "Not set up", tone: "warn" } : enabled ? { label: "On", tone: "ok" } : { label: "Off", tone: "neutral" };
  return (
    <article aria-label={name} data-loop={historyId} className={cx("group relative flex min-w-0 flex-col gap-3.5 rounded-xl border border-border bg-panel p-4 transition-colors hover:border-border-strong", !enabled && "bg-transparent")}>
      <header className="flex items-start gap-3">
        <span aria-hidden="true" className={cx("grid size-9 shrink-0 place-items-center rounded-lg border border-border", enabled && scope.ready ? "bg-accent-soft text-accent" : "bg-panel-2 text-muted")}>
          {icon}
        </span>
        <div className="min-w-0 flex-1">
          <h3 className="m-0 flex flex-wrap items-center gap-x-2 gap-y-1 text-body-lg font-semibold leading-6 text-text">
            <button type="button" onClick={() => setOpen(true)} className="min-w-0 cursor-pointer border-0 bg-transparent p-0 text-left font-[inherit] text-inherit after:absolute after:inset-0 after:content-[''] focus-visible:outline-none focus-visible:after:rounded-xl focus-visible:after:ring-2 focus-visible:after:ring-[var(--accent-ring)]">
              {name}
            </button>
            <Badge tone="neutral">{custom ? "Yours" : "Built-in"}</Badge>
            {!scope.ready && <Badge tone="warn">Not set up</Badge>}
          </h3>
          <p className="m-0 mt-0.5 text-small-lg leading-snug text-muted">{purpose}</p>
        </div>
        <div className="relative z-[2] flex shrink-0 items-center gap-2 pt-0.5">
          {running && <Spinner className="text-accent" />}
          {onToggle ? <Switch checked={enabled} disabled={busy} onChange={onToggle} label={`${name} enabled`} /> : <Badge tone={enabled ? "ok" : "neutral"}>{enabled ? "On" : "Off"}</Badge>}
        </div>
      </header>

      <dl className="m-0 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-small-lg">
        <dt className="text-faint">Runs</dt>
        <dd className="m-0 text-text">{schedule}</dd>
        <dt className="text-faint">Covers</dt>
        <dd className={cx("m-0 min-w-0 [overflow-wrap:anywhere]", scope.ready ? "text-text" : "text-warn")}>{scope.text}</dd>
        <dt className="text-faint">Last run</dt>
        <dd className="m-0 flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-text">
          {last ? (
            <>
              <OutcomePill outcome={last.outcome} />
              <span className="min-w-0 text-muted [overflow-wrap:anywhere]">{lastNote ?? last.summary}</span>
              <span className="text-faint">{relative(last.at)}</span>
            </>
          ) : (
            <span className="text-muted">{lastNote ?? "Not run yet"}</span>
          )}
        </dd>
      </dl>

      {attention && (
        <p className="m-0 rounded-lg bg-warn-soft px-3 py-2 text-small-lg text-warn" role="status">
          {attention}
        </p>
      )}

      <footer className="mt-auto">
        <LoopStrip history={week} onClick={() => setOpen(true)} />
        <div className="mt-2 flex items-center justify-between gap-3 text-small text-faint">
          <span className="min-w-0">{week ? `Last 7 days · ${stripTotals(week)}` : "Last 7 days"}</span>
          <span aria-hidden="true" className="shrink-0 whitespace-nowrap text-muted group-hover:text-text">
            Details ›
          </span>
        </div>
      </footer>

      {open && (
        <LoopDetail {...props} state={state} onClose={() => setOpen(false)}>
          {props.children}
        </LoopDetail>
      )}
    </article>
  );
}

/** A section of the drawer: a quiet heading over its body. */
export function DetailSection({ title, meta, right, children }: { title: string; meta?: ReactNode; right?: ReactNode; children: ReactNode }): ReactElement {
  return (
    <section className="border-t border-border px-5 py-5 first:border-t-0">
      <div className="mb-3 flex flex-wrap items-baseline justify-between gap-x-4 gap-y-2">
        <div className="flex min-w-0 items-baseline gap-2">
          <h4 className="m-0 text-body-lg font-semibold text-text">{title}</h4>
          {meta != null && <span className="text-small-lg text-faint">{meta}</span>}
        </div>
        {right}
      </div>
      {children}
    </section>
  );
}

/** A settings block that stays shut until asked (open from the start when the loop is not set up). */
export function Disclosure({ title, defaultOpen, children }: { title: string; defaultOpen?: boolean; children: ReactNode }): ReactElement {
  return (
    <details open={defaultOpen} className="group/d rounded-xl border border-border">
      <summary className="flex cursor-pointer list-none items-center justify-between gap-3 px-4 py-3 text-body font-medium text-text [&::-webkit-details-marker]:hidden">
        {title}
        <span aria-hidden="true" className="text-muted transition-transform group-open/d:rotate-90">
          ›
        </span>
      </summary>
      <div className="border-t border-border px-4 py-4">{children}</div>
    </details>
  );
}

function LoopDetail({
  historyId,
  icon,
  name,
  purpose,
  custom,
  enabled,
  onToggle,
  busy,
  schedule,
  scope,
  actions,
  state,
  onClose,
  onOpenColony,
  children,
}: LoopCardProps & { state: { label: string; tone: "ok" | "warn" | "neutral" }; onClose: () => void }): ReactElement {
  const ref = useRef<HTMLDialogElement>(null);
  const [range, setRange] = useState<RangeDays>(7);
  const history = useLoopHistory(historyId, range);
  useEffect(() => {
    const d = ref.current;
    if (d && !d.open) d.showModal?.();
  }, []);
  return (
    <dialog
      ref={ref}
      onClose={onClose}
      onClick={(e) => e.target === ref.current && ref.current?.close()}
      aria-labelledby={`loop-detail-${historyId}`}
      className="fixed inset-y-0 left-auto right-0 m-0 flex h-dvh max-h-none w-[min(680px,100vw)] max-w-none flex-col overflow-hidden border-0 border-l border-border bg-bg p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50 max-sm:w-screen"
    >
      <div className="flex shrink-0 items-start gap-3 border-b border-border bg-panel px-5 py-4">
        <span aria-hidden="true" className="grid size-9 shrink-0 place-items-center rounded-lg border border-border bg-accent-soft text-accent">
          {icon}
        </span>
        <div className="min-w-0 flex-1">
          <h2 id={`loop-detail-${historyId}`} className="m-0 flex flex-wrap items-center gap-x-2 gap-y-1 text-title font-semibold leading-6">
            {name}
            <Badge tone="neutral">{custom ? "Yours" : "Built-in"}</Badge>
            {!onToggle && <Badge tone={state.tone}>{state.label}</Badge>}
            {onToggle && !scope.ready && <Badge tone="warn">Not set up</Badge>}
          </h2>
          <p className="m-0 mt-0.5 text-small-lg text-muted">{purpose}</p>
          <p className="m-0 mt-1 text-small-lg text-faint">
            {schedule} · {scope.text}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {onToggle && <Switch checked={enabled} disabled={busy} onChange={onToggle} label={`${name} enabled`} />}
          <button type="button" onClick={() => ref.current?.close()} aria-label="Close" className="grid size-9 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
            ✕
          </button>
        </div>
      </div>
      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto">
        {actions && <div className="flex flex-wrap items-center gap-2 border-b border-border px-5 py-3">{actions}</div>}
        <DetailSection title="Activity" meta={history ? `${dayShort(history.from)} to ${dayShort(history.to)}` : undefined} right={<RangePicker range={range} onRange={setRange} compare={false} onCompare={() => {}} hideCompare />}>
          <ActivityCharts history={history} range={range} />
        </DetailSection>
        {children}
        <DetailSection title="Runs" meta={history ? plural(history.totals.runs, "run") : undefined}>
          <RunList history={history} onOpenColony={onOpenColony} />
        </DetailSection>
      </div>
    </dialog>
  );
}

function Stat({ label, value, tone }: { label: string; value: string; tone?: "err" }): ReactElement {
  return (
    <div className="min-w-0 rounded-lg border border-border bg-panel px-3 py-2">
      <div className="text-small text-faint">{label}</div>
      <div className={cx("text-title font-semibold tabular-nums", tone === "err" ? "text-err" : "text-text")}>{value}</div>
    </div>
  );
}

/** The detail's charts: runs by outcome per day, and model spend per day, over the chosen range. */
export function ActivityCharts({ history, range }: { history: LoopHistory | null; range: RangeDays }): ReactElement {
  if (!history) {
    return (
      <p className="flex items-center gap-2 text-body-sm text-muted">
        <Spinner /> Loading the history…
      </p>
    );
  }
  const labels = history.buckets.map((b) => dayLabel(b.day));
  const xLabels = history.buckets.map((b) => dayShort(b.day));
  const t = history.totals;
  return (
    <div className="space-y-5" data-range={range}>
      <div className="grid grid-cols-3 gap-2.5">
        <Stat label="Runs" value={String(t.runs)} />
        <Stat label="Failed" value={String(t.failed)} tone={t.failed > 0 ? "err" : undefined} />
        <Stat label="Spend" value={money(t.cost_usd)} />
      </div>
      <div className="[--chart-h:150px]">
        <div className="mb-1 text-small-lg font-medium text-muted">Runs per day</div>
        <AreaChart series={outcomeSeries(history)} labels={labels} xLabels={xLabels} format={(v) => String(Math.round(v))} formatY={(v) => (Number.isInteger(v) ? String(v) : "")} readTitle={`Last ${range} days`} emptyNote="no runs in this range" />
      </div>
      <div className="[--chart-h:110px]">
        <div className="mb-1 text-small-lg font-medium text-muted">Spend per day</div>
        <AreaChart
          series={[{ label: "Spend", color: "var(--accent)", values: history.buckets.map((b) => b.cost_usd) }]}
          labels={labels}
          xLabels={xLabels}
          format={money}
          formatY={(v) => (v >= 10 ? `$${Math.round(v)}` : `$${Number(v.toFixed(1))}`)}
          readTitle={`Colonies it dispatched, last ${range} days`}
          emptyNote="no spend in this range"
          seriesReadout={false}
        />
      </div>
    </div>
  );
}

/** The runs in the range, newest first, twelve at a time. */
export function RunList({ history, onOpenColony }: { history: LoopHistory | null; onOpenColony?: (id: string) => void }): ReactElement {
  const [shown, setShown] = useState(12);
  if (!history) return <Spinner />;
  if (history.runs.length === 0) return <p className="m-0 text-body-sm text-faint">No runs in this range.</p>;
  return (
    <div>
      <ul className="m-0 list-none divide-y divide-border overflow-hidden rounded-xl border border-border bg-panel p-0">
        {history.runs.slice(0, shown).map((r: LoopHistoryRun, i) => (
          <li key={`${r.at}-${i}`} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3.5 py-2.5 text-small-lg">
            <OutcomePill outcome={r.outcome} />
            <span className="min-w-0 flex-1 basis-48 text-text [overflow-wrap:anywhere]">{r.summary}</span>
            <span className="text-faint" title={new Date(r.at).toLocaleString()}>
              {relative(r.at)}
            </span>
            {r.cost_usd > 0 && <span className="tabular-nums text-muted">{money(r.cost_usd)}</span>}
            {r.colonies.length > 0 &&
              (onOpenColony ? (
                <button type="button" onClick={() => onOpenColony(r.colonies[0])} className="cursor-pointer border-0 bg-transparent p-0 text-accent hover:underline">
                  {plural(r.colonies.length, "colony", "colonies")}
                </button>
              ) : (
                <span className="text-muted">{plural(r.colonies.length, "colony", "colonies")}</span>
              ))}
          </li>
        ))}
      </ul>
      {history.runs.length > shown && (
        <Button size="sm" variant="ghost" className="mt-2" onClick={() => setShown((n) => n + 20)}>
          Show more
        </Button>
      )}
    </div>
  );
}

/** A run's items grouped by outcome, collapsed, with identical reasons merged into one line. */
export function GroupedDetails({
  items,
  defs,
  unit,
  onOpenColony,
}: {
  items: readonly DetailItem[];
  defs: readonly DetailGroupDef[];
  unit: { one: string; many: string };
  onOpenColony?: (id: string) => void;
}): ReactElement | null {
  const groups = groupItems(items, defs);
  if (groups.length === 0) return null;
  return (
    <div className="space-y-2" data-grouped>
      {groups.map((g) => (
        <Group key={g.key} group={g} unit={unit} onOpenColony={onOpenColony} />
      ))}
    </div>
  );
}

const DOT: Record<DetailGroupDef["tone"], string> = { ok: "bg-ok", warn: "bg-warn", err: "bg-err", neutral: "bg-faint", info: "bg-info", accent: "bg-accent" };

function Group({ group, unit, onOpenColony }: { group: DetailGroup; unit: { one: string; many: string }; onOpenColony?: (id: string) => void }): ReactElement {
  return (
    <details className="group/g overflow-hidden rounded-xl border border-border bg-panel">
      <summary className="flex cursor-pointer list-none items-center gap-2.5 px-3.5 py-2.5 text-body [&::-webkit-details-marker]:hidden">
        <span aria-hidden="true" className={cx("size-2 shrink-0 rounded-full", DOT[group.tone])} />
        <span className="font-medium text-text">{group.label}</span>
        <span className="rounded-full bg-panel-2 px-2 text-small tabular-nums text-muted">{group.count}</span>
        <span aria-hidden="true" className="ml-auto text-muted transition-transform group-open/g:rotate-90">
          ›
        </span>
      </summary>
      <ul className="m-0 list-none divide-y divide-border border-t border-border p-0">
        {group.lines.map((line) => {
          const many = line.items.length > 1;
          return (
            <li key={`${line.repo}:${line.reason.text}`} className="px-3.5 py-2.5 text-small-lg">
              {many ? (
                <>
                  <p className="m-0 text-text">
                    <strong className="font-semibold">{lineSubject(line, unit)}</strong>: {line.reason.text}
                    {line.reason.fix && (
                      <>
                        {" "}
                        <a href={line.reason.fix.href} target="_blank" rel="noreferrer" className="text-accent hover:underline">
                          {line.reason.fix.label}
                        </a>
                      </>
                    )}
                  </p>
                  <details className="mt-1">
                    <summary className="cursor-pointer text-muted hover:text-text">Show {line.repo ? unit.many : "them"}</summary>
                    <ul className="m-0 mt-1 list-none space-y-0.5 p-0 text-muted">
                      {line.items.map((i, n) => (
                        <li key={`${i.repo}${i.ref?.text}${n}`} className="[overflow-wrap:anywhere]">
                          <Ref item={i} onOpenColony={onOpenColony} />
                          {i.title ? ` ${i.title}` : ""}
                        </li>
                      ))}
                    </ul>
                  </details>
                </>
              ) : (
                <p className="m-0 text-text [overflow-wrap:anywhere]">
                  <Ref item={line.items[0]} onOpenColony={onOpenColony} />
                  {line.items[0].title ? ` ${line.items[0].title}` : ""} {line.reason.text && <span className="text-muted">— {line.reason.text}</span>}
                  {line.reason.fix && (
                    <>
                      {" "}
                      <a href={line.reason.fix.href} target="_blank" rel="noreferrer" className="text-accent hover:underline">
                        {line.reason.fix.label}
                      </a>
                    </>
                  )}
                </p>
              )}
            </li>
          );
        })}
      </ul>
    </details>
  );
}

function Ref({ item, onOpenColony }: { item: DetailItem; onOpenColony?: (id: string) => void }): ReactElement {
  const text = item.ref ? (item.ref.bare ? item.ref.text : `${item.repo}${item.ref.text}`) : item.repo;
  const label = item.ref?.url ? (
    <a href={item.ref.url} target="_blank" rel="noreferrer" className="font-mono text-meta-lg text-muted hover:underline">
      {text}
    </a>
  ) : (
    <span className="font-mono text-meta-lg text-muted">{text}</span>
  );
  return (
    <>
      {label}
      {item.colony && onOpenColony && (
        <>
          {" "}
          <button type="button" onClick={() => onOpenColony(item.colony!)} className="cursor-pointer border-0 bg-transparent p-0 text-accent hover:underline">
            Open colony
          </button>
        </>
      )}
    </>
  );
}

/** "Every hour · next in 28m", or "Off". The first letter is always a capital: one voice on every card. */
export function scheduleLine(cadence: string, enabled: boolean, nextRunAt: string | null, ready = true, now = Date.now()): string {
  const first = cadence.charAt(0).toUpperCase() + cadence.slice(1);
  if (!ready) return `${first} once it is set up`;
  if (!enabled) return `Off · ${cadence} when on`;
  return nextRunAt ? `${first} · next ${relative(nextRunAt, now)}` : first;
}
