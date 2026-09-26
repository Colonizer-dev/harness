- **Colonize: one button that turns issues and plain words into colonies.** The sidebar's
  "New colony" and the dashboard's "Send colonies" are now one orange **Colonize** button, with an
  ant in place of the plus and the GitHub mark (its antennae twitch on hover, and hold still under reduced
  motion); both open the same pane, and so does ⌘K / Ctrl+K from anywhere in the cockpit. The pane
  keeps the old hand-off list (repository scope, search, labels, now ten to a page with the shared
  pager) and adds a box above it: type or speak what you want, press Enter, and the summary model
  that already titles chats (`summary_model`) drafts one issue, or a few when the text lists
  independent tasks. Confirm or edit the title and body, pick the repository when the scope has more
  than one, and **Create** files them with the Mothership's `gh` (the same path as a chat's "file an
  issue"); the new issues land at the top of the list, pre-selected, and are dispatched at once
  unless "Dispatch right after creating" is off. Without a summary model the text itself is the one
  draft. New routes: `POST /api/colonize/draft` and `POST /api/repos/{owner}/{repo}/issues`; the
  activity log and History record `colonize.issue` (issue created from Colonize) and
  `colonize.colony` (a colony dispatched from it). What moved: `/loop 1h <task>`, voice and
  "Launch without an issue" (an open colony on the text) are in the pane's box; the full launch form
  is behind the pane's "launch form" link and the phone's More sheet (now "Launch"); the floating
  composer stays, with `#123` links and Ask, but ⌘K now opens Colonize. On the workspace dashboard
  the button sits on the title row, centred on the org name, with the range and Compare controls
  below it.
