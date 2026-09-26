- **Loops on a longer leash, and maps that stay fresh.** A new `every_days` cadence runs a loop
  every N days (1–365) at a fixed UTC time — every 14 days at 03:00, say, anchored so a run that
  fires late never drifts the schedule. A loop can also now be a map loop (`kind: "map"`): instead
  of a prompt it keeps architecture maps current, drawing its own repository each firing or
  `owner/*` mapping every repository of the org one at a time, ten minutes apart, with the list
  taken fresh each cycle so repositories added later join in. Refreshes run through the same
  admission path as the Map view — parallel limits, budgets and the archify skillset rule them in —
  and a refresh that fails keeps the old map, notes why on the loop and records `map.refresh` in
  History. The Map view asks "Keep this map up to date?" once per repository, defaulting to every
  14 days with presets 7/14/30/60/90 or a custom number ("Not now" is remembered in that browser
  for 30 days); map loops are also creatable from the Loops page. See
  [docs/loops.md](docs/loops.md). ([#564])

[#564]: https://github.com/Colonizer-dev/harness/issues/564
