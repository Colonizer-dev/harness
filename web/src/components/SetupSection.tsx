// The Setup checklist pane (issue #129). It renders the rows `setupView` derived and never
// re-derives a state here: each row is one line when it is done, and when it is not, the
// verbatim error, a one-sentence fix, the command to run and — where re-reading
// GET /api/status can change the answer — a Check again button, which fetches with ?fresh=1.
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { DISMISSIBLE_ROW_IDS, errorDetail, stackPresetOf, setupTone, type SetupRow, type SetupRowId, type SetupView } from "../setup";
import type { HarnessStatus, ModuleInfo, TelemetryStatus } from "../types";
import { type ImagePull } from "../useImagePull";
import { GithubTokenForm } from "./Connections";
import { openModelSwitcher } from "../cockpit/ModelSwitcher";
import { IconCheck, IconChevron } from "./icons";
import { OsLogo } from "./OsLogo";
import { Badge, Button, Spinner, Switch, cx, seconds } from "./ui";

/** The stacks a sandbox preset can name, the automatic default first; "custom" and anything unknown has no chip. */
const STACKS = ["auto", "node", "python", "rust", "go"] as const;

const stackLabel = (id: string) => (id === "auto" ? "Automatic" : id.charAt(0).toUpperCase() + id.slice(1));

/** Setup as the Settings dialog renders it. Everything but the rendering happened in setup.ts. */
export function SetupSection({
  status,
  setup,
  pull,
  telemetry,
  sandbox,
  onStatusChanged,
  onSandboxSaved,
  onTelemetryChanged,
  onDismissedChanged,
  onLaunch,
  onDismiss,
  onShown,
  onOpenLiveMap,
  onOpenCockpit,
  onOpenModels,
  back,
}: {
  status: HarnessStatus | null;
  setup: SetupView | null;
  pull: ImagePull;
  telemetry: TelemetryStatus | null;
  sandbox: ModuleInfo | null;
  onStatusChanged: (fresh?: boolean) => Promise<void> | void;
  onSandboxSaved: (saved: ModuleInfo) => void;
  onTelemetryChanged: (telemetry: TelemetryStatus) => void;
  onDismissedChanged: (ids: string[]) => void;
  onLaunch: () => void;
  onDismiss: () => void;
  onShown: () => void;
  onOpenLiveMap: () => void;
  /** Opens Settings → Your cockpit once the checklist is done, so the address can be bookmarked. */
  onOpenCockpit: () => void;
  /** Opens Settings → Models → Model providers, where every AI account and key lives. */
  onOpenModels: () => void;
  back?: () => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [editing, setEditing] = useState<SetupRowId | null>(null);
  const [checking, setChecking] = useState(false);
  const [picking, setPicking] = useState(false);
  const [savingMap, setSavingMap] = useState(false);
  const firstRef = useRef<HTMLLIElement | null>(null);

  // Re-render once a second while the image pulls, so the elapsed counter moves.
  const [, tick] = useState(0);
  useEffect(() => {
    if (pull.status?.state !== "pulling") return;
    const timer = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(timer);
  }, [pull.status?.state]);

  // Open on the row that needs somebody. Mount only: once the pane is open, scrolling is the reader's.
  useEffect(() => {
    onShown();
    firstRef.current?.scrollIntoView({ block: "start" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (!status || !setup) {
    return (
      <PaneShell setup={null} back={back} onDismiss={onDismiss}>
        <p className="flex items-center gap-2 text-body-sm text-muted">
          <Spinner /> Loading…
        </p>
      </PaneShell>
    );
  }

  const checkAgain = async () => {
    setChecking(true);
    try {
      // ?fresh=1: the mothership keeps a short-lived status cache, and a re-check that
      // answered from it would keep reporting the failure it is being asked about.
      await onStatusChanged(true);
    } finally {
      setChecking(false);
    }
  };

  const currentPreset = stackPresetOf(sandbox?.settings);
  const automatic = (currentPreset ?? "auto") === "auto";
  const pulling = pull.status?.state === "pulling";

  const pickStack = async (preset: string) => {
    if (!sandbox) return;
    setPicking(true);
    try {
      // The mothership merges a preset's defaults into missing keys only, so an explicit
      // `image` saved earlier would keep winning. Picking a stack drops it; cpus, memory
      // and disks are the reader's own and stay.
      const settings: Record<string, unknown> = { ...sandbox.settings, preset };
      delete settings.image;
      onSandboxSaved(await api.saveModule("sandbox", { provider: sandbox.provider, enabled: sandbox.enabled, settings }));
      void pull.start();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setPicking(false);
    }
  };

  const setMap = async (enabled: boolean) => {
    setSavingMap(true);
    try {
      onTelemetryChanged(await api.setTelemetry(enabled));
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSavingMap(false);
    }
  };

  // "Don't ask again" for an advisory row: kept on the mothership, so it holds across browsers and restarts.
  const dismiss = async (id: SetupRowId, dismissed: boolean) => {
    try {
      onDismissedChanged((await api.setSetupDismissed(id, dismissed)).dismissed);
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const dismissButton = (row: SetupRow) =>
    DISMISSIBLE_ROW_IDS.includes(row.id) ? (
      <Button size="sm" variant="ghost" onClick={() => void dismiss(row.id, true)}>
        Don't ask again
      </Button>
    ) : null;

  const retry = (row: SetupRow) =>
    row.retry ? (
      <Button size="sm" onClick={() => void checkAgain()} disabled={checking}>
        {checking && <Spinner />} Check again
      </Button>
    ) : null;

  const errorBlock = (row: SetupRow, source?: string | null) => {
    if (!row.error) return null;
    const { head, rest } = errorDetail(row.error, source);
    return (
      <p className="text-small-lg text-err [overflow-wrap:anywhere]">
        {head}
        {rest && (
          <details className="mt-1">
            <summary className="cursor-pointer text-muted hover:text-text">More</summary>
            <pre className="scroll-thin mt-1 overflow-x-auto font-mono text-meta-lg whitespace-pre-wrap text-muted">{rest}</pre>
          </details>
        )}
      </p>
    );
  };

  const notes = (row: SetupRow) =>
    row.notes.map((note, i) => (
      <p key={i} className="text-small-lg text-muted">
        {note}
      </p>
    ));

  const stackChips = (
    // Deliberately switchable while a pull runs: picking another stack saves it and starts the
    // pull for its image, and the mothership supersedes the in-flight one (its generation guard).
    <div className="flex flex-wrap gap-1.5" role="group" aria-label="Stack">
      {STACKS.map((id) => (
        <button
          key={id}
          type="button"
          disabled={picking}
          aria-pressed={(currentPreset ?? "auto") === id}
          onClick={() => void pickStack(id)}
          className={cx(
            "cursor-pointer rounded-full border px-2.5 py-1 text-small-lg font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-50",
            (currentPreset ?? "auto") === id
              ? "border-accent bg-accent-soft text-accent"
              : "border-border bg-panel text-muted hover:bg-panel-2 hover:text-text",
          )}
        >
          {stackLabel(id)}
        </button>
      ))}
    </div>
  );

  const stackBody = (row: SetupRow) => {
    const pullStatus = pull.status;
    if (row.state === "working" && pullStatus?.state === "pulling") {
      return (
        <div className="space-y-1">
          <p className="flex flex-wrap items-center gap-2 text-body-sm text-muted">
            <Spinner />
            <span>
              Downloading <code className="rounded bg-panel-3 px-1 font-mono text-small text-text">{pullStatus.image}</code> ·{" "}
              {seconds(pullStatus.started_at)}s
            </span>
          </p>
          {/* Deliberately no percentage: msb reports no progress when it is not on a terminal.
              The number below is a measurement, not a promise. */}
          <p className="text-small-lg text-muted">A cold pull measured about 108 s on one connection. This happens once per image.</p>
        </div>
      );
    }
    // Done and not being changed, the row is its one line.
    if (row.state === "done" && editing !== "stack") return null;
    return (
      <div className="space-y-2">
        {pullStatus?.state === "failed" && errorBlock(row)}
        {stackChips}
        {(pullStatus === null || pullStatus.state === "idle" || pullStatus.state === "failed") && (
          <Button size="sm" onClick={() => void pull.start()} disabled={picking || pulling}>
            {picking || pulling ? <Spinner /> : null}{" "}
            {automatic ? "Download image" : `Download ${stackLabel(currentPreset ?? "auto")} image`}
          </Button>
        )}
        {notes(row)}
        {row.state === "todo" && dismissButton(row)}
      </div>
    );
  };

  const githubBody = (row: SetupRow) => {
    if (row.state === "done" && editing !== "github") return null;
    if (row.state === "done") return <GithubTokenForm onStatusChanged={onStatusChanged} />;
    return (
      <div className="space-y-2">
        {errorBlock(row, status.github.error)}
        {row.fix && (
          <p className="text-body-sm">
            Run <code className="rounded bg-panel-3 px-1.5 py-0.5 font-mono text-small">{row.command}</code> on this machine, or paste a
            token here.
          </p>
        )}
        {retry(row)}
        <GithubTokenForm onStatusChanged={onStatusChanged} />
      </div>
    );
  };

  // The required model row (issue #1211): a Claude login, a provider key and a local server all count,
  // so the fix lives on the Models page; this row only says what is missing and links there.
  const modelBody = (row: SetupRow) => {
    if (row.state === "done" && editing !== "model")
      return notes(row).length > 0 ? <div className="space-y-1">{notes(row)}</div> : null;
    return (
      <div className="space-y-2">
        {row.fix && row.state !== "done" && <p className="text-body-sm">{row.fix}</p>}
        {retry(row)}
        <div className="flex flex-wrap gap-2">
          <Button size="sm" variant={row.state === "done" ? undefined : "primary"} onClick={onOpenModels}>
            Open Models
          </Button>
          <Button size="sm" onClick={openModelSwitcher}>
            Switch the orchestrator model
          </Button>
        </div>
        {notes(row)}
      </div>
    );
  };

  const machineBody = (row: SetupRow) =>
    row.state === "done"
      ? null
      : (
          <div className="space-y-2">
            {errorBlock(row)}
            {row.fix && <p className="text-body-sm">{row.fix}</p>}
            {row.command && <code className="rounded bg-panel-3 px-1.5 py-0.5 font-mono text-small">{row.command}</code>}
            {retry(row)}
            {notes(row)}
          </div>
        );

  const launchBody = (row: SetupRow) => (
    <div className="space-y-2">
      <Button variant="primary" disabled={!setup.launchEnabled} onClick={onLaunch}>
        {row.state === "todo" ? "Launch your first colony" : "Launch a colony"}
      </Button>
      {notes(row)}
    </div>
  );

  const mapBody = (row: SetupRow) => (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        {telemetry === null ? (
          <Spinner />
        ) : (
          <Switch label="Share anonymous usage with the live map" checked={telemetry.enabled === true} disabled={savingMap || telemetry.blocked_by !== null} onChange={(checked) => void setMap(checked)} />
        )}
        <Button size="sm" variant="ghost" onClick={onOpenLiveMap}>
          What is sent
        </Button>
      </div>
      {row.fix && row.state === "todo" && <p className="text-small-lg text-muted">{row.fix}</p>}
      {row.state === "todo" && dismissButton(row)}
      {notes(row)}
    </div>
  );

  const body = (row: SetupRow) => {
    switch (row.id) {
      case "machine":
        return machineBody(row);
      case "stack":
        return stackBody(row);
      case "github":
        return githubBody(row);
      case "model":
        return modelBody(row);
      case "launch":
        return launchBody(row);
      case "map":
        return mapBody(row);
    }
  };

  return (
    <PaneShell
      setup={setup}
      back={back}
      onDismiss={onDismiss}
      subtitle="What the first colony needs, in order"
    >
      <ol className="space-y-2.5">
        {setup.rows.map((row) => {
          // Done rows collapse to their one line; the launch CTA and the live-map switch stay out.
          const open = row.state !== "done" || editing === row.id || row.id === "launch" || row.id === "map";
          return (
            <li
              key={row.id}
              ref={row.id === setup.firstActionable?.id ? firstRef : undefined}
              className={cx(
                "rounded-xl border px-4 py-3",
                row.state === "todo" && row.id === "launch" ? "border-accent bg-accent-soft" : "border-border",
              )}
            >
              <div className="flex items-start gap-3">
                <StateMark state={row.state} />
                <div className="min-w-0 flex-1 space-y-2">
                  <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                    {row.os != null && <OsLogo os={row.os} size={14} />}
                    <span className="text-body-lg font-semibold">{row.title}</span>
                    {row.state === "done" && <span className="min-w-0 text-small-lg text-muted [overflow-wrap:anywhere]">{row.detail}</span>}
                    {row.state === "working" && <Badge tone="info">{pull.status?.state === "pulling" ? "Downloading" : "Checking"}</Badge>}
                    {row.state === "blocked" && <Badge tone="err">Blocked</Badge>}
                    {DISMISSIBLE_ROW_IDS.includes(row.id) && row.detail.startsWith("Skipped") && (
                      <Button size="sm" variant="ghost" className="ml-auto" onClick={() => void dismiss(row.id, false)}>
                        Ask again
                      </Button>
                    )}
                    {(row.id === "stack" || row.id === "github" || row.id === "model") && row.state === "done" && (
                      <Button size="sm" variant="ghost" className="ml-auto" onClick={() => setEditing(editing === row.id ? null : row.id)}>
                        {editing === row.id ? "Close" : "Change"}
                      </Button>
                    )}
                  </div>
                  {row.state !== "done" && <p className="text-small-lg text-muted [overflow-wrap:anywhere]">{row.detail}</p>}
                  {open && body(row)}
                </div>
              </div>
            </li>
          );
        })}
      </ol>
      {setup.progress.done === setup.progress.total && (
        <div className="mt-4 flex flex-wrap items-center gap-3 rounded-xl border border-border bg-panel-2 px-4 py-3">
          <p className="min-w-0 flex-1 text-small-lg text-muted">All set. Bookmark this cockpit so you can come back to it from any device.</p>
          <Button size="sm" onClick={onOpenCockpit}>
            Your cockpit address
          </Button>
        </div>
      )}
    </PaneShell>
  );
}

/** The pane chrome — title, the "n of 5 done" badge and the footer — shared by the loading
 *  and loaded states, so switching between them moves nothing. Mirrors SettingsDialog's Pane;
 *  kept local so the checklist does not import the dialog that imports it. */
function PaneShell({
  setup,
  back,
  onDismiss,
  subtitle,
  children,
}: {
  setup: SetupView | null;
  back?: () => void;
  onDismiss: () => void;
  subtitle?: string;
  children: ReactNode;
}) {
  const titleId = useId();
  return (
    <section aria-labelledby={titleId} className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="page-pad flex shrink-0 items-start gap-2 border-b border-border px-5 py-3.5">
        {back && (
          <button
            type="button"
            onClick={back}
            aria-label="Back to all settings"
            className="-ml-1.5 grid size-8 shrink-0 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
          >
            <IconChevron size={16} className="rotate-180" />
          </button>
        )}
        <div className="min-w-0 flex-1">
          <h3 id={titleId} className="text-lead font-semibold leading-8">
            Setup
          </h3>
          {subtitle && <p className="-mt-1 text-small-lg text-muted">{subtitle}</p>}
        </div>
        <div className="flex shrink-0 items-center leading-8">
          {setup && <Badge tone={setupTone(setup)}>{setup.progress.label}</Badge>}
        </div>
      </div>
      <div className="page-pad scroll-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">{children}</div>
      <div className="page-pad flex shrink-0 flex-wrap items-center gap-2 border-t border-border px-5 py-3">
        <span className="mr-auto text-small-lg text-muted">Model providers, plugins and skills are in Settings.</span>
        <Button size="sm" onClick={onDismiss}>
          Not now
        </Button>
      </div>
    </section>
  );
}

/** A row's mark: green check when done, spinner while working on its own, a red dot when a
 *  blocking condition is unmet, a hollow one while it waits on the reader. */
function StateMark({ state }: { state: SetupRow["state"] }) {
  if (state === "done")
    return (
      <span aria-hidden="true" className="mt-0.5 grid size-5 shrink-0 place-items-center rounded-full bg-ok-soft text-ok">
        <IconCheck size={12} />
      </span>
    );
  if (state === "working")
    return (
      <span aria-hidden="true" className="mt-0.5 grid size-5 shrink-0 place-items-center text-muted">
        <Spinner />
      </span>
    );
  return (
    <span aria-hidden="true" className="mt-0.5 grid size-5 shrink-0 place-items-center">
      <span className={cx("size-2.5 rounded-full", state === "blocked" ? "bg-err" : "border border-border-strong")} />
    </span>
  );
}
