# Working in this repository

Colonizer's own repository: the Rust mothership (`crates/colonizer`), the guest agent
(`crates/colonizer-agentd`), agent modules (`modules/agents/*`), the cockpit (`web/`) and
scripts (`scripts/`). [CONTRIBUTING.md](CONTRIBUTING.md) has the checks to run and the
conventions. The two that matter most, because other colonies are opening pull requests at the
same time as you:

- **Do not edit `CHANGELOG.md`.** For a user-visible change, add a fragment instead:
  `node scripts/changelog.mjs new <issue-number> <added|changed|fixed|security|take-care|…>`, then
  write the entry in the new `changelog.d/<issue>.<type>.md` in the style of `CHANGELOG.md`'s
  bullets (a bold one-line summary, then what changed, ending with `([#<issue>])`). CI fails a pull
  request that edits `CHANGELOG.md`; only a release does that. If you already wrote an entry into
  `CHANGELOG.md`, `node scripts/changelog.mjs convert` moves it into a fragment. See
  [changelog.d/README.md](changelog.d/README.md).
- **Adding a feature or an API route?** A feature keeps a descriptor in its own file — a
  `features::Feature` (`crates/colonizer/src/features.rs`) naming its `routes`, its `token_scope`,
  its `activity` rules, its `kinds` and its `start_tasks` — and one sorted line in `features::ALL`;
  regenerate the route snapshots with
  `UPDATE_ROUTE_SNAPSHOT=1 cargo test -p colonizer-harness route_table` and commit the module's
  `crates/colonizer/routes/<module>.snap`. A route is owner-only to scoped API tokens unless its
  feature's `token_scope` (or, for an un-migrated module, `api_tokens::classify`) lists it. The whole
  recipe is in [docs/architecture.md](docs/architecture.md#adding-a-module).
