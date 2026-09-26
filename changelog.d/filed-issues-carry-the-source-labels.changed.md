- **Filed issues carry the Source labels, so they stay in the list.** When Settings → Source
  offers only issues with certain labels, an issue created from Colonize or from a chat's "file an
  issue" now gets all of those include labels (none when the setting is empty), and Colonize's
  confirm step shows them ("labels: ready, colonize"). A label the repository lacks is created
  first; one that cannot be created is skipped, logged and named in the toast, and the issue is
  filed anyway. Both routes now also refuse while external writes are blocked, like every other
  filed issue.
