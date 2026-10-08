# The bench

A change to an agent — its prompt, a module setting, which model a settler runs on — is either an
improvement or a regression, and reading one colony's chat won't tell you which. The bench is the fixed set
of tasks that change has to survive, scored the same way every time.

It pairs with `scripts/colony-report.mjs`, which says what happened *inside* the colonies (cost, tools,
questions, silences). The bench says whether the work was actually right. For spend alone,
`node scripts/colony-report.mjs --costs` reads the spend journal (`spend.jsonl`) and ranks colonies by
cost, the agent's own estimate beside what the gateway metered, with a harness × model table; the
window is the journal's last 30 days unless `--since`/`--days` move it (#549).

The colony report also says where a colony's tokens went. Each turn bills its whole spend to one of six
categories — `read`, `search`, `command_output`, `edit`, `reasoning`, `replay` — by the tools it called:
Read/NotebookRead are `read`; Grep, Glob, LS and WebSearch are `search`; Bash, BashOutput and KillShell are
`command_output`; Edit, Write, MultiEdit and NotebookEdit are `edit`; everything else (Task, Skill,
WebFetch, any `mcp__*` tool, unknown names) and a turn with no tool call at all is `reasoning`. The turn is
billed whole, to the most consequential thing it did, by precedence `edit` > `command_output` > `search` >
`read` > `reasoning` — a turn that read files and then edited them is all `edit`, and `tool_call`s a
subagent made count toward the same turn, since `turn_end` is the colony's own and its `model_usage`
covers the subagents' work too. Cache tokens (`cache_read` + `cache_write`) are always `replay`. The
split takes each `turn_end`'s cumulative `model_usage`, diffs it against the previous snapshot, and
floors the delta at zero, the same way the spend journal's `turn_deltas` does in
`crates/colonizer/src/spend.rs` — so a colony's six categories add up exactly to its recorded usage. A
`turn_end` without `model_usage` (it is optional) attributes nothing and leaves the baseline for the next
measured turn, rather than re-counting what came before it. They surface in the report's "Token
categories" table, in `--json` as `tokenCategories`/`tokenCategoriesByModel`, and per bench task as
`token_categories`.

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

- the colony finished within `--timeout`;
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
node scripts/bench.mjs rate  bench-before.json bench-after.json   # the resolved rate, split by harness · model
node scripts/bench.mjs jev  bench-before.json bench-after.json   # grade Jev compaction across the runs
node scripts/bench.mjs routing                          # is Jev routing act mode justified? (below)
node scripts/bench.mjs clean --repo owner/bench         # close the bench's pull requests and delete their branches
```

`run` talks to a mothership on `COLONIZER_URL` (default `http://127.0.0.1:7878`), launches one colony per
task, and answers the questions it asks by itself: it picks the option matching the task's `answer.prefer`,
else the first one, and records what it chose. It authenticates with `COLONIZER_API_TOKEN`, else the
`api-token` file in `COLONIZER_CONFIG_DIR` (default `~/.config/colonizer`). `--only a,b` runs a subset,
`--timeout` bounds a colony in seconds (default 1200), `--data <dir>` names the mothership's data
directory the colony report, trajectory monitor, `jev`'s and `routing`'s ledgers read (default `COLONIZER_DATA_DIR`,
else `~/.local/share/colonizer`), and `--heldout <dir>` scores the held-out suite below (`--max-gap`,
default 0.25, sets its threshold). Results go to `bench-<label>.json`.

This costs real model tokens and opens real pull requests on the scratch repository. It opens them nowhere
else.

## Reading a comparison

```
| Task | Harness · model | Result | Clean | Cost | Worked | Questions | Why it failed |
| ambiguous-rounding | claude-code · opus → claude-code · opus | pass → FAIL | clean → – | 0.31 → 0.28 (-0.03) | 94s → 71s | 1 → 0 | asked 0 questions, expected 1 |
```

One run of one task is one sample. A cost difference of a few cents is noise; a task that flips from pass to
fail, or a question that stops being asked, is not.

Each scored task also records the harness (`agent`), the model it ran on — a launch override, else what
boot's routing recorded, `–` when the colony stayed on its module's own model — with the routing `tier`
beside it, and `compare` shows the pair per row (issue #296). Read down that column across runs and it
answers which harness × model is cheapest — or fastest — for a task shape.

Every result also carries `clean`: after scoring, the [trajectory monitor](trajectory-monitor.md) audits the
colony's persisted event log for shortcut shapes — history mining, weakened tests, writes to what the scorer
executes, solution fetches, unflagged injections — and the comparison gains a Clean column (`clean`,
`HACKED`, or `–` when there was no log to audit) plus the run-level clean rate and the gap between resolved
and clean-resolved. A run that raises its pass rate while widening that gap bought its score; the gap is the
number to watch.

The [offline evolver](evolver.md) builds on these run files: it clusters diagnosed failures into classes,
turns one class into a prompt-only proposal, and retains the proposal only when a rerun of the same tasks
beats a baseline — judged with the same per-task honesty as `compare`, single regressions included.

## The resolved rate

`compare` reads down one column per task. `rate` reads across the whole run and answers the question
the issue asks: how much work actually landed, for which harness × model. It counts three stages
separately, because a pull request opened, checks green and merged are three different questions — a
colony can open a pull request it cannot finish, finish one it cannot get merged, and get merged on a
branch the repository's own CI never ran. Collapsing the three into one "resolved" number flatters
whichever model is quickest at producing a plausible diff, and hides the failure mode where work is
opened and then dropped.

```sh
node scripts/bench.mjs rate bench-before.json bench-after.json   # --data <dir> names a non-default mothership
node scripts/bench.mjs rate --json bench-before.json             # the same report as JSON, stable enough to diff in CI
```

It reads two sources and unions them: the `bench-<label>.json` files named on the command line, and
the real colony session store at `<data dir>/sessions.json` — the same resolution rule `jev`, `brief`,
`routing` and the colony report already use (`--data`, else `COLONIZER_DATA_DIR`, else
`~/.local/share/colonizer`). Every record is split by harness · model and counted per stage, each cell
counted over the records that stage could speak for and printed as `green/measured (rate)`:

```text
| Harness · model | Source | Colonies | PR opened | Tests green | Merged |
| --- | --- | --- | --- | --- | --- |
| claude-code · zai/glm-5.3-flash | bench | 24 | 21/24 (88%) | 14/24 (58%) | 0/24 (0%) |
| codex · gpt-5.1-codex | colonies | 12 | 10/12 (83%) | 9/12 (75%) | 0/12 (0%) |
| claude-code · zai/glm-5.3-flash | colonies | 9 | 9/9 (100%) | 7/9 (78%) | 4/9 (44%) |
| – | overall | 45 | 40/45 (89%) | 30/45 (67%) | 4/45 (9%) |
```

**Those numbers are illustrative, not a measurement of anything.** This repository carries no committed
bench run files and no committed colony store, so there is nothing real to publish; inventing a table
and calling it a result is exactly the failure the rest of this page refuses. The block above is the
verbatim output of `rateReport` and `formatRateReport` over synthetic records built to the same shape,
not a run of this repository's bench: it shows the *shape* `rate` prints — the columns, the `·`
separator, the group order (biggest sample first), the `overall` row that pools counts rather than
averaging per-group rates. Running the command against a real store replaces every figure in it, and
only that run's numbers should ever be quoted.

What the columns do and do not mean, stated plainly:

- `–` means **never measured**, never "failed". A stage that did not run is not a stage that failed;
  the same convention the `jev` and `brief` rates use. A `0` means measured, and nothing qualified.
- **A colony still in flight reads `–` in all three columns.** `queued`, `blocked`, `starting`,
  `running`, `waiting_for_answer`, `publishing` and `idle` have not reached the pull-request stage yet,
  so they drop out of every denominator — counting them would be measuring the clock, not the model.
  The same goes for a colony still `parked`, and for a status missing or unknown: unmeasured, never a
  zero. Only a status the store can leave a colony in when its run is over (`pr_opened`, `merged`,
  `closed`, `no_changes`, `stopped`, `failed`) says anything about any of the three stages.
- **Bench rows are measurable from the first stage.** A bench task ran to a scored result, so all of
  them count towards PR opened and only some opened one — a bench row can read a partial rate like
  `21/24 (88%)`. Colonies are measured from the moment they settle.
- **Tests green** for bench rows is the bench's own hidden check passing (`visible`), not CI. The
  bench scratch repository has no CI, so there is no CI result to report for it.
- **The bench never merges.** `clean` closes the bench's pull requests and deletes their branches, so
  Merged for bench rows reads `0` unless the merge loop got there first. That column is only
  meaningful for real colonies; a bench-only table's `0` says nothing about any model.
- **One colony is one sample.** The same caveat that governs `compare` above: a difference of a few
  colonies between two harness × model rows is noise until it repeats.

To reproduce a report you have already published, keep the run files and point `--data` at the same
mothership data directory the colonies ran on, then diff the `--json` output between revisions —
the JSON is stable and ordered, so a CI job can regenerate it and fail on a change nobody intended.

## Grading Jev compaction

When [`jev_compaction`](colonies.md#measuring-jev-compaction) is on, the mothership appends one `decision`
row per chunk each compaction pass kept or dropped, and one `reread` row each time the agent re-issues a
call a decision was about, to `<data dir>/jev_ladder.jsonl`. `jev` reads that ledger and grades the
colonies against each other:

```sh
node scripts/bench.mjs jev bench-before.json bench-after.json   # --data <dir> names a non-default mothership
node scripts/bench.mjs jev --json bench-before.json             # the same report as JSON
```

A colony is a session. Its run is the first run file on the command line whose results list that
`session_id` — the same files `compare` reads — and it carries that result's task, agent and model for
display. A session no given run names (chat colonies, run files left off the command line) grades under
`(no run)`, last.

| Run | Colony | Task | Harness · model | Decisions | Rereads | TP | FP | FN | TN | Precision | Recall |
| :--- | :--- | :--- | :--- | --: | --: | --: | --: | --: | --: | --: | --: |
| before | 7c1e2a91 | add-helper | claude-code · opus | 14 | 3 | 2 | 2 | 1 | 9 | 0.50 | 0.67 |
| before | total | | | 14 | 3 | 2 | 2 | 1 | 9 | 0.50 | 0.67 |
| (no run) | d4b8f102 | – | – · – | 4 | 1 | 0 | 0 | 1 | 3 | – | 0.00 |
| overall | | | | 18 | 4 | 2 | 2 | 2 | 12 | 0.50 | 0.50 |

A decision predicts a chunk was needed when its `keep_result` is at or above the threshold — default 0.5,
`--threshold` to move it, the same score the plugin itself keeps at — and a reread of that decision's tool
call is the ground truth. Precision is the share of predicted-needed chunks that really were re-issued;
recall is the share of re-issued chunks the pass predicted to keep. A zero denominator reads as `–`, not
0: undefined, not a bad score. The counts sit beside the rates because one colony in one run is a small
sample; a `total` row pools them, so ten colonies' one-decision runs begin to say something. A reread only
ever grades decisions from its own session.

## Grading Jev brief picks

When the shadow boot brief (issue #585) is on (`jev_brief_shadow`, see
[protocol.md §6.1c](protocol.md#61c-jev-second-opinion-shadow-mode)), the mothership appends one `pick` row
per colony boot — the mandatory notes, the offered candidates, what Jev picked and what it left — and one
`used` row each time the colony is later seen to read a watched note or use a watched skill pack, to
`<data dir>/brief_picks.jsonl`. `brief` reads that ledger and grades the picks against the uses:

```sh
node scripts/bench.mjs brief bench-before.json bench-after.json   # --data <dir> names a non-default mothership
node scripts/bench.mjs brief --json bench-before.json             # the same report as JSON
```

The universe is the offered candidates. A candidate Jev picked that the colony was seen to use is a true
positive, a picked one it never used a false positive, and a used one it did not pick a false negative.
Mandatory notes (the `house-rule` and `security` tags, always loaded and never offered) are excluded, as
are uses of items that were not on the board. Precision and recall are over those counts, with a zero
denominator reading `–`: undefined, not a bad score. The table carries the counts beside the rates, the
per-colony means of the candidate, pick and mandatory counts, and `total` and `overall` rows that pool the
counts and recompute the rates from them. A colony booted twice is judged on its last boot. Shadow picks
say what Jev *would* have loaded, so the bench run both ways is the real test: `run --label plain` with
`jev_brief_shadow` off, `run --label brief` with it on, then `compare bench-plain.json bench-brief.json`.

## Judging Jev routing

The mothership appends a `decision` row to `<data dir>/routing.jsonl` for every routed boot — the rule's
tier, the `source` that won, and Jev's second opinion when one was asked
([protocol.md §6.1c](protocol.md#61c-jev-second-opinion-shadow-mode)) — and an `actual` row with what the
colony really spent when it ends. `routing` joins that ledger to each colony's outcome in `sessions.json`
and asks whether `jev_routing_act` should be on:

```sh
node scripts/bench.mjs routing                    # --data <dir> names a non-default mothership
node scripts/bench.mjs routing --threshold 0.7    # the act confidence to judge at (default 0.8)
node scripts/bench.mjs routing --json             # the same report as JSON
```

It reports how often Jev agrees with the rule (overall and per rule tier), its mean and median confidence
when it agrees and when it does not, and how many colonies `act` at the threshold would have put on another
tier — never under an operator override or with routing off, and never below the decision's `floor`. Each
disagreement gets a row: the rule's tier, Jev's, the confidence, the tier that ran, the tier act would run,
the outcome (`merged`; `pr-open`; `failed`, which includes a pull request closed unmerged; `other` for no
changes, stopped or parked; `pending` while it runs) and the actual cost. Rows from before the ledger
recorded `jev_agrees` have it recomputed as `jev.tier == rule`; a colony booted twice is judged on its last
decision.

The evidence is the confident disagreements judged in shadow: the rule's tier ran, the colony has an
outcome, and act would have changed the tier. They split by direction, and the verdict reads **justified**
only when

- there are at least 20 of them, and
- every direction with at least 5 says the rule was wrong: where Jev would go **lower**, the rule's tier
  merged at least 90% of the time (the task was easy, so the cheaper tier was likely enough); where Jev
  would go **higher**, the rule's tier failed at least 15 points more often than the baseline of every
  shadow colony with an outcome. At least one direction must have the 5.

Anything less reads "not yet justified" with the reason. Shadow outcomes only say what the rule's tier did,
never what Jev's would have done, so the real test is the bench run both ways: `run --label rule` with
`jev_routing_act` off, `run --label jev` with it on, then `compare bench-rule.json bench-jev.json` for the
cost, working time, clean-resolved rate and gap per run.

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
history (a missing manifest starts at version 1). A family has one active companion at a time: adding a
second is refused until the first retires. A task's family is its `family` field in `tasks.json`, else
its id. `run --heldout` resolves one active companion per family
**before any colony launches** — a family without one fails the run before it spends — then scores every
task that opened a pull request against its family's companion on a fresh clone of its own, never the
visible check's checkout. The scorer's git carries the same `-c` guards the mothership puts on every
host-side git (`HOST_GIT_NO_EXEC` in `crates/colonizer/src/github.rs`), so a branch's hooks, fsmonitor,
gc and maintenance never execute here; and only the pass bit, read from the exit code, and the companion's
id are kept — the output is dropped, so held-out material never lands in anything the bench writes.

The companion lands where its stack expects it, the stack read from the fresh clone's root marker:
`heldout-check.test.mjs` at the clone root for Node, `tests/heldout_check.rs` for Rust (an integration
test over the crate's public API) and `heldout_check_test.go` at the clone root for Go (declaring the
root package) — the full table is in [Synthetic tasks → Stacks](#stacks). `heldout add` keeps the check
file's extension, so a `.rs` or `.go` companion stays one wherever it is scored.

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
`scoring: { visible_ms, heldout_ms }` and the run summary sums `scoring_ms`, which `run` journals into
the mothership's spend.jsonl once it finishes — one `scoring` row under the `bench` org, carrying the
run's total ([protocol.md, §6.8 Spend](protocol.md#68-spend-per-org-and-per-day)), so the spend
history shows it beside the colonies' spend. A failed write only warns: a lost row is not a failed
run.

## Synthetic tasks

Four hand-written tasks is a thin sample. `scripts/bench/synth.mjs` grows the set the SWE-smith way: inject
a bug into real source, keep only the mutants that break the repository's own tests, and admit them through
a gate. Stage one is procedural — no model in the loop, so an accepted task costs $0 to make — and works on
three stacks: Node, Rust and Go.

### Stacks

The stack is read from the repository's root marker, most specific first: `Cargo.toml` is Rust, `go.mod` is
Go, `package.json` is Node — so a napi-style Rust crate that also ships a `package.json` generates as Rust,
its tests being cargo's. `generate --stack node|rust|go` overrides the detection. What each stack means,
from `scripts/bench/stacks.mjs`:

| | Node | Rust | Go |
| :--- | :--- | :--- | :--- |
| Source files | `*.js`/`*.mjs`/`*.cjs` outside `tests?/` | `*.rs` outside `tests/` and `benches/` | `*.go` that is not `*_test.go` |
| Test files | `*.test.*`, `*.spec.*`, files under `tests?/` | files under `tests/` or `benches/` | `*_test.go` |
| Left out of a copy | `node_modules`, `.git` | `target`, `.git` | `.git` |
| Syntax gate | `node --check <file>` | `cargo test --no-run` | `go test -count=1 -run '^$' ./...` |
| Tests, run twice | `node --test --test-reporter=tap` | `cargo test` | `go test -count=1 -json ./...` |
| Held-out companion | `heldout-check.test.mjs`, `node --test` | `tests/heldout_check.rs`, `cargo test --test heldout_check` | `heldout_check_test.go`, `go test -count=1 .` |

The mutation scanner lexes per language. Rust and Go swap `==`↔`!=` where JavaScript swaps `===`↔`!==`;
the rest of the table — `&&`↔`||`, `<`↔`<=`, `>`↔`>=`, `+`↔`-`, `*`↔`/`, an integer n → n+1,
`true`↔`false` — is shared. Each lexer skips its language's dead zones: strings and comments (which nest
in Rust and do not in Go), Rust's raw strings (`r"…"`, `r#"…"#`) and lifetimes (`'a` is a lifetime, not an
unterminated char), Go's runes and backtick strings, and regex literals, which only JavaScript has. A Rust
source file stops scanning at a test-only `#[cfg(…)]` — bare or wrapped in `all()`/`any()`: its inline
test module is part of the gate, not of the mutation surface. Generic brackets around a lifetime (`<'a>`)
do yield sites, but the mutants do not compile anywhere, so the gate discards them.

### What the generator does

A token-level scanner — deliberately not an AST — walks the detected stack's source, skipping the dead
zones listed above (a JavaScript template literal keeps its interpolations; a `/` after a keyword such as
`return` or `typeof` opens a regex, where division is impossible). It yields mutation sites: the operator
swaps and literal nudges of the stack's table. Where a construct is ambiguous — `**`, `++`, `=>`, a
generator's `*`, a number that is not a plain integer — the site is skipped rather than risk nonsense. The
issue text is templated from the failing test names and states the symptom only: which tests fail, and
that the fix belongs in the source, not the tests. It never names the operator or the line.

### The gate

Each candidate is checked before it is trusted, with the counts landing in `runs.json` so the pass rate is
measured, not asserted:

1. the reference checkout must be green, or generation aborts;
2. the stack's syntax gate (the table above) — a mutant that does not compile is rejected as vacuous
   breakage;
3. the bugged checkout runs its tests twice (go with `-count=1`, so its result cache cannot replay the
   first run as the second): the same tests failing both times, compared name by name, admits the
   instance to the held-out pool; differing failures mark a real but flaky bug, which goes to the raid
   set; nothing failing rejects it as survived.

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
lives — ready for the red-team hunters, who now get them. A mothership pointed at the pool with
`COLONIZER_BENCH_POOL=<dir>` reads `raid.json` at each red-team launch and deals the entries recorded against
the raided repository out round-robin (entry i to hunter i, then every swarm-size-th entry after it, at most
20 per brief), so no lead is ever handed to two hunters; a raid set longer than the swarm can carry at the
cap waits for a later run. The hunter's brief gains a paragraph quoting each entry's brief with its
`file:line`, injected class and commit, and saying to chase these first even where they fall outside the
focus assignment. Entries whose `source.repo` names another repository are skipped (the match is ASCII
case-insensitive), and a missing or malformed `raid.json` only logs — the swarm launches with ordinary
briefs. The `source.repo` label is `owner/name` from the repository's `origin` remote; a checkout with no
remote is labelled by its path, and no run's repo slug will match it.

### Measured so far

One-off measurement, 2026-09-24, `node scripts/bench/synth.mjs generate --repo <dir> --pool <dir>`: the
bench fixture admitted 3 of 3 candidates, `services/telemetry` 54 of 107 (53 survived, 0 syntax, 0 flaky) —
57/110 (52%) overall, $0 per accepted task.

### Not yet

An LM-rewrite method and PR-mirroring; and the human review of the first 20 accepted tasks plus the
colony trial that opens the pool to scoring.

## Measuring a skill pack

A skill pack is the hardest kind of change to grade. It is prompt-only, so nothing breaks, and the
effect it claims — less code written — is the same effect that appears when a requirement got cut.
So a pack is measured the same way any other change is: two labelled runs of the same task set,
differing in exactly one thing, then `compare`.

**The arms.** Same `scripts/bench.mjs` task set, same repository, same model and routing, run twice
under two labels:

```sh
# baseline: the mothership's claude-code `plugins` skillset setting as it is today (`archify`)
node scripts/bench.mjs run --repo owner/bench --label without

# then switch ponytail on in Settings -> Skillsets and run the identical set
node scripts/bench.mjs run --repo owner/bench --label with

node scripts/bench.mjs compare bench-without.json bench-with.json
```

Only the `plugins` setting differs between the arms. Everything else — the task set, the
per-task timeouts, the scorer, the held-out suite — is held fixed, because a second moving part is
a second explanation for whatever the comparison shows.

**What the harness produces.** Per task, the run file carries `passed` and `failures`, `visible` and
`heldout` check results, `cost_usd` and `routed_cost_usd` (and their `total_cost_usd`), the six
`token_categories` (`read`, `search`, `command_output`, `edit`, `reasoning`, `replay`, described at
the top of this page), `working_ms` and `wall_ms`, `turns`, `tool_calls`, `questions`,
`watchdog_nudges`, `subagents`, and `changed` — the **file names** the branch touched, with
`outside` naming any the task's `changed_within` list did not allow. Per run it sums the costs and
times and gives the pass rate, `clean_rate` and the `gap` between resolved and clean-resolved, plus
the held-out suite's own pass rate and its `compare` line. `clean` comes from the
[trajectory monitor](trajectory-monitor.md); `--heldout <dir>` adds the companion checks, which are
the control against a pack that learned the visible tests.

**What it does not produce.** Two of the numbers the issue asks for are not in the run file, and
pretending otherwise would be the easiest way to turn this into marketing:

- **Diff LOC.** The bench records *changed file names*, never lines added or removed. Count them
  separately — `git diff --numstat origin/<base>...<branch>` on each bench pull request, or
  `git diff --shortstat` summed over the run's branches — and keep that alongside the run file.
- **Reviewer-flagged regressions.** Nothing in the harness reads a review. The signal that a pack
  over-cut a requirement lives in the pull requests themselves: flag them when a review turns up a
  dropped validation, a removed error path, a security or accessibility cut, or an unstated scope
  reduction. A pack that raises its pass rate while its review flags go up has bought the score.

So the honest grade for a pack is three columns side by side: what the bench measured, what the
branch diffs measured, and what reviewers flagged.

**The confound, named.** The `plugins` setting is install-wide: it is one value for the whole
mothership, not per colony. The two arms are therefore **serial** — they cannot overlap, and any
colony that runs while an arm is in flight is on whatever configuration the setting currently
holds. Run the bench on a mothership nothing else is using, close the arms back to back, and record
the window; a comparison taken across a setting that moved underneath it is not a comparison.

**The sample-size caveat, which the rest of this page already insists on.** Four hand-written tasks
at one run each is **one sample per task**. A pass-rate move on four tasks is noise until it
repeats, and so is a few cents of cost: report medians and spread across repeated runs, not a
single figure. The one result worth acting on is a task that flips **pass → fail** — especially
`readme-typo`, the scope-discipline task, or any held-out family whose `gap` widens. Not a slightly
cheaper run.

### Not yet

**ponytail: not measured (as of 2026-10-05).** No bench run has been performed for the vendored
[ponytail](skill-packs.md#ponytail) pack, and this page reports no result for it — none exists. The
environment that would run it has no mothership to talk to, no `gh` to open the bench repository's
pull requests, and no model credentials, so a run could not have produced a real number and one was
not invented in place of it.

For a maintainer who has all three:

```sh
node scripts/bench.mjs seed --repo owner/bench
# arm 1 — the install's current `plugins` setting, untouched
node scripts/bench.mjs run --repo owner/bench --label without
# switch ponytail on in Settings -> Skillsets (or set `plugins` to `archify,ponytail`), then:
node scripts/bench.mjs run --repo owner/bench --label with
node scripts/bench.mjs compare bench-without.json bench-with.json
```

Then repeat both arms a few times, collect diff LOC from the two runs' pull requests, and read the
reviews on them for over-cutting, before writing any result here. Until then the pack stays opt-in.

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

`fetch` also takes a local `.json`/`.jsonl` file, `--ids`, `--offset`, and `--split`/`--config`; the gold and test
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
| `gold_sanity_gate` | **no** — #330 added the gate for the bench's own tasks (the reference solutions above), but `swebench.mjs` does not apply one |
| `trajectory_monitor` | **no** — the [trajectory monitor](trajectory-monitor.md) scores the bench's own runs, but `swebench.mjs` does not run it |

Until all five hold, every run is labeled **uncalibrated** — a signal to steer by, not a number to
publish, and not comparable to published SWE-bench results.
