- **Cached views survive a restart and a GitHub outage.** The Packages tab, repository meta,
  lines of code and registry facts are kept on disk (`<data>/cache`, capped, least-recently-used
  first out) and served at once after a restart with "updated 5m ago · refreshing" and a Refresh
  button, instead of "scanning" for minutes. Scans are reused per commit, GitHub and registry
  requests are conditional (a 304 costs nothing), a colony's push or merge refreshes only that
  repository, and GitHub avatars load through the mothership's week-long cache (`/api/img`). ([#519])

[#519]: https://github.com/Colonizer-dev/harness/pull/519
