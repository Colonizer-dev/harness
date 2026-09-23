# Cockpit Dashboards: design reference

The design for #398, exported from the Claude Design project "Cockpit dashboards and org views"
(`Cockpit Dashboards.dc.html`). It is a reference to implement from, not code the app loads.

| File | What it is |
| :--- | :--- |
| `cockpit-dashboards.dc.html` | The design source: markup, styles and the component logic (the `<script type="text/x-dc">` block holds the sample data and the derived numbers). Claude Design's own editor runtime is stripped, so the file does not render on its own. |
| `overview-dark.png`, `overview-light.png` | The overview across all workspaces, 1440 px wide |
| `org-dark.png`, `org-light.png` | One org's dashboard, 1440 px wide |

The design's tweak props are `theme` (`dark` / `light`), `defaultRange` (`7` / `30` / `90` days),
`startView` (`overview` / `org`) and `showSystem`.

The numbers in the renders are sample data. The cockpit must show real data from the mothership and
name anything the design shows that has no data source yet, rather than faking it (see #398).
