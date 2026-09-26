# Working in this repository

Colonizer's own repository: the Rust mothership (`crates/colonizer`), the guest agent
(`crates/colonizer-agentd`), agent modules (`modules/agents/*`), the cockpit (`web/`) and
scripts (`scripts/`). [CONTRIBUTING.md](CONTRIBUTING.md) has the checks to run and the
conventions. The one that matters most, because other colonies are opening pull requests at the
same time as you:

- **Do not edit `CHANGELOG.md`.** For a user-visible change, add a fragment instead:
  `node scripts/changelog.mjs new <issue-number> <added|changed|fixed|security|take-care|…>`, then
  write the entry in the new `changelog.d/<issue>.<type>.md` in the style of `CHANGELOG.md`'s
  bullets (a bold one-line summary, then what changed, ending with `([#<issue>])`). CI fails a pull
  request that edits `CHANGELOG.md`; only a release does that. If you already wrote an entry into
  `CHANGELOG.md`, `node scripts/changelog.mjs convert` moves it into a fragment. See
  [changelog.d/README.md](changelog.d/README.md).
