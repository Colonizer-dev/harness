// Saved model profiles in the model switcher: a named set of role → model choices, kept in the
// install config (GET/POST/PUT/DELETE /api/models/profiles) so every device sees the same list.
// "Use" loads a profile into the popover's draft for the chosen scope; the ordinary Apply (new
// colonies only, or also the running ones) then switches to it, so a profile is applied exactly as
// a hand-made switch is. "Save as profile…" stores the current selection; saved profiles can be
// renamed and deleted, the starters (derived from what the install has configured) cannot.
import { useState, type ReactElement } from "react";

import { cx } from "../components/ui";
import type { ModelProfile } from "../types";

/** "orchestrator opus · subagents deepseek-flash", for an option's title. */
export function profileSummary(profile: ModelProfile): string {
  const words: Record<string, string> = {
    model: "orchestrator",
    subagent_model: "subagents",
    background_model: "background",
    summary_model: "summary",
    small_model: "small",
    model_low: "small tasks",
    model_high: "large tasks",
  };
  return Object.entries(profile.roles)
    .map(([role, model]) => `${words[role] ?? role} ${model ? model.slice(model.indexOf("/") + 1) : "default"}`)
    .join(" · ");
}

type Mode = { kind: "idle" } | { kind: "save"; name: string } | { kind: "rename"; name: string } | { kind: "delete" };

export interface ProfileBarProps {
  /** Null while loading. */
  profiles: ModelProfile[] | null;
  /** What happened last: "Loaded Night shift — Apply to switch", or an error. */
  note: string | null;
  noteTone?: "info" | "err";
  busy: boolean;
  /** Nothing to save while the scope has no role. */
  canSave: boolean;
  onUse: (profile: ModelProfile) => void;
  onSave: (name: string) => Promise<boolean>;
  onRename: (profile: ModelProfile, name: string) => Promise<boolean>;
  onDelete: (profile: ModelProfile) => Promise<boolean>;
  /** For the tests, which render without effects. */
  initialSelected?: string;
  initialMode?: Mode;
}

const BUTTON = "cursor-pointer rounded-md border border-border bg-transparent px-2 py-1 text-[12px] text-text hover:border-border-strong disabled:cursor-not-allowed disabled:opacity-50";
const LINK = "cursor-pointer border-0 bg-transparent p-0 text-[11.5px] text-muted hover:text-text hover:underline disabled:cursor-not-allowed disabled:opacity-50";

export function ProfileBar(p: ProfileBarProps): ReactElement {
  const [selected, setSelected] = useState<string>(p.initialSelected ?? "");
  const [mode, setMode] = useState<Mode>(p.initialMode ?? { kind: "idle" });
  const profiles = p.profiles ?? [];
  const saved = profiles.filter((x) => !x.builtin);
  const starters = profiles.filter((x) => x.builtin);
  const current = profiles.find((x) => x.id === selected) ?? null;
  const input = "w-full min-w-0 rounded-md border border-border bg-panel-2 px-2 py-1 text-[12.5px] text-text";

  const submit = async () => {
    if (mode.kind === "save" && (await p.onSave(mode.name))) setMode({ kind: "idle" });
    if (mode.kind === "rename" && current && (await p.onRename(current, mode.name))) setMode({ kind: "idle" });
  };

  return (
    <section aria-label="model profiles" className="mb-3">
      <div className="mb-1 flex items-baseline justify-between gap-2 text-[11.5px] text-muted">
        <span>Profiles</span>
        {mode.kind === "idle" && (
          <button type="button" className={LINK} disabled={p.busy || !p.canSave} onClick={() => setMode({ kind: "save", name: "" })}>
            Save as profile…
          </button>
        )}
      </div>

      {mode.kind === "save" || mode.kind === "rename" ? (
        <form
          className="flex items-center gap-1.5"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <input
            aria-label={mode.kind === "save" ? "new profile name" : "profile name"}
            autoFocus
            maxLength={60}
            placeholder={mode.kind === "save" ? "Name this selection" : "New name"}
            value={mode.name}
            onChange={(e) => setMode({ ...mode, name: e.target.value })}
            className={input}
          />
          <button type="submit" disabled={p.busy || mode.name.trim() === ""} className={cx(BUTTON, "border-0 bg-accent font-semibold text-on-accent hover:brightness-110")}>
            {mode.kind === "save" ? "Save" : "Rename"}
          </button>
          <button type="button" className={BUTTON} onClick={() => setMode({ kind: "idle" })}>
            Cancel
          </button>
        </form>
      ) : (
        <div className="flex items-center gap-1.5">
          <select aria-label="profile" value={selected} disabled={p.busy || p.profiles === null} onChange={(e) => setSelected(e.target.value)} className={input}>
            <option value="">{p.profiles === null ? "Loading profiles…" : profiles.length ? "Pick a profile…" : "No profiles yet"}</option>
            {saved.length > 0 && (
              <optgroup label="Saved">
                {saved.map((x) => (
                  <option key={x.id} value={x.id} title={profileSummary(x)}>
                    {x.name}
                  </option>
                ))}
              </optgroup>
            )}
            {starters.length > 0 && (
              <optgroup label="Starters">
                {starters.map((x) => (
                  <option key={x.id} value={x.id} title={profileSummary(x)}>
                    {x.name}
                  </option>
                ))}
              </optgroup>
            )}
          </select>
          <button type="button" className={BUTTON} disabled={p.busy || !current} onClick={() => current && p.onUse(current)} title="Load this profile's models; Apply switches">
            Use
          </button>
        </div>
      )}

      {current && mode.kind === "idle" && (
        <div className="mt-1 flex flex-wrap items-baseline justify-between gap-x-2 text-[11px] text-faint">
          <span className="min-w-0">{profileSummary(current)}</span>
          {!current.builtin && (
            <span className="flex shrink-0 gap-2">
              <button type="button" className={LINK} disabled={p.busy} onClick={() => setMode({ kind: "rename", name: current.name })}>
                Rename
              </button>
              <button type="button" className={cx(LINK, "hover:text-err")} disabled={p.busy} onClick={() => setMode({ kind: "delete" })}>
                Delete
              </button>
            </span>
          )}
        </div>
      )}

      {mode.kind === "delete" && current && (
        <div role="alertdialog" aria-label="delete the profile" className="mt-1.5 flex items-center justify-between gap-2 rounded-lg border border-border bg-panel-2 px-2 py-1.5 text-[12px] text-text">
          <span className="min-w-0">Delete “{current.name}” on every device?</span>
          <span className="flex shrink-0 gap-1.5">
            <button type="button" className={BUTTON} onClick={() => setMode({ kind: "idle" })}>
              Keep
            </button>
            <button
              type="button"
              disabled={p.busy}
              className={cx(BUTTON, "border-err text-err")}
              onClick={() =>
                void p.onDelete(current).then((ok) => {
                  if (ok) {
                    setSelected("");
                    setMode({ kind: "idle" });
                  }
                })
              }
            >
              Delete
            </button>
          </span>
        </div>
      )}

      {p.note && (
        <p role={p.noteTone === "err" ? "alert" : "status"} className={cx("m-0 mt-1 text-[11.5px]", p.noteTone === "err" ? "text-err" : "text-muted")}>
          {p.note}
        </p>
      )}
    </section>
  );
}
