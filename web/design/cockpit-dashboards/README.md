# Cockpit Dashboards: design reference

The design for #398, exported from Claude Design on 2026-09-23 so a colony can read it without a
claude.ai login. It is a **reference, not code to ship**: nothing here is built, bundled or tested,
and `web/tsconfig.json` only includes `src/`.

- `Cockpit Dashboards.dc.html`: the design. The markup inside `<x-dc>` is the template, with
  `{{ … }}` bindings, `<sc-for>` loops and `<sc-if>` conditionals. The `<script data-dc-script>` at the
  bottom holds the state, the derived values and the sample data it renders. Its `data-props` are the
  design's knobs: `theme` (dark|light), `defaultRange` (7|30|90 days), `startView` (overview|org),
  `showSystem`.
- `support.js`: the generic runtime that renders a `.dc.html` (it needs `window.React`). Read it only
  to understand a binding. Do not port it.

## What changed in the design

- **Overview**, redesigned: a KPI strip, a needs-you queue, a throughput chart, compact org cards,
  and one colony table.
- **Org dashboard**, new: delivery KPIs per org, with charts (throughput, spend, latency) and a repo
  drill-down.
- **Workspace settings**: switch off orgs you don't use; empty ones hide themselves.
- **Light and dark themes**, both taken from the `index.css` tokens.

## Screen map

| Screen in the design | Repo files it replaces or extends |
|---|---|
| Overview | `web/src/cockpit/OverviewView.tsx`, `web/src/cockpit/OrgSpend.tsx`, `web/src/cockpit/BurnDownCard.tsx`, `web/src/cockpit/FleetPanel.tsx`, `web/src/spend.ts` |
| Org dashboard | `web/src/cockpit/OrgSpend.tsx`, `web/src/orgs.ts`, `web/src/spend.ts` |
| Header / rail | `web/src/cockpit/Header.tsx`, `web/src/cockpit/Rail.tsx`, `web/src/index.css`, `web/src/components/ui.tsx` |
| Workspace settings | `web/src/orgs.ts`, `web/src/components/OrgSettingsDialog.tsx` |

## Porting rules

- Use the design's sample data only to see the shapes. Wire every number to what the mothership
  already serves (`/api/sessions`, `/api/status`, `/api/orgs`, spend, red-team runs). List any figure
  that has no source yet in the pull request. Do not invent one.
- Take its colour variables (`--bg`, `--panel`, `--ac`, `--ok`, …) as the existing `index.css` tokens.
  Don't add a second palette.

The org and repo names in the sample data are placeholders (acme-corp, globex-labs, …) and were
swapped in at export. They don't refer to real workspaces.
