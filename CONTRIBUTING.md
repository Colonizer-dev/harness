# Contributing

Most pull requests here are opened by colonies working in parallel, so the repository is laid out
so that the usual way to add something does not touch a line anyone else is touching. The checks
in the [pull request template](.github/pull_request_template.md) still apply. This file covers the
conventions that keep parallel pull requests from conflicting. How the mothership is put together,
and where a new module plugs into it, is in [docs/architecture.md](docs/architecture.md).

## Your first PR in 15 minutes

Every issue in [docs/good-first-issues.md](docs/good-first-issues.md) runs on a plain laptop: no
KVM, no microVM, no protoc, no libkrun. From a fresh clone:

1. Install Node 24 (what CI pins) — it runs the web build, the script tests and the changelog
   steps. A Rust issue also needs stable Rust from rustup (`--profile minimal`, plus the rustfmt
   and clippy components); there is no `rust-toolchain.toml`, and `rustfmt.toml` sets the line
   width to 130. `python3` is only needed for colonizer-agentd's integration tests. The first
   cargo build fetches dependencies over the network, even for a single crate.

2. Clone the repository and run the lane your issue is in:

   ```sh
   git clone https://github.com/Colonizer-dev/harness && cd harness   # ~5 s
   cd web && npm ci            # ~7 s
   npm run build               # tsc --noEmit && vite build: the typecheck, ~4 s
   npm test                    # vitest, ~15 s
   ```

   The build's one warning (two chunks over 900 kB after minification, monaco the larger) is
   expected, and web has no lint step. To click through the cockpit while you work: `npm run dev`,
   then open http://127.0.0.1:5173/?mock=1 — the UI against an in-browser mock backend, no Rust
   needed.

   For a Rust issue, start with the smallest crate, the guest agent:
   `cargo test -p colonizer-agentd --locked` — cold build and test in about 15 seconds, 13 tests.
   The "cannot harden the daemon" warning inside a container is expected test output. The
   mothership (`-p colonizer-harness`) is a much bigger build, and `cargo test --workspace` also
   needs Node 24 on PATH. A docs-only change needs nothing but Node:
   `node scripts/check-doc-links.mjs` runs in under a second.

3. Before you push Rust, run what [Checks](#checks) runs: `cargo fmt --all --check` and
   `cargo clippy --workspace --all-targets --locked -- -D warnings`.

4. For a user-visible change, add a fragment: `node scripts/changelog.mjs new <issue> <type>`
   writes `changelog.d/<issue>.<type>.md` containing only a template comment — replace it with a
   bold one-line summary, then what changed, ending with `([#<issue>])`.
   `node scripts/changelog.mjs check` validates it, and fails on a file that is still the
   template. Which type to pick is in
   [the changelog section](#changelog-add-a-fragment-never-edit-changelogmd) above.

5. Open the pull request — the checks in its template apply. Questions go on the issue, or in
   Discussions (Q&A) once it is enabled.

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

## Adding a module to the mothership

Every list you touch is alphabetical, one entry per line, so parallel pull requests add their lines
in different places:

1. `mod <name>;` in `crates/colonizer/src/main.rs`, with the module in `crates/colonizer/src/<name>.rs`.
2. Routes: `pub(crate) fn routes() -> axum::Router<crate::Shared>` in your module, and
   `.merge(crate::<name>::routes())` in `server::api_routes`. Then regenerate the route table with
   `UPDATE_ROUTE_SNAPSHOT=1 cargo test -p colonizer-harness route_table` and commit
   `crates/colonizer/routes.snap`. A new route is owner-only to scoped API tokens until it is
   listed in `api_tokens::classify`.
3. State: one field in the "module state" block of `App` and one line in the same block of
   `App::new` (`app.rs`).
4. Background work: `pub(crate) fn start_tasks(app: &Shared)` in your module, and
   `crate::<name>::start_tasks(app);` in `server::start_tasks`.

A non-test Rust file stays under 2,000 lines; once it grows past that, split it into a directory
module (`<name>/mod.rs` plus siblings) with its tests in `<name>/tests.rs` (issue #825).

[docs/architecture.md](docs/architecture.md#adding-a-module) has the details.

## Tests

Rust tests live in `#[cfg(test)]` modules beside the code. Build a shared struct through its
test-only constructor, never a struct literal, so adding a field means one edit:
`AgentModule::test("claude-code").needs_claude(true)` (crates/colonizer/src/modules.rs).

## Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
node --test scripts/test/*.test.mjs
node scripts/check-doc-links.mjs                    # every relative link and #anchor in the Markdown resolves
node scripts/doc-index.mjs check                    # the docs/protocol/ and docs/loops/ indexes match their directories
node scripts/changelog.mjs check                    # the changelog.d/ fragments are well-formed
sh scripts/ci/check-exec-bits.sh
sh scripts/ci/check-rust-file-size.sh               # non-test Rust files stay under 2,000 lines
sh scripts/ci/check-case-collisions.sh              # no two tracked paths differ only in letter case
(cd web && npm ci && npm run build && npm test)     # when web/ changed
(cd modules/agents/<id> && npm test)                # when that agent module changed; run npm ci first where it has a package-lock.json
node --test modules/agents/opencode/test/*.test.mjs  # opencode has no package.json
```

`check-doc-links.mjs` takes file names to check only those (`node scripts/check-doc-links.mjs
docs/cli.md`). If you rename a heading, search for its old anchor: other pages may link to it.

The protocol is split by area — a page under `docs/protocol/`, indexed from `docs/protocol.md` — and
each built-in loop is a page under `docs/loops/`, indexed from `docs/loops.md`. A new area or loop is
a new file there plus `node scripts/doc-index.mjs write`, which refreshes the index; never edit the
generated list by hand.

A new shebang script needs its executable bit in the index (`git update-index --chmod=+x <file>`);
`check-exec-bits.sh` fails without it.
