import { useMemo, useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import { orgHidden } from "../../orgs";
import type { OrgInfo } from "../../types";
import { Avatar } from "../Avatar";
import { IconChevron } from "../icons";
import { Button, Switch, cx, inputClass } from "../ui";
import { Pane } from "./ui";

// ---------------------------------------------------------------------------
// Show or hide orgs (issue #1213): the mothership sees every org the GitHub login reaches, including
// archives and personal accounts Colonizer should never touch. Hiding one takes it out of the
// workspace switcher, the repository pickers and every "All repositories" choice. Nothing is deleted,
// and the org's running colonies keep running.
// ---------------------------------------------------------------------------

/** The orgs to list: decided ones only (an org still awaiting an answer has its own prompt). */
export function listedOrgs(orgs: readonly OrgInfo[]): OrgInfo[] {
  return orgs.filter((o) => !o.awaiting_decision).sort((a, b) => a.org.localeCompare(b.org));
}

/** The orgs "Hide all without colonies" would hide: shown, and no colony of theirs on the list. */
export function hideableWithoutColonies(orgs: readonly OrgInfo[]): OrgInfo[] {
  return listedOrgs(orgs).filter((o) => !orgHidden(o.settings) && o.colonies.total === 0);
}

export function OrgsPane({ orgs, onOrgSaved, back }: { orgs: OrgInfo[]; onOrgSaved?: (saved: OrgInfo) => void; back?: () => void }) {
  const api = useApi();
  const toast = useToast();
  const [query, setQuery] = useState("");
  const [showHidden, setShowHidden] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const q = query.trim().toLowerCase();
  const all = useMemo(() => listedOrgs(orgs), [orgs]);
  const match = (o: OrgInfo) => !q || o.org.toLowerCase().includes(q) || (o.description ?? "").toLowerCase().includes(q);
  const shown = all.filter((o) => !orgHidden(o.settings) && match(o));
  const hidden = all.filter((o) => orgHidden(o.settings));
  const hiddenMatching = hidden.filter(match);
  const bulk = hideableWithoutColonies(orgs);

  const set = async (info: OrgInfo, hide: boolean): Promise<boolean> => {
    try {
      const saved = await api.saveOrg(info.org, { ...info.settings, hidden: hide });
      onOrgSaved?.({ ...info, settings: saved.settings });
      return true;
    } catch (e) {
      toast(errorMessage(e), "error");
      return false;
    }
  };
  const toggle = async (info: OrgInfo, show: boolean) => {
    setBusy(info.org);
    if (await set(info, !show)) toast(show ? `${info.org} is shown again` : `${info.org} is hidden from Colonizer`);
    setBusy(null);
  };
  const hideAll = async () => {
    setBusy("*");
    let done = 0;
    for (const info of bulk) if (await set(info, true)) done += 1;
    setBusy(null);
    toast(`Hid ${done} org${done === 1 ? "" : "s"} without colonies`);
  };

  const row = (o: OrgInfo) => {
    const isShown = !orgHidden(o.settings);
    return (
      <li key={o.org} className="flex items-center gap-3 py-2">
        <Avatar name={o.org} src={o.avatar_url} size={28} rounded="md" />
        <div className="min-w-0 flex-1">
          <div className="truncate text-body-sm font-medium text-text">{o.org}</div>
          <div className="truncate text-meta-lg text-faint">
            {o.colonies.live} live · {o.colonies.total} {o.colonies.total === 1 ? "colony" : "colonies"}
            {o.description ? ` · ${o.description}` : ""}
          </div>
        </div>
        <label className="flex shrink-0 items-center gap-2 text-small-lg text-muted">
          <Switch checked={isShown} disabled={busy !== null} onChange={(on) => void toggle(o, on)} label={`Show ${o.org} in Colonizer`} />
          <span className="hidden sm:inline">Show in Colonizer</span>
        </label>
      </li>
    );
  };

  return (
    <Pane title="Show or hide orgs" subtitle="Choose which GitHub orgs Colonizer shows and acts on" back={back}>
      <div className="space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <input className={cx(inputClass, "h-8 min-w-48 flex-1 py-0 text-small-lg")} placeholder="Find an org…" aria-label="find an org" value={query} onChange={(e) => setQuery(e.target.value)} />
          <Button size="sm" variant="secondary" disabled={bulk.length === 0 || busy !== null} onClick={() => void hideAll()}>
            Hide all without colonies{bulk.length > 0 ? ` (${bulk.length})` : ""}
          </Button>
        </div>
        {all.length === 0 && <p className="text-small-lg text-faint">No orgs yet.</p>}
        {all.length > 0 && shown.length === 0 && <p className="text-small-lg text-faint">{q ? `No shown org matches “${query.trim()}”.` : "Every org is hidden."}</p>}
        <ul aria-label="shown orgs" className="m-0 list-none divide-y divide-border p-0">
          {shown.map(row)}
        </ul>
        {hidden.length > 0 && (
          <div className="border-t border-border pt-2">
            <button
              type="button"
              onClick={() => setShowHidden((v) => !v)}
              aria-expanded={showHidden || (q !== "" && hiddenMatching.length > 0)}
              className="flex cursor-pointer items-center gap-1.5 border-0 bg-transparent p-0 text-small-lg text-muted hover:text-text"
            >
              <IconChevron size={12} className={cx("shrink-0 transition-transform", (showHidden || (q !== "" && hiddenMatching.length > 0)) && "rotate-90")} />
              Hidden ({hidden.length})
            </button>
            {(showHidden || (q !== "" && hiddenMatching.length > 0)) && (
              <ul aria-label="hidden orgs" className="m-0 mt-1 list-none divide-y divide-border p-0">
                {hiddenMatching.map(row)}
              </ul>
            )}
          </div>
        )}
      </div>
    </Pane>
  );
}
