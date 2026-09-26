# Changelog fragments

Pending changelog entries live here, one file per change, and never in `CHANGELOG.md`. A release
folds them into `CHANGELOG.md` and deletes them. Many pull requests are open at once (most of them
opened by colonies), and when each one edited the top of `CHANGELOG.md` they all conflicted there.
A file of your own cannot conflict with anyone else's.

## Adding an entry

```sh
node scripts/changelog.mjs new 123 fixed          # scaffolds changelog.d/123.fixed.md; fill it in
node scripts/changelog.mjs new 123 added --text '**One-line summary.** What changed and why a user cares. ([#123])'
```

- **Name:** `<issue-or-slug>.<type>.md`, in lowercase letters, digits and dashes. Use the issue or
  pull request number when there is one (`123.fixed.md`, or `123-short-slug.fixed.md` if the change
  has more than one entry), and a short slug otherwise (`cockpit-pager.changed.md`).
- **Type**, which picks the section: `added`, `changed`, `deprecated`, `removed`, `fixed`,
  `security`, or `take-care` (anything that could cost a user work: a colony, a worktree, a setting).
- **Content:** one entry, written like the bullets in `CHANGELOG.md`: a bold one-line summary of
  what a user notices, then what changed, ending with `([#123])`. The leading `- ` is optional.
  `[#123]` needs no link definition, because the release writes it. Define any other
  reference-style link at the foot of the file (`[mem0]: https://mem0.ai`). HTML comments are
  dropped.

`node scripts/changelog.mjs check` validates every fragment. CI runs it on each pull request and
fails one that edits `CHANGELOG.md` outside a release.

## If your branch edited CHANGELOG.md

A branch started before this convention, or one written from habit, may have edited
`CHANGELOG.md`. To fix it:

- **Entries you added under `## Unreleased`:** run `node scripts/changelog.mjs convert`. It moves
  each entry into its own fragment, named after its issue and summary, together with the link
  definitions only it uses, and puts the Unreleased note back. Commit `CHANGELOG.md` and
  `changelog.d/`.
- **A conflict in `CHANGELOG.md` after rebasing onto main:** take main's file
  (`git checkout origin/main -- CHANGELOG.md`), then write your entry as a fragment with
  `node scripts/changelog.mjs new <issue> <type>`.

## Cutting a release

In the release pull request, the one that bumps the crate versions:

```sh
node scripts/changelog.mjs assemble --version v0.2.0             # dated today (UTC); --date to pick one
```

This writes `## [v0.2.0] - <date>` under `## Unreleased`. It gets a section per type, in the order
above, with the entries sorted by file name (issue numbers numerically first). Link definitions are
merged, deduplicated and sorted, and the fragments are deleted. Reorder or edit the result by hand
if the release reads better that way, since the release pull request is the one place
`CHANGELOG.md` may change. The release workflow refuses a tag whose commit still has fragments here.
