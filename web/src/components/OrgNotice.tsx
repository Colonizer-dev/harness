// The mothership's note about a newly-appeared org (issue #176), as a notification rather than a
// decision card: "Added to `acme`" with one line of explanation and two inline actions. It borrows
// the toast's building blocks (a tinted icon chip, a semibold title, a muted line) and the bell
// panel's row spacing, so it reads like every other notification in the cockpit. It is not a colony
// decision and is never counted as one.
//
// It rides the org poll, not a colony's WebSocket: answering is a plain PUT /api/orgs/{org}
// (`{enabled}`), and an adopted or declined org never comes back (the backend remembers the
// answer), so the row disappears in every open tab on its next org refresh.
import { useState, type ReactElement } from "react";
import { errorMessage, useApi, useToast } from "../context";
import type { OrgInfo, OrgSettings } from "../types";
import type { Api } from "../api";
import { Avatar } from "./Avatar";
import { IconChevronDown, IconOrg } from "./icons";
import { Button, Spinner, cx } from "./ui";

/** Where "Not now" leaves the org: the hint on the button and in the toast after it. */
export const LATER_HINT = "You can add it later in Settings → Workspaces.";

/** One click answers: Add workspace saves `{enabled: true}`, Not now saves `{enabled: false}`. */
export function answerNewOrg(api: Pick<Api, "saveOrg">, org: string, add: boolean) {
  return api.saveOrg(org, { enabled: add });
}

/** The grouped row's title once more than one org is waiting. */
export function groupTitle(count: number): string {
  return `Added to ${count} organisations`;
}

/** The org avatar, or the building icon in the toast's tinted chip when there is none. */
function OrgChip({ org, avatarUrl }: { org: string; avatarUrl?: string | null }): ReactElement {
  const chip = (
    <span aria-hidden="true" className="grid size-8 shrink-0 place-items-center rounded-lg bg-accent-soft text-accent">
      <IconOrg size={16} />
    </span>
  );
  return avatarUrl ? <Avatar name={org} src={avatarUrl} size={32} fallback={chip} /> : chip;
}

/** One org's row: chip, "Added to <org>", the explanation, and Add workspace / Not now. */
export function OrgNoticeRow({
  org,
  avatarUrl,
  onAnswered,
}: {
  org: string;
  avatarUrl?: string | null;
  /** Called only after the save succeeded, with the settings the mothership stored. */
  onAnswered: (org: string, settings: OrgSettings) => void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState<"add" | "skip" | null>(null);

  const answer = async (add: boolean) => {
    if (saving) return;
    setSaving(add ? "add" : "skip");
    try {
      const saved = await answerNewOrg(api, org, add);
      onAnswered(org, saved.settings);
      toast(add ? { title: `${org} is now a workspace`, kind: "success" } : { title: `${org} left out`, body: LATER_HINT, kind: "info" });
    } catch (e) {
      // The row stays where it is; the operator can answer again.
      toast(errorMessage(e), "error");
      setSaving(null);
    }
  };

  const titleId = `org-notice-${org}`;
  return (
    <div role="group" aria-labelledby={titleId} className="flex flex-wrap items-center gap-x-3 gap-y-2 py-2.5">
      <OrgChip org={org} avatarUrl={avatarUrl} />
      <div className="min-w-0 flex-1">
        <div id={titleId} className="truncate text-body font-semibold leading-snug">
          Added to <span className="font-mono">{org}</span>
        </div>
        <div className="mt-0.5 truncate text-small-lg leading-snug text-muted">Add it as a workspace so colonies can work on its repos.</div>
      </div>
      <div className="ml-auto flex shrink-0 items-center gap-2">
        <Button size="sm" variant="primary" disabled={saving !== null} aria-label={`Add ${org} as a workspace`} onClick={() => void answer(true)}>
          {saving === "add" && <Spinner />}
          Add workspace
        </Button>
        <Button size="sm" variant="secondary" disabled={saving !== null} title={LATER_HINT} aria-label={`Not now: keep ${org} out of the workspaces`} onClick={() => void answer(false)}>
          {saving === "skip" && <Spinner />}
          Not now
        </Button>
      </div>
    </div>
  );
}

/**
 * Every org waiting on an answer: one row for one org, and a grouped row ("Added to 3
 * organisations") that expands into one row each when there are more.
 */
export function OrgNotices({
  orgs,
  onAnswered,
}: {
  orgs: readonly Pick<OrgInfo, "org" | "avatar_url">[];
  onAnswered: (org: string, settings: OrgSettings) => void;
}): ReactElement | null {
  const [open, setOpen] = useState(false);
  if (orgs.length === 0) return null;
  if (orgs.length === 1) {
    const [only] = orgs;
    return (
      <section aria-label={`New organisation: ${only.org}`}>
        <OrgNoticeRow org={only.org} avatarUrl={only.avatar_url} onAnswered={onAnswered} />
      </section>
    );
  }
  const title = groupTitle(orgs.length);
  return (
    <section aria-label={title}>
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="flex w-full cursor-pointer items-center gap-3 border-0 bg-transparent py-2.5 text-left text-text"
      >
        <span aria-hidden="true" className="grid size-8 shrink-0 place-items-center rounded-lg bg-accent-soft text-accent">
          <IconOrg size={16} />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block truncate text-body font-semibold leading-snug">{title}</span>
          <span className="mt-0.5 block truncate font-mono text-meta text-faint">{orgs.map((o) => o.org).join(" · ")}</span>
        </span>
        <span className="shrink-0 text-small-lg text-muted">{open ? "Hide" : "Review"}</span>
        <IconChevronDown size={14} className={cx("shrink-0 text-muted transition-transform", open && "rotate-180")} />
      </button>
      {open && (
        <div className="border-t border-border pl-11">
          {orgs.map((o) => (
            <div key={o.org} className="border-b border-border last:border-b-0">
              <OrgNoticeRow org={o.org} avatarUrl={o.avatar_url} onAnswered={onAnswered} />
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
