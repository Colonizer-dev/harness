# The v0.1.3 audit

An external source-code audit read Colonizer v0.1.3 (commit `d89ce76`) on 17 September 2026. Its
verdict, in plain words: not ready for unattended work on sensitive repositories with real
credentials. It comes first here on purpose, and this page is edited as findings close.

What the harness is good for today is attended work. You launch the colony, you watch it, and a
human reads the pull request before anything happens next. The audit's tracking issue is
[#91](https://github.com/Colonizer-dev/harness/issues/91).

## What it credits

These held when the audit checked them against the code.

- **The mount split.** The worktree and `/harness/out` are writable; the bare repository, the agent,
  the plugins, the memory scopes and the binaries are mounted read-only.
- **Secrets on the host.** The GitHub token, the provider keys and the mem0 key stay on the host
  and never enter a colony. Since #468 a newly saved secret goes to the system keychain (Keychain
  on macOS, the Secret Service on Linux) when the keychain answers a startup probe; otherwise, and
  for every secret saved before that, it is a 0600 file under the mothership's config directory. The Claude credential is handed
  to the sandbox as a host-scoped secret and swapped in by the TLS proxy for `api.anthropic.com`;
  the guest environment holds a placeholder.
- **Per-colony gateway tokens.** Each colony gets its own random token, written 0600 on the host and
  compared in constant time at the gateway.
- **The publish step.** The branch must carry the `colonizer/` prefix and must not be the base
  branch. There is no force-push. Nothing merges automatically by default. The exceptions, both
  off by default, are the publish module's `automerge` setting (which needs `autofix` too), which
  merges a fix colony's pull request once an independent review session passes it, and the merge
  train: a background tick that squash-merges an open colony pull request only when mergeability
  is clean, every check and the base branch's own CI are green, it is not a draft and carries no
  HOLD / do-not-merge / WIP mark, its commits pass the author and attribution allowlists, and the
  branch is up to date with the base — never a force-merge, never `--admin`. `merge_train_overrides`
  turns it on or off per org or repo, and `merge_train_deny_orgs` keeps named orgs out whatever
  the overrides say.
- **Untrusted colony output.** The worktree's `.git` is rewritten from the value recorded before the
  VM ran, nested `.git` directories are removed, and `pr.md` must be a regular file within a size
  limit.
- **Memory review.** Proposals arrive pending, and only an approved note is mounted into another
  colony. An operator can turn this off for repo notes only; org and global notes are always reviewed.
- **API token.** Every cockpit API request needs the per-install token (`<config_dir>/api-token`):
  an `Authorization: Bearer` header, or the `colonizer_token` cookie, which keeps the same-origin
  `Origin` requirement on writes and upgrades. The earlier Host-and-Origin checks stopped browsers,
  not scripts — the API had no authentication until this change. `Host` is still checked against
  the bind address (DNS rebinding), and unauthenticated `GET /api/status` answers a reduced body
  (version, counts, capacity and health only) for fleet peers. Since then, scoped API tokens
  (#557) are accepted too, as a Bearer header only; see
  [Trust controls added since the audit](#trust-controls-added-since-the-audit).
- **Pinned inputs.** The vendored artefacts, the agent binary and the Headroom bundles are verified
  against recorded sha256 digests, and the GitHub Actions are pinned by commit.

## Trust controls added since the audit

None of these clears a gate below. Each is a control the audit did not see, and each has its own
limits.

- **Scoped API tokens** (#557). `colonizer token create` mints named tokens with a scope — `read`,
  `operate` or `launch`, in that order — and optional org and repo limits, a cap on unfinished
  colonies and a daily dollar budget. Only a SHA-256 hash is stored, in
  `<config_dir>/api-tokens.json`. A scoped token works as `Authorization: Bearer` only, never as the
  cookie. A route outside its scope answers 403; a colony outside its org or repo limits answers 404
  (`crates/colonizer/src/api_tokens.rs`). See [cli.md](cli.md#scoped-api-tokens).
- **Remote access** (#555, #558, #575). Off by default. When you turn it on (Settings → Remote
  access, or `PUT /api/remote`), the mothership keeps one outbound WebSocket to the relay and
  serves the cockpit through it, under the same API token. `host_guard` admits a tunnelled request
  only while the switch is on and only for the tunnel's own host, and a cookie write through the
  tunnel must carry `Origin: https://<that host>` exactly (`crates/colonizer/src/server.rs`). The
  relay is not deployed yet, and the security review filed findings R1–R3 as blockers for its
  first deployment; see [remote-access-review.md](remote-access-review.md) and
  [remote-tunnel.md](remote-tunnel.md).
- **Security-aware routing** (#530, #626). At boot, the paths a task names are classified `open`,
  `standard`, `custom`, `vetted` or `restricted` (secrets, keys, cloud credentials, infra config),
  from built-in defaults a repository can extend with `.colonizer/sensitivity.toml`
  (`crates/colonizer/src/sensitivity.rs`). A gateway provider must meet the class's minimum mark —
  `vetted` work needs a provider marked `vetted`, `restricted` work one marked `trusted` — or the
  request is refused with a 403. An org can move the bar per class in its workspace settings
  (loosening `restricted` never goes below `vetted`) and pin restricted work to a list of vendors.
  The gate sits in the provider gateway, so it covers configured providers only: an agent's own
  Claude traffic goes straight to `api.anthropic.com` with its host-scoped credential and does not
  pass through it.
- **Gateway audit** (#546). Every authenticated gateway request appends one line to the colony's
  `gateway.jsonl`: provider, wire, method, path, requested and sent model, status, a typed failure
  code, durations, bytes and token counts. The record is a fixed struct, so keys, tokens and
  request bodies never reach it (`crates/colonizer/src/gateway_audit.rs`).
- **Log redaction** (#761). A credential that reaches a colony's logs anyway (an agent echoing
  a token, a tool printing a connection string, a request path with a key in its query string) is
  replaced with `[REDACTED:<kind>]` before the line is written to `events.jsonl`, `harness.jsonl`
  or `gateway.jsonl`, and before the line is broadcast to the cockpit. The detectors are layered:
  provider token prefixes (GitHub, Anthropic, OpenAI, AWS, Stripe, Slack), a corpus of other
  credential shapes (PEM private keys, JWTs, `Bearer` values, webhook URLs, more token prefixes),
  passwords in `scheme://user:pass@host` and in database and broker connection strings, values of
  `KEY=value` pairs and JSON fields whose name says secret, and, last, long high-entropy strings.
  Git SHAs, UUIDs, lockfile hashes and base64 image data are left alone. A JSON field named as an
  identifier or digest skips only the high-entropy layer: the key's last word, split at `_`, `-`,
  `.` and camelCase, must be `id`, `uuid`, `sha`, `hash`, `digest`, `etag`, `signature` or the like
  (`user_id`, `commitSha`, not `did` or `paid`), and a secret word anywhere in the key (`token`,
  `key`, `session`, `cookie`, …) removes the exemption, so `api_key_id` is still checked. JSON lines are redacted
  field by field, so they stay valid JSON. The local archive redacts older logs on the way into a
  bundle. It is pattern matching, so it can miss a secret with no recognisable shape; it is a
  second line behind keeping secrets out of the colony, not a replacement
  (`crates/colonizer-redact/src/lib.rs`). The findings ledger (`findings.jsonl`) is redacted as it is
  written, and a fleet export redacts the logs it carries.
  The same redactor covers the other text the mothership keeps or sends from agent and model
  output: a filed finding (`finding-body.md` and the issue), an independent review (`review.md`
  and the PR comment), the commit subject and pull request taken from `pr.md` (`pr-body.md` and
  the PR itself), chat transcripts (`chats/<id>.jsonl`), colony summaries (`sessions.json`) and
  the activity log (`activity.jsonl`). Redaction does not hide the leak: when it changed
  `pr.md`, `review.md` or a finding, the colony's log gets a warning naming what went (`pr.md
  contained 1 secret (github token), redacted before publishing`), and autopilot does not publish
  such a `pr.md`: it holds the colony (`autopilot_held`) for a person to press Create PR, since the
  colony had the secret in hand and the diff itself is not redacted. A rotated `events-N.jsonl` is renamed, not rewritten, so it
  holds what was written: one from before redaction existed is redacted whenever it is read back
  (the cockpit's diagnosis and a resumed colony's prompt) or bundled. Not covered: the rest of
  `sessions.json` (a colony's `error` and its pending question), architecture maps, and the
  numeric stats files (routing, spend, jev ladder).
  There is one redactor, the `colonizer-redact` crate, and every path uses it. The session store
  redacts every line appended through it (`SessionStore::append`, `store::ledger_line`), so a new
  writer to `events.jsonl`, `harness.jsonl`, `findings.jsonl` or a later ledger cannot forget to;
  writers that also broadcast the line redact it first, and for them the store's pass is a no-op.
  An archive bundle redacts every text file it carries, not only logs: `out/pr.md`, `review.md`
  and a staged vault note go in redacted, and binary files go in as they are. The fleet history
  push (#762) uploads each log redacted, hashed and sized as redacted, since the owner stores
  payloads exactly as sent. The observability exporter (#1028) redacts every string with the
  same crate and has no detector of its own. The operator vault (#777) runs the shared redactor
  after its exact-value scrub, on staged notes and on proposals; the deja-vu transcript copies
  (#592) do the same. Jev's coarse token filter for text sent to Jev is followed by the shared
  redactor. Boundary event details (#609) are redacted as they are built.
- **Credentials stay in the session directory.** An archive bundle or a fleet export never holds
  a session's `vm/token`, `gateway-token`, `vm/mesh-authkey` or `vm/session.json`, nor any file
  there named like a credential (`*token*`, `*authkey*`, `*.key`, `*.pem`, `secrets*`)
  (`is_credential_file` in `crates/colonizer/src/archive.rs`). The fleet history push carries only
  the three log ledgers, never one of these files.
- **Inside the colony.** Credential files in the worktree are masked and agent config is pinned
  read-only ([path-policy.md](path-policy.md), #545); every colony boots behind an egress policy
  with a deny set no setting can reopen ([sandbox-network.md](sandbox-network.md#egress-policy-303),
  #542); and the agent runs with capabilities dropped and a seccomp denylist
  ([architecture.md](architecture.md#in-guest-hardening), #547).
- **Prompt screening** (#540). An opt-in `screen` module checks the diff and the pull request text
  for hidden code points before push ([prompt-screening.md](prompt-screening.md)). It reads code
  points only; it is not a prompt-injection detector.

## Findings

Four findings describe ways a colony could cross into the host. They are filed as draft security
advisories, visible to maintainers only, and are not described here until they are fixed.

| Finding | Severity | Detail |
| :--- | :--- | :--- |
| F01 | High | Withheld until fixed. |
| F02 | High | Withheld until fixed. |
| F03 | High | Withheld until fixed. |
| F04 | Medium | Withheld until fixed. |

The public ones, each with its issue:

| Finding | Issue |
| :--- | :--- |
| **F05.** No fail-closed gate for pushes, PRs and issues | [#84](https://github.com/Colonizer-dev/harness/issues/84) |
| **F06.** A publish that fails after the commit can't be completed | [#85](https://github.com/Colonizer-dev/harness/issues/85) |
| **F07.** The parallel limit isn't atomic, and costs aren't a budget | [#86](https://github.com/Colonizer-dev/harness/issues/86) |
| **F08.** Failed writes of state and events are ignored | [#87](https://github.com/Colonizer-dev/harness/issues/87) |
| **F09.** Telemetry retention isn't guaranteed | [#88](https://github.com/Colonizer-dev/harness/issues/88) |
| **F10.** Release provenance and pinned inputs | [#89](https://github.com/Colonizer-dev/harness/issues/89), with CI and real-colony tests in [#73](https://github.com/Colonizer-dev/harness/issues/73) and [#368](https://github.com/Colonizer-dev/harness/issues/368) |
| The installer's app swap isn't atomic | [#90](https://github.com/Colonizer-dev/harness/issues/90) |

Classify a finding on arrival: [boundaries.md](boundaries.md) splits what the harness enforces
from what it only suggests. "Hit a wall" — a denial the colony could not cross — is a guidance gap,
never a security finding. "Defeated a control" — a boundary crossed — is one, and it is the
watchdog's stop-and-flag signal.

The [escape-vector review checklist](escape-vectors.md) tracks each colony-escape class against the live sandbox with a verdict and mechanism per vector, and re-runs on every sandbox-affecting change.

## Checkpoints

Four gates have to pass before unattended work is on the table.

- **G1, boundaries.** F01–F04 fixed, each with a negative test on a real colony. That covers host
  access, a writer that stays behind, and credential use beyond the colony's routes.
- **G2, external effects.** The no-write policy for publishing and issues, and gates bound to the
  exact tree, work ([#84](https://github.com/Colonizer-dev/harness/issues/84)).
- **G3, recovery.** Failures injected into storage, VM stop, commit, push, PR, concurrency and
  updates; recovery is idempotent and keeps an accurate record
  ([#85](https://github.com/Colonizer-dev/harness/issues/85),
  [#86](https://github.com/Colonizer-dev/harness/issues/86),
  [#87](https://github.com/Colonizer-dev/harness/issues/87),
  [#90](https://github.com/Colonizer-dev/harness/issues/90)).
- **G4, release.** Full CI and native end-to-end tests on a pinned runtime; privacy retention and
  supply-chain evidence meet the requirements
  ([#73](https://github.com/Colonizer-dev/harness/issues/73),
  [#88](https://github.com/Colonizer-dev/harness/issues/88),
  [#89](https://github.com/Colonizer-dev/harness/issues/89)).

## Where this stands

Nothing is checked off yet, and no gate is cleared. The roadmap is the issue tracker, and this page
is the audit's view of it. Since the audit, `.github/workflows/ci.yml` runs on every pull request and
push to `main`: `cargo test --workspace` (including agentd's no-KVM smoke test), `cargo clippy` with
warnings denied and `cargo fmt --check`; the Claude Code runner's tests; the web UI's `tsc`, build and
tests; the tests of the telemetry receiver, the remote-access relay and the scripts; a macOS compile and
lint (`rust-macos`); and the UHP conformance check (`conformance`). `supply-chain.yml` adds dependency audits
and SBOMs. CI gates merges on `main` since
[#367](https://github.com/Colonizer-dev/harness/issues/367) (below). The limit that keeps this short of
what G4 asks for: a real colony is booted in CI since #368, but without a real model or a real GitHub write: the
`colony-e2e` job runs a whole colony — mothership, microVM, agentd and the Claude Code runner against a
scratch repository and a stub model server — on every pull request, so the negative tests on real
colonies that G1 asks for are not what it runs.

## Authority controls, issue #98 (partial G2)

`crates/colonizer/src/authority.rs` adds per-effect grants bound to a candidate hash
(hex SHA-256 over length-prefixed parts, via the existing `ring` dependency): `authorize`
denies expired, ungranted, candidate-mismatched, non-independent, or unbound approvals, all
fail-closed. `publish.rs` splits the single publish gate into ordered `commit_allowed` →
`push_allowed` → `pr_allowed` checks plus a PR-body binding helper, and `lifecycle.rs`
`recover` fences restarted colonies behind fresh re-authorization. Wiring only — no gate is
checked off until real-colony negative tests exercise these paths.

## No-write policy, issue #84 (partial G2)

Wired: an operator kill-switch, `COLONIZER_NO_EXTERNAL_EFFECTS` or `COLONIZER_NO_WRITE` set to
anything but `0`/`false`/`off`/`no` (`authority::external_writes_blocked`). While it is on, the
publish endpoint answers 409, the publish task refuses before claiming the colony, and
`commit_allowed` → `push_allowed` → `pr_allowed` all fail closed; the commit, push and PR steps in
`github.rs` each re-check before running. Draft PRs are refused like ready ones. Stacked-PR retargets,
fix-PR merges and review comments, and filed findings are skipped with a log line. Every publish also
refuses to open or reuse a PR when the local branch head moved after the push step.

Not done: the PR body's SHA-256 is only logged, never checked against an approval — per-effect grants
stay with #98. Running the repository's tests on the host before a push is still the operator's
responsibility. As above, F05 is not checked off and G2 is not cleared.

## Required checks, issue #367 (partial G4)

CI gates merges on the default branch. What is live today is the branch protection on `main`, not
the script's ruleset: it requires the six jobs that always run on a pull request (`rust`, `runner`,
`scripts`, `telemetry`, `web`, `colony-report`) plus the supply-chain `vulnerabilities` job, each
pinned to the GitHub Actions app, so a status another app posted under the same name does not
count, and it applies to administrators too. The script describes the same gate as a repository
ruleset (`CI required on main`), which has not been applied; the only ruleset on the repository
blocks force-pushes and deletion of `main`. `scripts/require-ci-checks.mjs` prints the ruleset (a dry run, the default) and
`--apply` sends it through `gh api`, idempotently; applying it needs a repository admin, which is why
it is a script and not a pull request. The update replaces the ruleset wholesale, so a rule, ref
condition or bypass actor added to it by hand in the UI is lost on the next run — change it in the
script, not in the UI. Not required: `colony-e2e`, on purpose, since a KVM-dependent job can
flake on a runner difference and a retry is cheaper than a blocked pull request; `relay`,
`rust-macos` and `conformance`, which run on every pull request but are not in the script's list
(`REQUIRED_CHECKS` in `scripts/require-ci-checks.mjs`); the `sbom` job, which is evidence, not a
gate; and the release jobs, which paths and tags keep away from an
ordinary pull request. Two settings travel with it and are flipped by hand in the repository's
settings: "Allow auto-merge" on, because a colony pull request held for required checks is queued
with `gh pr merge --squash --auto` and GitHub refuses that queue without it, and no merge queue,
because no workflow here has a `merge_group` trigger and a queued pull request would never leave it.
The script keeps `vulnerabilities` out of its list on purpose, because it can go red on a newly
published advisory with no commit at all (the weekly run is the detection path); the live branch
protection requires it anyway, so such an advisory blocks every merge until it is fixed
([#935](https://github.com/Colonizer-dev/harness/issues/935)).
