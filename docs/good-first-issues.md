# Good first issues

Small, self-contained work to start on: each of these touches one or two files, needs no microVM,
no KVM and no running colony, and checks out with the commands in
[CONTRIBUTING.md](../CONTRIBUTING.md#your-first-pr-in-15-minutes). The list is a snapshot taken on
2026-09-28, so before you start check that the issue is still open and unclaimed (no
`colonizer:claimed` label and no claim comment on it). Maintainers apply the
`good first issue` label to the ones listed here.

## Small

- [#620](https://github.com/Colonizer-dev/harness/issues/620) `colonizer update --force`: session_counts calls /api/sessions without the bearer token (always 401) — Rust CLI

  **Start here:** `session_counts` in `crates/colonizer/src/update.rs` sends no token; carry one the way `Machine::from_cli` (`crates/colonizer/src/cli.rs`) does for the other client commands, and add a test against a fake API that requires a token.

- [#604](https://github.com/Colonizer-dev/harness/issues/604) Local CLI commands accept --host and --token-file and silently ignore them — Rust CLI

  **Start here:** the global args on `Cli` in `crates/colonizer/src/cli.rs` are accepted on every subcommand, but `open()` still reads `Settings::from_env()`; honour or refuse the flags on the local commands, and make `--help` say which.

- [#613](https://github.com/Colonizer-dev/harness/issues/613) Header and Overview: today's spend, not only the running total — web

  **Start here:** the total renders in `web/src/cockpit/OverviewView.tsx` from the per-org rollups; sum today's rows from the spend journal instead (`web/src/spend.ts`, covered by `spend.test.ts`).

- [#603](https://github.com/Colonizer-dev/harness/issues/603) ACP module: the Model setting is parsed but ignored — agent module (Node)

  **Start here:** `modules/agents/acp/runner.mjs` never reads `COLONIZER_MODEL`; send `session/set_model` after `session/new` when the agent advertised model selection (the cockpit `set_model` handler in the same file already does this). Test with the fake agent in `modules/agents/acp/test/` (`npm test`).

- [#622](https://github.com/Colonizer-dev/harness/issues/622) Small correctness bugs from the docs audit: live-map count, empty ts, thinking_per_mtok validation, stacked diff, push-subscription race — Rust

  **Start here:** pick one sub-bug per pull request. Two examples: the pricing validation in `crates/colonizer/src/providers.rs` runs `valid_price` over four rates but not `thinking_per_mtok`, and `base_ref` in `crates/colonizer/src/maps.rs` only ever tries `origin/<base>`.

- [#611](https://github.com/Colonizer-dev/harness/issues/611) Inspector: per-file +/- counts on the pull request card — web

  **Start here:** the pull-request card in `web/src/cockpit/Inspector.tsx` still carries the comment that the API reports no diff stats; `GET /api/sessions/{id}/diff` (`crates/colonizer/src/maps.rs`) now returns them. `Inspector.test.tsx` covers the view.

## A step up

Still no microVM, but more to read:

- [#621](https://github.com/Colonizer-dev/harness/issues/621) Claude token saved later is silently ignored once any account exists; legacy keychain/.enc tokens aren't migrated — Rust

  **Start here:** the precedence lives in `claude_cred_for` in `crates/colonizer/src/app.rs`, with the token route in `crates/colonizer/src/claude_accounts.rs`; unit-testable with temp dirs.

- [#601](https://github.com/Colonizer-dev/harness/issues/601) Agent modules' declared egress is validated but never used by the allowlist fence — Rust

  **Start here:** `resolve` in `crates/colonizer/src/egress.rs` unions only the global and org egress_allow; the module manifest's `Egress` struct is already parsed in `crates/colonizer/src/modules.rs`.

## For maintainers

- Label the ones on the list when triaging. `gh issue edit` takes several numbers at once:

  ```sh
  gh issue edit 601 603 604 611 613 620 621 622 --add-label "good first issue"
  ```

- Enable Discussions (Settings → General → Features → Discussions), keeping the default Q&A and
  Show and tell categories, and pin a welcome post. One that works:

  > New here? Pick an issue from
  > [docs/good-first-issues.md](https://github.com/Colonizer-dev/harness/blob/main/docs/good-first-issues.md) —
  > each one is small, self-contained, and needs no microVM. The fast path from a fresh clone to a
  > green pull request is CONTRIBUTING → "Your first PR in 15 minutes". Comment on the issue to
  > claim it before you start, and ask anything here in Q&A or on the issue itself.

- When an issue on the list closes, remove it from this page.
