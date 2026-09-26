- **Colonies are no longer launched on epics.** An epic is a planning container: a colony on it
  duplicates the colonies on its sub-issues (one spent $1.82 and an hour on #531). A launch on an
  issue with sub-issues, an `epic` label, or a title ending "(epic)" or starting "Epic:" is now a
  **409** naming why and listing up to ten open sub-issues to launch instead; `allow_epic: true`
  starts one anyway. The check sits in `POST /api/sessions`, so the dashboard, the Colonize pane,
  the MCP tool and the CLI all get it. Issue lists mark epics ("Epic · 5 sub-issues") and leave them
  out of bulk hand-offs, noting how many were skipped. ([#531])

[#531]: https://github.com/Colonizer-dev/harness/issues/531
