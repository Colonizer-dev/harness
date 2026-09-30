**A Docs & README loop keeps documentation in step with the code, off until you enable it.** Name a
repository or an org on the Loops page (or `POST /api/docs-loop/enable`) and a daily run — hourly at
the most — reads the mothership's clone of each one without a model: merged changes to an
identifier, flag or route a doc names, in code it describes, that left every doc alone (a docs
map derived from the docs' own links, which a `.colonizer/docs-map.toml` can extend), broken relative links and anchors in README files and
`docs/`, commands and flags the docs show that no longer exist, API routes added or removed since
the last run that `docs/protocol.md` does not reflect, and missing changelog entries (a warning only, as the repository's own check has it) where the
repository keeps `changelog.d/` or `## Unreleased`. A repository with findings gets one colony, told
to change only documentation, keep the house style, claim nothing the code does not do, keep the
diff small and run the repository's docs checks; none is dispatched while a docs colony or docs
branch is open, within the cooldown (24 hours by default), or with external writes blocked. Every run's report is
kept in the loop's history and the activity log, the last one shown on the Loops page, and **Dry
run** reports without writing anything. See [docs/loops.md](docs/loops.md#docs--readme).
