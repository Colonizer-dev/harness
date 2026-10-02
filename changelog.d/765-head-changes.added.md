**Commit links follow a pull request's head, and the cockpit shows them.** When the PR watcher or the
merge train sees a colony's head change (its own force-push, GitHub's update-branch), the mothership
fetches the branch into its mirror and re-points the colony's commit links; a `sync_repo` fetch that
moved a colony branch does the same. An unchanged head costs nothing. The links are served at
`GET /api/sessions/{id}/commits` and listed under **Commits** in the colony pane, where an orphaned
link carries a badge explaining that a squash or rewrite made the match ambiguous, so it was kept
rather than guessed. ([#765])
