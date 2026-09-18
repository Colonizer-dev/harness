# The bench

A change to an agent — its prompt, a module setting, which model a settler runs on — is either an
improvement or a regression, and reading one colony's chat won't tell you which. The bench is the fixed set
of tasks that change has to survive, scored the same way every time.

It pairs with `scripts/colony-report.mjs`, which says what happened *inside* the colonies (cost, tools,
questions, silences). The bench says whether the work was actually right.

## The tasks

`scripts/bench/tasks.json`. Each one is an issue on a scratch repository, and says what a good outcome is:

| Task | What it is for |
| :--- | :--- |
| `add-helper` | The ordinary case: a small, fully specified addition. Should need no question. |
| `cart-rounding` | A bug with a known fix. Should need no question. |
| `ambiguous-rounding` | A decision the issue does not make. Should ask exactly one question. |
| `readme-typo` | Scope discipline: change one word, touch nothing else. |

The repository's own files are in `scripts/bench/fixture`. The checks that decide whether a task passed are
in `scripts/bench/checks`, and are **not** in the scratch repository: they are copied into the colony's
branch after the fact, so an agent cannot write code that satisfies a test it can read.

A task passes when all of this holds:

- a pull request was opened;
- the hidden check passes on its branch;
- the repository's own tests still pass;
- no file outside the task's `changed_within` list was touched;
- it asked exactly as many questions as the task expects.

## Running it

```sh
node scripts/bench.mjs seed --repo owner/bench          # once: the repository, its files and the issues
node scripts/bench.mjs run  --repo owner/bench --label before
# change one thing: a prompt, a module setting, a model
node scripts/bench.mjs run  --repo owner/bench --label after
node scripts/bench.mjs compare bench-before.json bench-after.json
node scripts/bench.mjs clean --repo owner/bench         # close the bench's pull requests
```

`run` talks to a mothership on `COLONIZER_URL` (default `http://127.0.0.1:7878`), launches one colony per
task, and answers the questions it asks by itself: it picks the option matching the task's `answer.prefer`,
else the first one, and records what it chose. `--only` runs a subset, `--timeout` bounds a colony in
seconds.

This costs real model tokens and opens real pull requests on the scratch repository. It opens them nowhere
else.

## Reading a comparison

```
| Task | Result | Cost | Worked | Questions | Why it failed |
| ambiguous-rounding | pass → FAIL | 0.31 → 0.28 (-0.03) | 94s → 71s | 1 → 0 | asked 0 questions, expected 1 |
```

One run of one task is one sample. A cost difference of a few cents is noise; a task that flips from pass to
fail, or a question that stops being asked, is not.
