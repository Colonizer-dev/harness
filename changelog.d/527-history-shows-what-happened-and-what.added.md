- **History shows what happened, and what you did.** The mothership now keeps an activity log
  (`<data>/activity.jsonl`, rolled over at 2 MB with one previous file kept): every colony outcome —
  pull request opened, merged or closed, nothing to change, stopped, failed, waiting on an answer —
  recorded once at the moment it happens, and every change a person makes through the API — launching,
  stopping, resuming, deleting, answering, Create PR, loops, red-team runs, workspaces switched on or
  off, providers, modules, secrets and tokens saved or removed — with who (`you` in the cockpit, or the
  API token) and when. Names only: a secret's id is logged, never its value, and request bodies are
  never read. `GET /api/activity` pages it (`before`, `limit`) and filters it (`kind`, `actor`, `org`,
  `repo`, `q`). The History page is rebuilt on it: one timeline with an icon per kind, sticky day
  headings, counts of pull requests, merges, failures, questions and your actions, filters by kind,
  repository and actor plus search, 50 rows a page with older activity on request, runs of quiet
  events folded into one expandable row ("12 colonies finished with nothing to change, 00:15–00:16"),
  and each row linking to its colony, pull request or settings section. See
  [docs/protocol.md](docs/protocol.md) §6.9. ([#527])

[#527]: https://github.com/Colonizer-dev/harness/pull/527
