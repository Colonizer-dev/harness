# Releases

How a harness release is cut: by hand the way the first ones were, or by the release train, which does
the same steps on a schedule while `main` is green. Installing a release is
[docs/install.md](install.md), and how a running mothership finds and applies one is
[docs/updates.md](updates.md). What each release contained is [CHANGELOG.md](../CHANGELOG.md).

## The steps

A release is a pull request titled `release: vX.Y.Z`, a lightweight tag on its merge commit, and the
[`Release`](../.github/workflows/release.yml) workflow running on that tag.

1. Every user-visible change added a fragment under [`changelog.d/`](../changelog.d/README.md).
2. `node scripts/release-prep.mjs prep` folds them into `CHANGELOG.md` (`changelog.mjs assemble`),
   moves the release crates to the next patch, refreshes `Cargo.lock` (`cargo update -w --offline`)
   and writes a `docs/release-notes/vX.Y.Z.md` stub listing the fragments. With no fragment pending it
   does nothing. It only ever makes the next patch version: a minor or major bump is a person's
   decision and a hand-made release pull request.
3. The release pull request merges. CI is the ordinary CI; the `scripts` job lets this one pull request
   change `CHANGELOG.md` because it adds the section for the bumped crate version.
4. The tag `vX.Y.Z` goes on the merge commit, once `node scripts/release-prep.mjs verify vX.Y.Z` is
   clean: `changelog.mjs check --release` finds the section and no fragment left, and every release
   crate (`colonizer`, `colonizer-agentd`, `colonizer-observability`, `colonizer-redact`) is at that
   version.
5. `Release` builds and publishes it; `release-health` checks it; hosts with `updates.auto_apply` pick
   it up.

### A crate that is already ahead

`colonizer-redact` can be a version ahead of the others, because a feature pull request bumped it to
publish something new. `prep` moves a crate that is still at the latest release to the new version and
leaves one already at the new version alone, and the path dependencies on it follow its version. A
crate anywhere else (two ahead, or behind) stops `prep` with a message: that is a mistake to fix by
hand, not to guess about. CI's "Test the packaged crates" step reads each crate's own version, so a
crate ahead packages fine.

## The release train

[`.github/workflows/release-train.yml`](../.github/workflows/release-train.yml) runs daily at 21:17 UTC,
on every push to `main` that changes `CHANGELOG.md`, and by hand (Actions → Release train → Run
workflow, with a dry-run switch and the number of green runs to require).

**Tag.** It first looks for merged `release: vX.Y.Z` commits since the latest tag that have no tag
yet, runs the `verify` gate on each (on that commit), and pushes the lightweight tag. A release that
fails the gate is not tagged and the run fails with the reason; fix it on `main` and run the workflow
again.

**Propose.** Then, if all of these hold, it opens `release: vX.Y.Z` with auto-merge on:

- `main` has pending fragments;
- the latest three CI runs on `main` that were not cancelled are green, and the newest is for the
  head of `main` (so a red or unfinished `main` waits);
- no `release:` pull request is open;
- the head commit of `main` does not carry `release-train: skip` in its message, which postpones the
  train until a later commit lands. Put it on the commit you want the train to wait behind.

### Why Release is dispatched, and how the pull request gets CI

A push or pull request made with the workflow's `GITHUB_TOKEN` starts no workflow. A tag pushed that
way would never start `Release`, so the train starts it explicitly (`gh workflow run release.yml --ref
vX.Y.Z`). Run that way `github.ref` is `refs/tags/vX.Y.Z`, which is what the `release` and `crates`
jobs test, and `release-health` follows it. The tag is deliberately pushed with `GITHUB_TOKEN` even
when the optional secret exists, so `Release` runs once, never twice.

The release branch has the same problem. With a `RELEASE_TRAIN_TOKEN` repository secret (a
fine-grained token or a GitHub App installation token with contents, pull requests and actions write)
the push starts CI like any other. Without it the train dispatches CI on the release branch; its check
runs report on the branch's head commit, which is what the pull request's required checks look at.
Auto-merge needs "Allow auto-merge" in Settings → General; without it the train warns and the release
pull request is merged by hand.

### Stopping it

- Put `release-train: skip` in the head commit's message to postpone a run.
- Put the `hold` label on the release pull request, or close it, to stop that release. A closed
  release pull request does not come back until the next run, which sees the fragments still pending.
- Disable the workflow in the Actions tab to stop the train altogether. A release by hand is the
  steps above.

## The freeze

Fragments that land after the release assembled its own would fail the release check (`changelog.mjs
check --release` wants none left). While a `release: vX.Y.Z` pull request is open in a repository, the
[merge steward](colonies.md#the-merge-steward-getting-a-colonys-pull-request-merged) holds each colony
pull request that adds a `changelog.d/` fragment and merges it after the release does. It looks only
at the first 100 files of a pull request, asks GitHub for an open release pull request only when a
candidate adds a fragment, and treats a failed lookup as "open". A pull request without a fragment is
not held, and **Merge now** is still the person's call. Pull requests the steward does not own are held
the usual way, with the `hold` label.

A fragment that slips through anyway (a person merged one) is caught by the gate: the tag is not
pushed, the run fails with `1 fragment(s) left in changelog.d/`, and the fix is a follow-up release
pull request from `prep`, which folds it into the next patch.
