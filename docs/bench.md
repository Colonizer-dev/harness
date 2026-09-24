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

Every task also has a reference solution in `scripts/bench/reference/<task-id>/`: only the files its fix
changes or adds, laid out as in the fixture. `scripts/test/bench.test.mjs` holds each check to it: the
check must fail on the bare fixture and pass, along with `npm test`, once the reference is laid over it, and
the reference must stay inside the task's `changed_within`. A check that passes on untouched code, or fails
on a correct fix, would score every colony the same whatever it did.

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
seconds, and `--heldout <dir>` scores the held-out suite below (`--max-gap`, default 0.25, sets its
threshold).

This costs real model tokens and opens real pull requests on the scratch repository. It opens them nowhere
else.

## Reading a comparison

```
| Task | Result | Cost | Worked | Questions | Why it failed |
| ambiguous-rounding | pass → FAIL | 0.31 → 0.28 (-0.03) | 94s → 71s | 1 → 0 | asked 0 questions, expected 1 |
```

One run of one task is one sample. A cost difference of a few cents is noise; a task that flips from pass to
fail, or a question that stops being asked, is not.

Every result also carries `clean`: after scoring, the [trajectory monitor](trajectory-monitor.md) audits the
colony's persisted event log for shortcut shapes — history mining, weakened tests, writes to what the scorer
executes, solution fetches, unflagged injections — and the comparison gains a Clean column (`clean`,
`HACKED`, or `–` when there was no log to audit) plus the run-level clean rate and the gap between resolved
and clean-resolved. A run that raises its pass rate while widening that gap bought its score; the gap is the
number to watch.

## Held-out suite

A change tuned on repeated runs of the visible checks — or a colony that has somehow seen them — can pass
without doing the work. So each task family also has a held-out **companion** check: the same fix, inputs
the visible check never names, kept **outside this repository**, where the agents being scored cannot read
it. The set lives in a directory you pass and nothing in the repo defaults to it; a directory inside this
working tree is refused, through a symlink too.

```sh
node scripts/bench.mjs heldout add --heldout ~/bench-heldout --family cart-rounding --check my-check.test.mjs
node scripts/bench.mjs run --repo owner/bench --label after --heldout ~/bench-heldout
```

`heldout add` copies the check into the set, bumps its version and records the addition in `heldout.json`'s
history (a missing manifest starts at version 1). `run --heldout` resolves one active companion per family
**before any colony launches** — a family without one fails the run before it spends — then scores every
task that opened a pull request against its family's companion on a fresh clone of its own, never the
visible check's checkout. The scorer's git carries the same `-c` guards the mothership puts on every
host-side git (`HOST_GIT_NO_EXEC` in `crates/colonizer/src/github.rs`), so a branch's hooks, fsmonitor,
gc and maintenance never execute here; and only the pass bit, read from the exit code, and the companion's
id are kept — the output is dropped, so held-out material never lands in anything the bench writes.

A companion retires after `RETIRE_AFTER` (3) scoring decisions — the number the synth pool retires on — and
each retirement or addition bumps the version and enters the history. The report is per family's **gap**:
the visible pass rate minus the held-out pass rate, worst first, a task that never opened a pull request
counting as failing both. A family strictly above `--max-gap` (default 0.25) fails the run naming the
family and the numbers, and `run` exits 1. `bench-<label>.json` carries
`heldout: { version, max_gap, families, failures, next_version? }` — `version` is the set the run was
scored against, `next_version` appears when recording the run's decisions left the set on a new version.
`compare` adds a line for it and flags two different scored versions as not comparable across the
rotation.

Scoring makes no model calls, so its only cost is time: each result records
`scoring: { visible_ms, heldout_ms }` and the run summary sums `scoring_ms` (not yet journaled into the
mothership's spend.jsonl, #296).

## Synthetic tasks

Four hand-written tasks is a thin sample. `scripts/bench/synth.mjs` grows the set the SWE-smith way: inject
a bug into real source, keep only the mutants that break the repository's own tests, and admit them through
a gate. Stage one is procedural and Node-only — no model in the loop, so an accepted task costs $0 to make.

### What the generator does

A token-level scanner — deliberately not an AST — walks JavaScript source, skipping comments, strings,
template literals (interpolations included) and regexes, including a `/` after a keyword such as `return`
or `typeof`, where division is impossible. It yields mutation sites: operator swaps (`+`↔`-`, `*`↔`/`,
`<`↔`<=`, `>`↔`>=`, `===`↔`!==`, `&&`↔`||`) and literal nudges (an integer n → n+1, `true`↔`false`).
Where a construct is ambiguous — `**`, `++`, `=>`, a generator's `*`, a number that is not a plain
integer — the site is skipped rather than risk nonsense. The issue text is templated from the failing test
names and states the symptom only: which tests fail, and that the fix belongs in the source, not the tests.
It never names the operator or the line.

### The gate

Each candidate is checked before it is trusted, with the counts landing in `runs.json` so the pass rate is
measured, not asserted:

1. the reference checkout must be green, or generation aborts;
2. `node --check` on the mutated file — a mutant that does not parse is rejected as vacuous breakage;
3. the bugged checkout runs its tests twice: the same tests failing both times, compared name by name,
   admits the instance to the held-out pool; differing failures mark a real but flaky bug, which goes to
   the raid set; nothing failing rejects it as survived.

Generation also refuses a git work tree with uncommitted changes under the repo, so the commit recorded in
each entry reproduces the mutated source exactly; outside git the commit is null. Every admitted entry
carries its provenance — method, stack, source repo and commit, file and line, the mutation, the gate
outcome with failing and passing test names and the margin (failing ÷ total), the date, and the cost.

### The pool

`--pool <dir>` is required and never defaulted into the repo: a held-out set committed next to the agents
being scored is visible to them. It holds `heldout.json`, `raid.json` and `runs.json`, written through a
temp file and a rename, and the mutating commands (`generate`, `review`, `record`) take an exclusive
`pool.lock` while they work — a hard kill leaves that lock to remove by hand.

### Review and rotation

The first accepted tasks are reviewed by hand: `review` records `genuine` or `vacuous`, and `draw` refuses
— "pool not open" — until 20 carry a verdict. Once open, `draw` hands out reviewed-genuine tasks
oldest-first, `record` retires a task after three scoring decisions, and `inventory` shows counts by
method, stack, status and age plus the gate's aggregate pass rate and cost per accepted task.

### The raid set

Flaky mutants are real bugs that are unfit to score, so they are kept apart: never drawn from, never
sharing an id with the held-out pool, and carrying briefs that name the injected class of bug and where it
lives — ready for the red-team hunters.

### Measured so far

One-off measurement, 2026-09-24, `node scripts/bench/synth.mjs generate --repo <dir> --pool <dir>`: the
bench fixture admitted 3 of 3 candidates, `services/telemetry` 54 of 107 (53 survived, 0 syntax, 0 flaky) —
57/110 (52%) overall, $0 per accepted task.

### Not yet

An LM-rewrite method and PR-mirroring; stacks beyond Node (Rust, Go); wiring the raid set into the
red-team hunters' briefs (`crates/colonizer/src/redteam.rs`); and the human review of the first 20 accepted
tasks plus the colony trial that opens the pool to scoring.

## External suites (SWE-bench)

The bench scores tasks this harness wrote; `scripts/swebench.mjs` scores the colonies on work nobody here
chose — real SWE-bench bug reports with hidden tests and a hidden gold patch. Stages, in order: **Lite**
(300 Python tasks), **Verified** (500 human-checked tasks, the number worth trusting), **Multilingual**
(whose rows carry no `language` column, so per-language results group under `unknown` for now).

```sh
node scripts/swebench.mjs fetch princeton-nlp/SWE-bench_Lite --limit 5 --out lite.json
node scripts/swebench.mjs run lite.json --owner my-org --task-budget 2 --total-budget 20 --label lite-before
node scripts/swebench.mjs score swebench-lite-before.json --dataset princeton-nlp/SWE-bench_Lite
```

`fetch` also takes a local `.json`/`.jsonl` file, `--ids`, and `--split`/`--config`; the gold and test
patches stay in the instances file for scoring-side checks only — a colony never sees them. `run` needs
the bench's mothership and `gh` logged in to an account allowed to create **private** repositories: each
instance becomes `<owner>/swebench-<instance-id>`, a single-commit reconstruction of upstream at
`base_commit` — no history (so no future commit holding the fix), no remote, no eval tests — one colony
per instance in sequence, `--model-tier`/`--model`/`--subagent-model` riding through. It writes
`swebench-<label>.json` and `swebench-<label>.predictions.jsonl`; scratch repositories are left for
inspection (delete with `gh repo delete`).

The budget envelope is required: `run` refuses to start without `--task-budget` and `--total-budget`. A
task that cannot be paid for never starts (`skipped-budget`, reported separately, never a failure), a
colony is stopped via `POST /api/sessions/{id}/stop` once its live cost passes the task cap, and the run
stops cleanly when the next task would pass the total; `--timeout-min` (default 60) bounds it in time too.

`score` runs the official harness (`python -m swebench.harness.run_evaluation`) over the predictions —
Docker and `pip install swebench` required — or ingests a report with `--report`. It writes `resolved`
back into the run record and prints raw and clean resolved rates, per language, and cost against the
caps. **Clean** excludes flagged tasks — a patch editing any file the instance's `test_patch` touches
(`touches-eval-tests`: the colony wrote its own exam), or an empty patch; tasks the harness itself errors
on are unknown, not failures, and sit in neither rate.

### The controls, honestly

| Control | Enforced today |
| :--- | :--- |
| `single_commit_snapshot` | yes |
| `concealed_eval_artifacts` | yes — tests and gold patch never reach the scratch repo; scoring is the official harness |
| `no_network_answer_sources` | **no** — colonies have internet; nothing is fenced yet |
| `gold_sanity_gate` | **no** — issue #330 |
| `trajectory_monitor` | **no** — not implemented |

Until all five hold, every run is labeled **uncalibrated** — a signal to steer by, not a number to
publish, and not comparable to published SWE-bench results.
