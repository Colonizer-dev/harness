import { useEffect, useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import type { UpdateStatus } from "../../types";
import { Badge, Button, Spinner, Switch } from "../ui";
import { ReleaseNotes } from "../ReleaseNotes";
import { IconExternal } from "../icons";
import { Code, Pane, Row } from "./ui";
import { restartOnNewVersion } from "../../cockpit/UpdateBanner";

// ---------------------------------------------------------------------------
// Updates: which Colonizer this is, and whether a newer release is out (#45)
// ---------------------------------------------------------------------------

/// How far behind the running build is: the gap between when it was built and
/// when the newer release came out. Null when the release carries no date.
function daysBehind(builtAt: string, publishedAt: string | null): number | null {
  if (!publishedAt) return null;
  const gap = Date.parse(publishedAt) - Date.parse(builtAt);
  if (!Number.isFinite(gap) || gap <= 0) return null;
  return Math.floor(gap / 86_400_000);
}

/// One sentence about the gap, with the release date in it once.
function behindLabel(builtAt: string, publishedAt: string | null): string | null {
  const days = daysBehind(builtAt, publishedAt);
  if (days === null || !publishedAt) return null;
  const on = new Date(publishedAt).toLocaleDateString();
  if (days < 1) return `Released ${on}, the same day as the build you are running.`;
  return `Released ${on}, ${days} day${days === 1 ? "" : "s"} after the build you are running.`;
}

export function UpdatesPane({
  update,
  onChanged,
  back,
}: {
  update: UpdateStatus | null;
  onChanged: (update: UpdateStatus) => void;
  back?: () => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState(false);
  const [applying, setApplying] = useState(false);
  const [restartBusy, setRestartBusy] = useState(false);

  const restart = async (which: { ids: string[] } | { all: true }) => {
    setRestartBusy(true);
    await restartOnNewVersion(api, which, (message, tone) => toast(message, tone ?? "info"), onChanged);
    setRestartBusy(false);
  };

  // While restarts onto the new version run, follow them until they are done (issue #1097).
  const restartsRunning = (update?.restarts?.restarting.length ?? 0) > 0;
  useEffect(() => {
    if (!restartsRunning) return;
    const timer = setInterval(() => {
      api
        .update()
        .then(onChanged)
        .catch(() => {});
    }, 2000);
    return () => clearInterval(timer);
  }, [api, onChanged, restartsRunning]);

  // While an update is being applied the process is about to be replaced, so the
  // pane follows it until the answer stops coming.
  useEffect(() => {
    const phase = update?.apply.phase;
    if (phase !== "draining" && phase !== "installing" && phase !== "restarting") return;
    const timer = setInterval(() => {
      api
        .update()
        .then(onChanged)
        .catch(() => {});
    }, 1500);
    return () => clearInterval(timer);
  }, [api, onChanged, update?.apply.phase]);

  const install = async () => {
    setApplying(true);
    try {
      await api.applyUpdate();
      onChanged(await api.update());
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setApplying(false);
    }
  };

  const set = async (enabled: boolean) => {
    setSaving(true);
    try {
      onChanged(await api.setUpdateCheck(enabled));
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  const info = (
    <p>
      While it is on, the Mothership asks GitHub every few hours whether a newer release of
      Colonizer-dev/harness is out. The request says nothing about this install; the live map is separate
      and off until you switch it on.
    </p>
  );

  return (
    <Pane title="Updates" subtitle="Which Colonizer this is, and whether a newer one is out" info={info} back={back}>
      {!update ? (
        <p className="flex items-center gap-2 text-body-sm text-muted">
          <Spinner /> Loading…
        </p>
      ) : (
        <div className="space-y-4">
          <div className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg">
            <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
              <span className="font-semibold text-body-sm">{update.installed.version}</span>
              {update.installed.dirty && <Badge tone="warn">built from a modified tree</Badge>}
            </div>
            <p className="mt-1 text-muted">
              {update.installed.commit ? (
                <>
                  commit <Code>{update.installed.commit.slice(0, 7)}</Code>,{" "}
                </>
              ) : null}
              built {new Date(update.installed.built_at).toLocaleString()}
              {update.installed.release && update.installed.release !== update.installed.version
                ? ` (after ${update.installed.release})`
                : ""}
            </p>
          </div>

          {update.available && update.latest && (
            <div className="rounded-xl border border-ok/40 bg-ok-soft px-3.5 py-2.5 text-small-lg">
              <p className="font-semibold text-body-sm">Colonizer {update.latest.version} is available</p>
              {behindLabel(update.installed.built_at, update.latest.published_at) && (
                <p className="text-muted">{behindLabel(update.installed.built_at, update.latest.published_at)}</p>
              )}
              {update.latest.notes && (
                <ReleaseNotes
                  source={update.latest.notes}
                  className="scroll-thin mt-1.5 max-h-48 space-y-1 overflow-auto text-small-lg text-muted"
                />
              )}
              <a className="mt-1.5 inline-flex items-center gap-1 text-accent hover:underline" href={update.latest.url} target="_blank" rel="noreferrer">
                Release notes <IconExternal size={12} />
              </a>
              <div className="mt-2.5 flex flex-wrap items-center gap-2">
                <Button
                  variant="primary"
                  disabled={
                    applying ||
                    !update.can_apply.ok ||
                    update.apply.phase === "draining" ||
                    update.apply.phase === "installing" ||
                    update.apply.phase === "restarting"
                  }
                  onClick={() => void install()}
                >
                  {update.apply.phase === "draining" ||
                  update.apply.phase === "installing" ||
                  update.apply.phase === "restarting" ? (
                    <Spinner />
                  ) : null}
                  {update.apply.phase === "draining"
                    ? "Draining…"
                    : update.apply.phase === "installing"
                      ? "Installing…"
                      : update.apply.phase === "restarting"
                        ? "Restarting…"
                        : `Update to ${update.latest.version}`}
                </Button>
                {!update.can_apply.ok && <span className="text-muted">{update.can_apply.reason}</span>}
              </div>
              {update.apply.phase === "restarting" && (
                <p className="mt-1.5 text-muted">
                  Installed. The Mothership is restarting into it; colonies keep their microVMs and reconnect.
                </p>
              )}
              {update.apply.colonies.length > 0 && update.apply.phase !== "idle" && (
                <ul className="mt-1.5 space-y-0.5 text-muted">
                  {update.apply.colonies.map((c) => (
                    <li key={c.id}>
                      {c.repo} — {c.outcome}
                    </li>
                  ))}
                </ul>
              )}
              {update.apply.phase === "failed" && update.apply.error && (
                <p className="mt-1.5 text-err">
                  Update failed, and the running version is untouched: {update.apply.error}
                </p>
              )}
            </div>
          )}

          {update.switch_to_releases && (
            <div className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg">
              <p className="font-semibold text-body-sm">
                {update.switch_to_releases.reason.includes("development build")
                  ? "This is a development build"
                  : "Update is not available from here"}
              </p>
              <p className="mt-1 text-muted">{update.switch_to_releases.reason}.</p>
              <p className="mt-1.5">
                To switch to releases, run <Code>{update.switch_to_releases.command}</Code>
              </p>
              <p className="mt-1 text-muted">{update.switch_to_releases.then}</p>
            </div>
          )}

          {(update.behind?.length ?? 0) > 0 && (
            <div className="rounded-xl border border-warn/50 bg-warn-soft px-3.5 py-2.5 text-small-lg">
              <div className="flex flex-wrap items-center gap-2">
                <p className="font-semibold text-body-sm">
                  {update.behind!.length === 1 ? "1 colony is" : `${update.behind!.length} colonies are`} still on the previous version
                </p>
                <Button
                  size="sm"
                  disabled={restartBusy || update.behind!.every((c) => update.restarts?.restarting.includes(c.id))}
                  onClick={() => void restart({ all: true })}
                >
                  Restart all on the new version
                </Button>
              </div>
              <p className="mt-1 text-muted">
                Their microVMs keep the components (msb, plugins, agent modules) of the version they booted on until they are
                stopped and resumed. A restart keeps the worktree and the conversation.
              </p>
              <ul className="mt-1.5 space-y-1">
                {update.behind!.map((c) => {
                  const busy = update.restarts?.restarting.includes(c.id) ?? false;
                  const failed = update.restarts?.failed[c.id];
                  return (
                    <li key={c.id} className="flex flex-wrap items-center gap-x-2 gap-y-0.5">
                      <span>
                        {c.repo} <Code>{c.id}</Code> — {c.status}
                      </span>
                      {c.affected_by.length > 0 && <Badge tone="warn">affected: {c.affected_by.join("; ")}</Badge>}
                      <Button size="sm" disabled={restartBusy || busy} onClick={() => void restart({ ids: [c.id] })}>
                        {busy ? <Spinner /> : null}
                        {busy ? "Restarting…" : "Restart on the new version"}
                      </Button>
                      {failed && <span className="text-err">Restart failed: {failed}</span>}
                    </li>
                  );
                })}
              </ul>
            </div>
          )}

          <Row id="update-check-switch" label="Check for new releases" help="Looks for a newer Colonizer in the background and tells you here." inline>
            <Switch
              id="update-check-switch"
              labelledBy="update-check-switch-label"
              label="Check for new releases"
              checked={update.enabled}
              disabled={saving || update.blocked_by !== null}
              onChange={(checked) => void set(checked)}
            />
          </Row>

          {update.blocked_by && (
            <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-muted">
              Kept off by <Code>{update.blocked_by}</Code> in the Mothership’s environment.
            </p>
          )}
          {!update.enabled && !update.blocked_by && (
            <p className="text-small-lg text-muted">Off: the Mothership makes no request to GitHub about releases.</p>
          )}
          {update.error && <p className="text-small-lg text-err">Last check failed: {update.error}</p>}
          {update.enabled && update.last_checked && !update.error && (
            <p className="text-small-lg text-faint">Last checked {new Date(update.last_checked).toLocaleString()}.</p>
          )}
        </div>
      )}
    </Pane>
  );
}
