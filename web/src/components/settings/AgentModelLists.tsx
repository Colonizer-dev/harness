// Settings → Models, in the agent module's pane: for each agent module, the models it may run. The
// ticked ones are the only ones the agent and model menu offers for that module. Saving PUTs the
// install's list (PUT /api/models/enabled); "Reset to assigned models" clears it back to the models
// the module's roles are assigned to. Org-level lists are in the API but not offered here.
import { useEffect, useId, useState } from "react";

import { errorMessage, useApi } from "../../context";
import type { AgentModuleRoles, ModelAssignments } from "../../features/models/types";
import { Button, Spinner } from "../ui";

export function AgentModelLists() {
  const api = useApi();
  const [assignments, setAssignments] = useState<ModelAssignments | null>(null);
  const [error, setError] = useState<string | null>(null);
  const headingId = useId();

  useEffect(() => {
    let live = true;
    api
      .modelAssignments()
      .then((a) => live && setAssignments(a))
      .catch((e: unknown) => live && setError(errorMessage(e)));
    return () => {
      live = false;
    };
  }, [api]);

  return (
    <section aria-labelledby={headingId} className="mt-5 space-y-3">
      <div>
        <h3 id={headingId} className="text-body font-medium text-text">Models each agent may run</h3>
        <p className="text-small-lg text-muted">
          Only the ticked models appear in the agent and model menu for that agent. Changes apply to new colonies.
        </p>
      </div>
      {error && <p className="text-body-sm text-err">{error}</p>}
      {!assignments && !error && (
        <p className="flex items-center gap-2 text-body-sm text-muted">
          <Spinner /> Loading models…
        </p>
      )}
      {assignments?.modules.map((m) => (
        <ModuleModels key={m.id} module={m} offered={assignments.models.map((x) => ({ id: x.id, label: x.label }))} />
      ))}
    </section>
  );
}

function ModuleModels({ module, offered }: { module: AgentModuleRoles; offered: { id: string; label: string }[] }) {
  const api = useApi();
  const groupId = useId();
  const [saved, setSaved] = useState<string[]>(module.enabled_models);
  const [picked, setPicked] = useState<string[]>(module.enabled_models);
  const [busy, setBusy] = useState<"save" | "reset" | null>(null);
  const [status, setStatus] = useState<{ tone: "ok" | "err"; text: string } | null>(null);

  // Every model on offer, plus any enabled id the list does not describe, so saving never drops one silently.
  const known = new Set(offered.map((o) => o.id));
  const items = [...offered, ...saved.filter((id) => !known.has(id)).map((id) => ({ id, label: id }))];
  const dirty = picked.length !== saved.length || picked.some((id) => !saved.includes(id));
  const blocked = module.blocked !== null;

  const toggle = (id: string, on: boolean) => setPicked((cur) => (on ? [...cur.filter((x) => x !== id), id] : cur.filter((x) => x !== id)));

  const run = async (kind: "save" | "reset") => {
    setBusy(kind);
    setStatus(null);
    try {
      // Save sends the ticked models in the menu's order; reset sends null, which clears the saved list.
      const models = kind === "reset" ? null : items.filter((i) => picked.includes(i.id)).map((i) => i.id);
      const reply = await api.setEnabledModels({ scope: "install", module: module.id, models });
      setSaved(reply.enabled_models);
      setPicked(reply.enabled_models);
      setStatus({
        tone: "ok",
        text:
          kind === "reset"
            ? `Reset ${module.name} to its assigned models.`
            : `Saved: ${reply.enabled_models.length} ${reply.enabled_models.length === 1 ? "model" : "models"} enabled for ${module.name}.`,
      });
    } catch (e) {
      setStatus({ tone: "err", text: errorMessage(e) });
    } finally {
      setBusy(null);
    }
  };

  return (
    <fieldset disabled={blocked} className="rounded-xl border border-border bg-panel px-4 py-3">
      <legend className="px-1 text-body-sm font-medium text-text">
        {module.name}
      </legend>
      {blocked && <p className="mb-2 text-small-lg text-warn">{module.blocked}</p>}
      {items.length === 0 ? (
        <p className="py-1 text-small-lg text-faint">No models on offer.</p>
      ) : (
        <div className="grid gap-1.5 py-1 sm:grid-cols-2">
          {items.map((item) => {
            const inputId = `${groupId}-${item.id}`;
            return (
              <label key={item.id} htmlFor={inputId} className="flex min-w-0 cursor-pointer items-center gap-2 rounded-md px-1 py-1 text-body-sm text-text hover:bg-panel-2">
                <input
                  id={inputId}
                  type="checkbox"
                  checked={picked.includes(item.id)}
                  onChange={(e) => toggle(item.id, e.target.checked)}
                  className="size-4 shrink-0 cursor-pointer accent-accent"
                />
                <span className="min-w-0 truncate">{item.label}</span>
              </label>
            );
          })}
        </div>
      )}
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <Button variant="primary" size="sm" disabled={blocked || !dirty || busy !== null} onClick={() => void run("save")}>
          {busy === "save" && <Spinner />} Save models
        </Button>
        <Button variant="ghost" size="sm" disabled={blocked || busy !== null} onClick={() => void run("reset")}>
          {busy === "reset" && <Spinner />} Reset to assigned models
        </Button>
        {status && (
          <span role="status" className={status.tone === "ok" ? "text-small-lg text-ok" : "text-small-lg text-err"}>
            {status.text}
          </span>
        )}
      </div>
    </fieldset>
  );
}
