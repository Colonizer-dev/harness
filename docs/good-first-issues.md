# Good first issues

Small, self-contained work to start on. A candidate touches one or two files, needs no microVM, no
KVM and no running colony, and checks out with the commands in
[CONTRIBUTING.md](../CONTRIBUTING.md#your-first-pr-in-15-minutes).

## The list

Empty for now. Every issue on the last snapshot (2026-09-28: #601, #603, #604, #611, #613, #620,
#621 and #622) has since closed, and none is labelled `good first issue` today. To see whether
that has changed:

```sh
gh issue list --repo Colonizer-dev/harness --label "good first issue" --state open
```

Before you start on one, check that it is unclaimed (no `colonizer:claimed` label and no claim
comment on it).

## For maintainers

- Label a small, self-contained issue `good first issue` when you triage it — that label is the
  list, so the query above finds it without editing this page. `gh issue edit` takes several
  numbers at once:

  ```sh
  gh issue edit <n> <n> … --add-label "good first issue"
  ```

- Enable Discussions (Settings → General → Features → Discussions), keeping the default Q&A and
  Show and tell categories, and pin a welcome post. One that works:

  > New here? Pick an issue from
  > [docs/good-first-issues.md](https://github.com/Colonizer-dev/harness/blob/main/docs/good-first-issues.md) —
  > each one is small, self-contained, and needs no microVM. The fast path from a fresh clone to a
  > green pull request is CONTRIBUTING → "Your first PR in 15 minutes". Comment on the issue to
  > claim it before you start, and ask anything here in Q&A or on the issue itself.

- When one of these issues closes, drop the label so the query above stops returning it.
