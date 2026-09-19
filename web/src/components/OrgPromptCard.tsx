// The mothership's ask for a newly-appeared org (issue #176): "You were added to `acme`. Add it as
// a workspace?" It borrows the ask_user choice card's shape and visual language — option rows with
// a label and a short description, one clear action — but rides the org poll, not a colony's
// WebSocket: answering is a plain PUT /api/orgs/{org}, and a declined or adopted org never comes
// back here (the backend remembers the answer).
import { useState } from "react";
import { errorMessage, useApi, useToast } from "../context";
import type { OrgSettings } from "../types";
import { Avatar } from "./Avatar";
import { Indicator } from "./AskUserCard";
import { IconCheck, IconOrg } from "./icons";
import { Button, Spinner, cx } from "./ui";

interface ChoiceOption {
  label: string;
  description: string;
}

const OPTIONS: ChoiceOption[] = [
  {
    label: "Add as a workspace",
    description: "It joins the workspace list, and colonies can start on its repositories.",
  },
  {
    label: "Not this one",
    description: "Keeps it out of the list. You can turn it on later in its workspace settings.",
  },
];

const ADOPT = OPTIONS[0].label;

export function OrgPromptCard({
  org,
  avatarUrl,
  onAnswered,
}: {
  org: string;
  avatarUrl?: string | null;
  /** Called only after the save succeeded, with the settings the mothership stored. */
  onAnswered: (org: string, settings: OrgSettings) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [choice, setChoice] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const answer = async () => {
    if (!choice || saving) return;
    setSaving(true);
    try {
      const saved = await api.saveOrg(org, { enabled: choice === ADOPT });
      onAnswered(org, saved.settings);
    } catch (e) {
      // The card stays exactly where it is; the operator can answer again.
      toast(errorMessage(e), "error");
      setSaving(false);
    }
  };

  const headingId = `org-prompt-${org}`;
  return (
    <div
      role="region"
      aria-label={`New workspace ${org}`}
      className="overflow-hidden rounded-2xl border border-border bg-panel shadow-[var(--shadow)]"
    >
      <div className="flex items-center gap-2 border-b border-border bg-accent-soft/60 px-4 py-2.5">
        <span className="grid size-6 place-items-center rounded-full bg-accent text-on-accent">
          <IconOrg size={14} />
        </span>
        <span className="text-[13px] font-semibold">New workspace</span>
        <span className="ml-auto text-[12px] text-muted">1 decision</span>
      </div>

      <fieldset className="min-w-0 px-4 py-4" aria-labelledby={headingId}>
        <div className="mb-3 flex items-center gap-2.5">
          <Avatar name={org} src={avatarUrl} size={28} />
          <h3 id={headingId} className="min-w-0 text-[15px] font-semibold leading-snug">
            You were added to <span className="font-mono [overflow-wrap:anywhere]">{org}</span>. Add it as a workspace?
          </h3>
        </div>
        <div className="grid gap-2" role="radiogroup" aria-labelledby={headingId}>
          {OPTIONS.map((option) => (
            <label
              key={option.label}
              className={cx(
                "flex min-h-14 cursor-pointer gap-3 relative rounded-xl border p-3 transition-colors has-[input:focus-visible]:ring-2 has-[input:focus-visible]:ring-[var(--accent-ring)]",
                choice === option.label ? "border-accent bg-accent-soft/50" : "border-border hover:border-border-strong hover:bg-panel-2",
              )}
            >
              <input
                type="radio"
                name={headingId}
                className="sr-only"
                checked={choice === option.label}
                onChange={() => setChoice(option.label)}
              />
              <Indicator multi={false} checked={choice === option.label} />
              <span className="min-w-0 flex-1">
                <span className="block font-medium leading-snug">{option.label}</span>
                <span className="mt-0.5 block text-[13px] leading-snug text-muted">{option.description}</span>
              </span>
            </label>
          ))}
        </div>
      </fieldset>

      <div className="flex flex-wrap items-center gap-3 border-t border-border bg-panel-2/60 px-4 py-3">
        <p className="min-w-0 flex-1 text-[12.5px] text-muted">
          {saving ? "Saving your answer…" : choice ? "Ready to save." : "Choose one to continue."}
        </p>
        <Button variant="primary" disabled={!choice || saving} onClick={() => void answer()}>
          {saving ? <Spinner /> : <IconCheck size={15} />}
          Confirm
        </Button>
      </div>
    </div>
  );
}
