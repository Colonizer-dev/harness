# Contributing

Most pull requests here are opened by colonies working in parallel, so the repository is laid out
so that the usual way to add something does not touch a line anyone else is touching. The checks
in the [pull request template](.github/pull_request_template.md) still apply. This file covers the
conventions that keep parallel pull requests from conflicting. How the mothership is put together,
and where a new module plugs into it, is in [docs/architecture.md](docs/architecture.md).

## Changelog: add a fragment, never edit CHANGELOG.md

Every user-visible change adds one file under [`changelog.d/`](changelog.d/README.md):

```sh
node scripts/changelog.mjs new 123 fixed      # changelog.d/123.fixed.md, then write the entry in it
```

The name is `<issue-or-slug>.<type>.md`, and the type (`added`, `changed`, `deprecated`, `removed`,
`fixed`, `security`, `take-care`) picks the section. The file holds one entry in `CHANGELOG.md`'s
style: a bold one-line summary, then what changed, ending with `([#123])`. The release pull request
folds the fragments into `CHANGELOG.md` with `node scripts/changelog.mjs assemble --version vX.Y.Z`.
CI (`scripts` job) validates the fragments and fails any other pull request that edits
`CHANGELOG.md`. The failure says how to fix it: `node scripts/changelog.mjs convert` moves entries
written under Unreleased into fragments. A deliberate correction to an already released entry takes
the `changelog-edit` label.

## Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
node --test scripts/test/*.test.mjs
sh scripts/ci/check-exec-bits.sh
(cd web && npm ci && npm run build && npm test)     # when web/ changed
```

A new shebang script needs its executable bit in the index (`git update-index --chmod=+x <file>`);
`check-exec-bits.sh` fails without it.
