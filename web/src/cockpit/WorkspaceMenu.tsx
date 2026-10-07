// The workspace menu (issue #1228): the Spotlight panel behind the sidebar's workspace switcher, in
// the cockpit's rail and in the narrow window's drawer alike. A search box, then Pinned, Recent and
// All workspaces, the ones switched off, and the way to manage them. Pins and recents are kept in
// this browser only (workspaceGroups.ts).
import { useState, type ReactElement, type RefObject } from "react";

import { Avatar } from "../components/Avatar";
import { sameOrg, store, stored } from "../components/ui";
import type { OrgEntry } from "../orgs";
import { needFor } from "./feed";
import { Glyph } from "./NavRail";
import { SpotlightPanel, type PanelRow, type PanelSection } from "./spotlight/Panel";
import { PINNED_ORGS_KEY, RECENT_ORGS_KEY, loadOrgList, pushRecentOrg, togglePinned, workspaceGroups } from "./workspaceGroups";

/** A workspace's second line in the switcher: what runs, what waits, how many in all. */
export function scopeStats(live: number, queued: number, total: number, lead?: string): string {
  const parts = [lead, live > 0 ? `${live} live` : null, queued > 0 ? `${queued} queued` : null, `${total} ${total === 1 ? "colony" : "colonies"}`];
  return parts.filter(Boolean).join(" · ");
}

export function AllMark({ size = 24 }: { size?: number }): ReactElement {
  return (
    <span aria-hidden="true" className="grid shrink-0 place-items-center rounded-full bg-panel-3 text-muted" style={{ width: size, height: size }}>
      <Glyph name="all" size={Math.round(size * 0.55)} />
    </span>
  );
}

function PinIcon({ filled }: { filled: boolean }): ReactElement {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill={filled ? "currentColor" : "none"} stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="m12 3 2.6 5.6 6.1.7-4.5 4.2 1.2 6L12 16.5 6.6 19.5l1.2-6L3.3 9.3l6.1-.7z" />
    </svg>
  );
}

function ActionTile({ name }: { name: string }): ReactElement {
  return (
    <span aria-hidden="true" className="spot-tile grid size-8 shrink-0 place-items-center rounded-[10px] bg-panel-3 text-muted">
      <Glyph name={name} size={15} />
    </span>
  );
}

export function WorkspacePanel({
  anchor,
  orgs,
  hiddenOrgs,
  selectedOrg,
  needByOrg = {},
  onSelect,
  onOpenOrgSettings,
  onManageOrgs,
  onClose,
}: {
  anchor: RefObject<HTMLElement | null>;
  orgs: readonly OrgEntry[];
  hiddenOrgs: readonly OrgEntry[];
  selectedOrg: string | null;
  /** Keyed lowercase (needCountByOrg); empty where the caller has no counts. */
  needByOrg?: Record<string, number>;
  onSelect: (org: string | null) => void;
  onOpenOrgSettings: (org: string) => void;
  onManageOrgs?: () => void;
  onClose: () => void;
}): ReactElement {
  const [query, setQuery] = useState("");
  const [pinned, setPinned] = useState<string[]>(() => loadOrgList(stored(PINNED_ORGS_KEY)));
  const [recent, setRecent] = useState<string[]>(() => loadOrgList(stored(RECENT_ORGS_KEY)));
  const current = orgs.find((o) => sameOrg(o.org, selectedOrg)) ?? null;
  const needTotal = Object.values(needByOrg).reduce((a, b) => a + b, 0);
  const settingsOrg = current?.org ?? orgs[0]?.org ?? hiddenOrgs[0]?.org ?? null;
  const q = query.trim();

  const pick = (org: string | null) => {
    if (org) {
      const next = pushRecentOrg(recent, org);
      setRecent(next);
      store(RECENT_ORGS_KEY, JSON.stringify(next));
    }
    onSelect(org);
    onClose();
  };
  const pin = (org: string) => {
    const next = togglePinned(pinned, org);
    setPinned(next);
    store(PINNED_ORGS_KEY, JSON.stringify(next));
  };
  const settings = (org: string) => {
    onClose();
    onOpenOrgSettings(org);
  };

  const groups = workspaceGroups(orgs, hiddenOrgs, pinned, recent, query);
  const needBadge = (n: number) => (n > 0 ? <span className="rounded-full bg-warn-soft px-2 py-0.5 text-meta-lg font-medium text-warn">{n} need you</span> : undefined);
  const orgRow = (o: OrgEntry): PanelRow => {
    const isPinned = pinned.some((x) => sameOrg(x, o.org));
    return {
      id: `org:${o.org}`,
      title: o.org,
      subtitle: scopeStats(o.live, o.queued, o.total) + (o.pending > 0 ? ` · ${o.pending} to review` : ""),
      leading: <Avatar name={o.org} src={o.avatar ?? undefined} size={32} rounded="full" />,
      trailing: needBadge(needFor(needByOrg, o.org)),
      actions: (
        <button
          type="button"
          aria-label={isPinned ? `unpin ${o.org}` : `pin ${o.org}`}
          aria-pressed={isPinned}
          title={isPinned ? "Unpin" : "Pin to the top"}
          onClick={() => pin(o.org)}
          className="grid size-7 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-3 hover:text-text"
        >
          <PinIcon filled={isPinned} />
        </button>
      ),
      checked: sameOrg(o.org, selectedOrg),
      verb: "switch",
      onPick: () => pick(o.org),
    };
  };
  const sections: PanelSection[] = [];
  if (!q)
    sections.push({
      id: "scope",
      rows: [
        {
          id: "all",
          title: "All workspaces",
          subtitle: scopeStats(orgs.reduce((a, o) => a + o.live, 0), orgs.reduce((a, o) => a + o.queued, 0), orgs.reduce((a, o) => a + o.total, 0), `${orgs.length} workspaces`),
          leading: <AllMark size={32} />,
          trailing: needBadge(needTotal),
          checked: selectedOrg === null,
          verb: "switch",
          onPick: () => pick(null),
        },
      ],
    });
  sections.push(
    { id: "pinned", title: "Pinned", rows: groups.pinned.map(orgRow) },
    { id: "recent", title: "Recent", rows: groups.recent.map(orgRow) },
    { id: "all-orgs", title: q ? "Workspaces" : groups.pinned.length + groups.recent.length > 0 ? "All" : "Workspaces", rows: groups.all.map(orgRow) },
    {
      id: "off",
      title: "Switched off",
      aside: "open settings to turn on",
      rows: groups.off.map((o) => ({
        id: `off:${o.org}`,
        title: o.org,
        subtitle: "Switched off in its settings; its colonies stay listed",
        leading: <Avatar name={o.org} src={o.avatar ?? undefined} size={32} rounded="full" />,
        trailing: <Glyph name="settings" size={14} />,
        ariaLabel: `settings for ${o.org} (switched off)`,
        verb: "open settings",
        onPick: () => settings(o.org),
      })),
    },
  );
  const manage: PanelRow[] = q
    ? []
    : [
        ...(settingsOrg && current ? [{ id: "org-settings", title: `${current.org} settings`, leading: <ActionTile name="settings" />, verb: "open", onPick: () => settings(settingsOrg) } satisfies PanelRow] : []),
        ...(onManageOrgs
          ? [{ id: "manage", title: "Manage orgs…", subtitle: "Show or hide workspaces", leading: <ActionTile name="settings" />, verb: "open", onPick: () => (onClose(), onManageOrgs()) } satisfies PanelRow]
          : settingsOrg && !current
            ? [{ id: "manage", title: "Manage workspaces", leading: <ActionTile name="settings" />, verb: "open", onPick: () => settings(settingsOrg) } satisfies PanelRow]
            : []),
      ];
  sections.push({ id: "manage", title: "Manage", rows: manage });

  return (
    <SpotlightPanel
      label="workspaces"
      placement="anchored"
      anchor={anchor}
      align="start"
      width={400}
      onClose={onClose}
      query={query}
      onQuery={setQuery}
      placeholder="Switch workspace…"
      sections={sections}
      empty={`No workspace matches “${q}”.`}
      footerEnd={<span>{orgs.length} {orgs.length === 1 ? "workspace" : "workspaces"}</span>}
    />
  );
}
