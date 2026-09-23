## What

<!-- One or two sentences: what changed, and why it had to. -->

## Checked

- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo test --workspace` passes
- [ ] `npm test` passes everywhere I touched (`modules/agents/claude-code`, `services/telemetry`, `web`)
- [ ] If this builds something `docs/gaps.md` lists, ports a design but leaves part of it out, or changes a `docs/vision.md` claim, `docs/gaps.md` is updated

## Verified on a real colony

<!-- What you exercised against an actual colony boot, and what you did not. "Nothing" is an
acceptable answer if it is written down: say what would exercise it. CI runs the suites; only a
real colony tells you the thing works. -->

-

## Independent review

Changes to the boot path, the runner contract, a trust boundary (anything that runs `gh` or git on
the host) or cost accounting are reviewed by someone other than their author before merging.

- [ ] This change touches one of those, and that review has happened — or it touches none.
