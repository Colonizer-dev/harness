# Docs & README

Part of the [Colonizer loops](../loops.md).

A built-in loop that keeps a repository's documentation in step with its code. It is **off** until
you name a repository (`owner/name`) or an org (`owner`) for it: **Loops** → **Docs & README** →
**Enable**, or `POST /api/docs-loop/enable`. It then runs daily; the interval goes down to hourly,
and a loop you have just enabled first runs 10 minutes later.

Each run reads the mothership's bare clone of every allowlisted repository (at most 20 per run, 5
seconds apart) at its default branch. The checks are deterministic, spend no model tokens and never
run the repository's own code on the host:

- **Code changed, docs did not**: merged changes since the loop's last run (or, the first time,
  since one interval ago) that changed something a doc names — an identifier that reads as code
  (`loop_next`, `OrgSettings`), a flag (`--max-runs`) or an API route (`/api/loops/{id}`) in its
  inline code — in a code file that doc describes, without touching it. A change that touched any
  doc (the changelog aside) is left out, and so are comment-only, formatting-only and moved lines.
  The docs map is derived from the docs themselves: a README or `docs/` page that names at most
  eight code files (by a link or in inline code) describes each of them, and `docs/<name>.md`
  describes the code files called `<name>` (when at most three share the name). Entry points
  (`main`, `lib`, `mod`, `index`) and tests are left out.
  A `.colonizer/docs-map.toml` in the repository adds to it:

  ```toml
  derive = true                        # false: use only the [[map]] entries below
  docs = ["guides/**"]                 # more paths that count as docs
  ignore = ["src/generated/**"]        # code never flagged
  checks = ["npm run docs:check"]      # the docs checks the colony runs
  [[map]]
  code = ["src/api/**"]
  docs = ["docs/api.md"]
  [[cli]]
  name = "mytool"
  sources = ["src/cli.rs"]
  ```

- **Broken links and anchors**: relative links in README files and `docs/` whose file is gone, or
  whose `#anchor` matches no heading — GitHub's rules, as `scripts/check-doc-links.mjs` reads them.
- **Commands that no longer exist**: in shell code blocks and inline code, `npm`/`pnpm`/`yarn`/`bun
  run` scripts no `package.json` defines, script paths that are gone, `make` targets and `cargo -p`
  crates that are not declared, and the repository's own CLI's subcommands and flags — for
  Colonizer, `colonizer` against `crates/colonizer/src/cli.rs`; elsewhere, the `[[cli]]` entries.
- **Route drift** (Colonizer's own layout, or a `[routes]` entry with `snapshot` and `doc`): routes
  `routes.snap` gained since the last run that `docs/protocol.md` does not name, and routes it lost
  that the doc still names.
- **Changelog**: only where the repository keeps `changelog.d/` fragments or a `## Unreleased`
  section. With fragments it follows the repository's own `scripts/changelog.mjs check`: a change
  touching its `CODE_PATHS` (or code, without that script) that neither adds a fragment nor edits
  `CHANGELOG.md` — so never a release, which folds and deletes fragments — and only changes since
  the repository adopted `changelog.d/`, dependency bumps aside. Like that check, it only warns:
  the finding is reported, but never dispatches a colony on its own. With `## Unreleased`: an
  empty section, or one missing a merged change.

When a repository has findings, the run dispatches **one** colony for it (origin `docs-loop`) with
the findings as its brief and these rules: change only documentation, never code; keep the
repository's writing style; claim nothing the code does not do, and verify every statement against
the code; keep the diff small; run the repository's docs checks. It does not dispatch when a docs
colony of this loop is still live or its pull request is open, when a branch named for docs or the
README with commits the default branch lacks was pushed in the last 14 days, or within the
repository's cooldown (24 hours after its last dispatch by default). With external writes blocked
(`COLONIZER_NO_EXTERNAL_EFFECTS`) a run only reports.

Every run's report — the findings, and what was dispatched or skipped and why — is kept in the
loop's history (the last 30 runs) and written to the activity log as `loop.docs`; the Loops page
shows the last one. **Dry run** reports the findings and writes nothing: no colony, no history, no
activity. The routes are in [protocol.md](../protocol/loops.md#docs--readme-loop).
